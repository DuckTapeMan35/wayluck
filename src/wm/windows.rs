//! Windows, as the window manager sees them: told of once, with what their
//! clients set up and change (names, parent, size limits), and what they
//! ask for (focus, fullscreen). It may ask a window to close.

use super::protocol::wayluck_wm_window_v1::{self, WayluckWmWindowV1};
use crate::{
    Wayluck,
    windows::{self, WindowId},
};
use smithay::{
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, Resource, backend::ClientId,
    },
    wayland::{compositor::with_states, seat::WaylandFocus, shell::xdg::SurfaceCachedState},
};
use std::cell::Cell;

/// On a window's surface: its client asked to be fullscreen before its
/// window manager knew the window.
#[derive(Default)]
struct EarlyFullscreen(Cell<bool>);

/// On a window's surface: the size limits its window manager last heard.
#[derive(Default)]
struct SentHints(Cell<Option<[i32; 4]>>);

impl Wayluck {
    /// Tell the window manager (if any) about a window, once, with what its
    /// client set up so far: its names, parent, size limits, and whether it
    /// asked to be fullscreen.
    pub fn wm_window_added(&mut self, id: WindowId) {
        if self.wm.windows.contains_key(&id) {
            return;
        }
        let Some((manager, client)) = self.wm.manager_and_client() else {
            return;
        };
        let Ok(resource) = client.create_resource::<WayluckWmWindowV1, WindowId, Wayluck>(
            &self.display_handle,
            manager.version(),
            id,
        ) else {
            return;
        };
        manager.window(&resource, id.into());
        self.wm.windows.insert(id, resource);

        self.wm_window_title(id);
        self.wm_window_app_id(id);
        self.wm_window_parent(id);
        self.wm_size_hints(id);
        let toplevel = self.windows.get(id).and_then(|w| w.toplevel());
        let early = toplevel.is_some_and(|t| {
            with_states(t.wl_surface(), |s| {
                s.data_map
                    .get::<EarlyFullscreen>()
                    .is_some_and(|f| f.0.get())
            })
        });
        if early {
            self.wm_fullscreen_request(id, true);
        }
    }

    /// A window's parent changed: the window manager hears the new one
    /// (none, if the parent isn't a window it knows).
    pub fn wm_window_parent(&self, id: WindowId) {
        let Some(resource) = self.wm.windows.get(&id) else {
            return;
        };
        let parent = (self.windows.parent(id)).and_then(|parent| self.wm.windows.get(&parent));
        resource.parent(parent);
    }

    /// A window's size limits, if they changed since the window manager
    /// last heard them.
    pub fn wm_size_hints(&self, id: WindowId) {
        let Some(resource) = self.wm.windows.get(&id) else {
            return;
        };
        let Some(window) = self.windows.get(id) else {
            return;
        };
        let Some(surface) = window.wl_surface() else {
            return;
        };
        // An X window's are its WM_NORMAL_HINTS; a Wayland window's come
        // with a commit.
        let (min, max) = match window.x11_surface() {
            Some(x11) => (
                x11.min_size().unwrap_or_default(),
                x11.max_size().unwrap_or_default(),
            ),
            None => with_states(&surface, |s| {
                let limits = *s.cached_state.get::<SurfaceCachedState>().current();
                (limits.min_size, limits.max_size)
            }),
        };
        let hints = [min.w, min.h, max.w, max.h];
        with_states(&surface, |s| {
            s.data_map.insert_if_missing(SentHints::default);
            let sent = &s.data_map.get::<SentHints>().unwrap().0;
            if sent.replace(Some(hints)) != Some(hints) {
                resource.size_hints(hints[0], hints[1], hints[2], hints[3]);
            }
        });
    }

    /// A window's title changed: the window manager hears the new one.
    pub fn wm_window_title(&mut self, id: WindowId) {
        if let Some((resource, (Some(title), _))) = self.wm_window_names(id) {
            resource.title(title);
        }
    }

    /// A window's app id changed: the window manager hears the new one.
    pub fn wm_window_app_id(&mut self, id: WindowId) {
        if let Some((resource, (_, Some(app_id)))) = self.wm_window_names(id) {
            resource.app_id(app_id);
        }
    }

    /// The window manager's object for a window, and the window's names.
    fn wm_window_names(&self, id: WindowId) -> Option<(&WayluckWmWindowV1, windows::Names)> {
        let resource = self.wm.windows.get(&id)?;
        Some((resource, windows::names(self.windows.get(id)?)))
    }

    /// The user clicked a window: the window manager hears so, and decides
    /// about focus.
    pub fn wm_focus_request(&self, id: WindowId) {
        if let Some(resource) = self.wm.windows.get(&id) {
            resource.focus_request();
        }
    }

    /// A window's client asked to be fullscreen, or to stop: the window
    /// manager hears so, and decides. false: there's none to ask.
    pub fn wm_fullscreen_request(&self, id: WindowId, on: bool) -> bool {
        let Some(resource) = self.wm.windows.get(&id) else {
            // Before its first commit: the window manager hears it with the window.
            let toplevel = self.windows.get(id).and_then(|w| w.toplevel());
            let (Some(toplevel), Some(_)) = (toplevel, &self.wm.manager) else {
                return false;
            };
            with_states(toplevel.wl_surface(), |s| {
                s.data_map.insert_if_missing(EarlyFullscreen::default);
                s.data_map.get::<EarlyFullscreen>().unwrap().0.set(on);
            });
            return true;
        };
        if on {
            resource.fullscreen_request();
        } else {
            resource.unfullscreen_request();
        }
        true
    }

    /// A window is gone: the window manager hears so, and should destroy it.
    pub fn wm_window_closed(&mut self, id: WindowId) {
        if let Some(resource) = self.wm.windows.remove(&id) {
            resource.closed();
        }
    }
}

impl Dispatch<WayluckWmWindowV1, WindowId> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WayluckWmWindowV1,
        request: wayluck_wm_window_v1::Request,
        id: &WindowId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            // Asking the client: it may close, or not.
            wayluck_wm_window_v1::Request::Close => {
                let Some(window) = state.windows.get(*id) else {
                    return;
                };
                if let Some(t) = window.toplevel() {
                    t.send_close();
                } else if let Some(x11) = window.x11_surface()
                    && let Err(e) = x11.close()
                {
                    tracing::debug!("X: can't close window {}: {e}", x11.window_id());
                }
            }
            // The manager's error, if the curve isn't one.
            wayluck_wm_window_v1::Request::SetAnimation { duration, curve } => {
                let Some(manager) = state.wm.manager.clone() else {
                    return;
                };
                if let Some(setting) = super::animation(&manager, duration, &curve) {
                    state.windows.set_animation(Some(*id), Some(setting));
                }
            }
            wayluck_wm_window_v1::Request::UnsetAnimation => {
                state.windows.set_animation(Some(*id), None);
            }
            wayluck_wm_window_v1::Request::Destroy => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _resource: &WayluckWmWindowV1,
        id: &WindowId,
    ) {
        state.wm.windows.remove(id);
    }
}
