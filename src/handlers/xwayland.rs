//! X clients: Xwayland, and wayluck as its X window manager (smithay's
//! X11Wm). For now it only says what happens: no X window is shown.

use crate::Wayluck;
use smithay::{
    delegate_xwayland_shell,
    reexports::calloop::LoopHandle,
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

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::info!(
            "X: window {} ({:?}, {:?}) asks to be shown",
            window.window_id(),
            window.title(),
            window.class(),
        );
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!("X: override-redirect window {} shown", window.window_id());
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!("X: window {} hidden", window.window_id());
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::debug!("X: window {} gone", window.window_id());
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
