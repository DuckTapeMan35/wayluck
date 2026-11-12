//! zwlr-foreign-toplevel-management: windows for taskbars with what they show:
//! titles, app ids, the outputs they're on, their parents, and their states.
//! Active is the focused one; fullscreen, as the window manager made it;
//! maximized, covering all its output leaves windows (as a window alone on a workspace does).
//! What a taskbar asks for (activating, fullscreen) the window manager hears, and
//! decides; closing asks the window's client, as the window manager would.
//! The same windows are listed as ext-foreign-toplevel-list lists them.

use crate::{
    Wayluck,
    windows::{self, WindowId},
};
use smithay::{
    output::Output,
    reexports::{
        wayland_protocols_wlr::foreign_toplevel::v1::server::{
            zwlr_foreign_toplevel_handle_v1::{self, State, ZwlrForeignToplevelHandleV1},
            zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::ClientId, protocol::wl_output::WlOutput,
        },
    },
};

/// Every taskbar's view of the windows.
#[derive(Default)]
pub struct Taskbars {
    bound: Vec<Bound>,
}

/// One taskbar's view: what it's been told.
struct Bound {
    manager: ZwlrForeignToplevelManagerV1,
    windows: Vec<Told>,
}

/// A window, and what the taskbar was last told of it.
struct Told {
    id: WindowId,
    handle: ZwlrForeignToplevelHandleV1,
    /// Told nothing yet: told everything.
    fresh: bool,
    names: windows::Names,
    states: Vec<State>,
    outputs: Vec<WlOutput>,
    parent: Option<WindowId>,
}

impl Wayluck {
    pub fn taskbars_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, ZwlrForeignToplevelManagerV1, _>(3, ());
    }

    /// Windows, or what's shown of them, may have changed: every taskbar
    /// hears what's new.
    pub fn taskbars_changed(&mut self) {
        let mut bound = std::mem::take(&mut self.taskbars.bound);
        for b in &mut bound {
            self.tell_taskbar(b);
        }
        self.taskbars.bound = bound;
    }

    fn tell_taskbar(&self, b: &mut Bound) {
        let Some(client) = b.manager.client() else {
            return;
        };
        let listed: Vec<WindowId> = self.listed().collect();
        // Every window's handle first: a parent's may be needed for another.
        for &id in &listed {
            if b.windows.iter().any(|t| t.id == id) {
                continue;
            }
            let version = b.manager.version();
            let made = client.create_resource::<ZwlrForeignToplevelHandleV1, WindowId, Wayluck>(
                &self.display_handle,
                version,
                id,
            );
            let Ok(handle) = made else {
                continue;
            };
            b.manager.toplevel(&handle);
            b.windows.push(Told {
                id,
                handle,
                fresh: true,
                names: (None, None),
                states: Vec::new(),
                outputs: Vec::new(),
                parent: None,
            });
        }
        let handles: Vec<(WindowId, ZwlrForeignToplevelHandleV1)> = (b.windows.iter())
            .map(|t| (t.id, t.handle.clone()))
            .collect();
        for t in &mut b.windows {
            let Some(window) = self.windows.get(t.id) else {
                continue;
            };
            let mut told = false;
            let names = windows::names(window);
            if t.fresh || t.names.0 != names.0 {
                t.handle.title(names.0.clone().unwrap_or_default());
                told = true;
            }
            if t.fresh || t.names.1 != names.1 {
                t.handle.app_id(names.1.clone().unwrap_or_default());
                told = true;
            }
            t.names = names;
            let outputs: Vec<WlOutput> = (self.outputs_of(t.id).iter())
                .flat_map(|o| o.client_outputs(&client))
                .collect();
            for output in outputs.iter().filter(|o| !t.outputs.contains(o)) {
                t.handle.output_enter(output);
                told = true;
            }
            for output in t.outputs.iter().filter(|o| !outputs.contains(o)) {
                if output.is_alive() {
                    t.handle.output_leave(output);
                    told = true;
                }
            }
            t.outputs = outputs;
            let states = self.taskbar_states(t.id, t.handle.version());
            if t.fresh || t.states != states {
                let bytes = states
                    .iter()
                    .flat_map(|s| u32::from(*s).to_ne_bytes())
                    .collect();
                t.handle.state(bytes);
                t.states = states;
                told = true;
            }
            let parent = self.windows.parent(t.id);
            // Since version 3.
            if t.handle.version() >= 3 && (t.fresh || t.parent != parent) {
                let handle = parent.and_then(|p| handles.iter().find(|(id, _)| *id == p));
                t.handle.parent(handle.map(|(_, h)| h));
                told = true;
            }
            t.parent = parent;
            t.fresh = false;
            if told {
                t.handle.done();
            }
        }
        // Windows gone: closed.
        b.windows.retain(|t| {
            if listed.contains(&t.id) {
                return true;
            }
            t.handle.closed();
            false
        });
    }

    /// What a taskbar is told window id is.
    fn taskbar_states(&self, id: WindowId, version: u32) -> Vec<State> {
        let mut states = Vec::new();
        if self.windows.focused == Some(id) {
            states.push(State::Activated);
        }
        if self.windows.is_fullscreen(id) {
            // Since version 2.
            if version >= 2 {
                states.push(State::Fullscreen);
            }
        } else if self.covers_usable(id) {
            states.push(State::Maximized);
        }
        states
    }

    /// Does window id cover all its output leaves windows?
    fn covers_usable(&self, id: WindowId) -> bool {
        let Some(window) = self.windows.get(id) else {
            return false;
        };
        let Some(rect) = self.space.element_geometry(window) else {
            return false;
        };
        let outputs = self.outputs_of(id);
        let usable = outputs.first().and_then(|o| self.usable(o));
        usable.is_some_and(|usable| rect.contains_rect(usable))
    }

    /// The monitors window id is shown on: where it's placed now (not as of
    /// the last frame drawn).
    fn outputs_of(&self, id: WindowId) -> Vec<Output> {
        let window = self.windows.get(id);
        let Some(rect) = window.and_then(|w| self.space.element_geometry(w)) else {
            return Vec::new();
        };
        (self.space.outputs())
            .filter(|o| !self.is_virtual(o))
            .filter(|o| {
                self.space
                    .output_geometry(o)
                    .is_some_and(|g| g.overlaps(rect))
            })
            .cloned()
            .collect()
    }
}

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for Wayluck {
    /// A new taskbar: told of every window at once.
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        let mut bound = Bound {
            manager,
            windows: Vec::new(),
        };
        state.tell_taskbar(&mut bound);
        state.taskbars.bound.push(bound);
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ZwlrForeignToplevelManagerV1,
        request: zwlr_foreign_toplevel_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Request::Stop = request {
            state.taskbars.bound.retain(|b| &b.manager != manager);
            manager.finished();
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        manager: &ZwlrForeignToplevelManagerV1,
        _: &(),
    ) {
        state.taskbars.bound.retain(|b| &b.manager != manager);
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, WindowId> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        _handle: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        id: &WindowId,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Request;
        match request {
            Request::Activate { .. } => state.wm_activate_request(*id),
            Request::SetFullscreen { .. } => {
                state.wm_fullscreen_request(*id, true);
            }
            Request::UnsetFullscreen => {
                state.wm_fullscreen_request(*id, false);
            }
            // Asking the client, as the window manager would: it may close.
            Request::Close => state.close_window(*id),
            // The window manager has no say asked of it for these (yet).
            _ => {}
        }
    }
}
