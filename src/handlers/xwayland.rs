//! X clients: Xwayland, and wayluck as its X window manager (smithay's
//! X11Wm). An X window it shows is a window like any other:
//! for now, only when there's no window manager.

use crate::Wayluck;
use smithay::{
    delegate_xwayland_shell,
    desktop::Window,
    reexports::{calloop::LoopHandle, wayland_server::protocol::wl_surface::WlSurface},
    utils::{Logical, Rectangle},
    wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    xwayland::{
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
        xwm::{Reorder, ResizeEdge, X11Window, XwmId},
    },
};
use std::process::Stdio;

impl Wayluck {
    /// Start Xwayland (on its own display number), and be its window
    /// manager once it's ready. Programs started from now on hear its
    /// display (DISPLAY, session_env).
    pub fn start_xwayland(&mut self, handle: &LoopHandle<'static, Wayluck>) {
        let spawned = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            Stdio::null(),
            Stdio::inherit(),
            |_| (),
        );
        let (xwayland, client) = match spawned {
            Ok(spawned) => spawned,
            Err(e) => return tracing::warn!("no X clients: can't start Xwayland: {e}"),
        };
        let number = xwayland.display_number();
        let wm_handle = handle.clone();
        let source = handle.insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready { x11_socket, .. } => {
                match X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone()) {
                    Ok(wm) => {
                        tracing::info!("Xwayland ready on :{number}");
                        state.xwm = Some(wm);
                    }
                    Err(e) => tracing::warn!("can't be Xwayland's window manager: {e}"),
                }
            }
            XWaylandEvent::Error => tracing::warn!("Xwayland exited while starting"),
        });
        if let Err(e) = source {
            return tracing::warn!("no X clients: {e}");
        }
        self.x_display = Some(format!(":{number}").into());
    }
}

impl XWaylandShellHandler for Wayluck {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    /// Xwayland made the surface a shown X window is drawn on: from now on
    /// it's a window like the others (until a window manager can place X
    /// windows, only without one), cascading. Not before: clicks and keys
    /// reach it through its surface.
    fn surface_associated(&mut self, _xwm: XwmId, _surface: WlSurface, window: X11Surface) {
        let managed = !window.is_override_redirect() && window.is_mapped();
        if !managed || self.wm.manager.is_some() || self.windows.find_x11(&window).is_some() {
            return;
        }
        let id = self.add_window(Window::new_x11_window(window.clone()));
        tracing::info!(
            "X: window {} is {id} ({:?})",
            window.window_id(),
            window.title()
        );
        // Clients listing windows hear of it (toplevel_list.rs).
        self.list_window(id);
    }
}

delegate_xwayland_shell!(Wayluck);

impl XwmHandler for Wayluck {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        // Its events come only once it's started (start_xwayland).
        self.xwm
            .as_mut()
            .expect("X window manager events before it started")
    }

    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!("X: new window {}", window.window_id());
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!("X: new override-redirect window {}", window.window_id());
    }

    /// An X client wants its window shown: mapped, Xwayland makes it a
    /// surface (surface_associated, above). Until a window manager can
    /// place X windows, only without one.
    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if self.wm.manager.is_some() {
            return tracing::info!("X: window {} not shown yet", window.window_id());
        }
        if let Err(e) = window.set_mapped(true) {
            tracing::warn!("X: can't show window {}: {e}", window.window_id());
        }
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!("X: override-redirect window {} shown", window.window_id());
    }

    /// Hidden or gone, an X window is closed: shown again, it's new.
    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.windows.find_x11(&window) {
            tracing::info!("X: window {} ({id}) hidden", window.window_id());
            self.remove_window(id);
        }
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.windows.find_x11(&window) {
            tracing::info!("X: window {} ({id}) gone", window.window_id());
            self.remove_window(id);
        }
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        tracing::debug!(
            "X: window {} asks for {x:?},{y:?} {w:?}x{h:?}",
            window.window_id()
        );
        // Its size it may pick (as a Wayland client does); where it is, it
        // may not, once it's shown. X needs an answer either way.
        let mut rect = window.geometry();
        if let Some(w) = w {
            rect.size.w = w as i32;
        }
        if let Some(h) = h {
            rect.size.h = h as i32;
        }
        if self.windows.find_x11(&window).is_none() {
            rect.loc.x = x.unwrap_or(rect.loc.x);
            rect.loc.y = y.unwrap_or(rect.loc.y);
        }
        if let Err(e) = window.configure(rect) {
            tracing::debug!("X: can't configure window {}: {e}", window.window_id());
        }
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<X11Window>,
    ) {
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _edge: ResizeEdge,
    ) {
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}
}
