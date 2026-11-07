//! X clients: Xwayland, and wayluck as its X window manager (smithay's
//! X11Wm). What X asks of its window manager comes here first, and each
//! kind of request goes to the module that handles it: X windows (shown,
//! placed, closed like any other window), X's menus and tooltips, and the
//! clipboard between X and Wayland.

mod menus;
mod selection;
mod windows;

pub use selection::WaylandSelections;

use crate::{Wayluck, input::KeyboardFocus};
use smithay::{
    reexports::{calloop::LoopHandle, wayland_server::protocol::wl_surface::WlSurface},
    utils::{Logical, Rectangle},
    wayland::{
        selection::SelectionTarget,
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
        xwm::{Reorder, ResizeEdge, WmWindowProperty, X11Window, XwmId},
    },
};
use std::{os::fd::OwnedFd, process::Stdio};

impl Wayluck {
    /// Start Xwayland (on its own display number), and be its window
    /// manager once it's ready. Programs started from now on hear its
    /// display (DISPLAY, session_env).
    pub fn start_xwayland(&mut self, handle: &LoopHandle<'static, Wayluck>) {
        let spawned = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            std::iter::empty::<String>(),
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
                let dh = state.display_handle.clone();
                match X11Wm::start_wm(wm_handle.clone(), &dh, x11_socket, client.clone()) {
                    Ok(wm) => {
                        tracing::info!("Xwayland ready on :{number}");
                        state.xwm = Some(wm);
                        selection::ready(state);
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

    /// Does an X window have the keyboard (not a Wayland window, a bar, or
    /// nothing)?
    pub fn x_has_keyboard(&self) -> bool {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        matches!(focus, Some(KeyboardFocus::X11(..)))
    }
}

impl XWaylandShellHandler for Wayluck {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    fn surface_associated(&mut self, _xwm: XwmId, _surface: WlSurface, window: X11Surface) {
        windows::associated(self, &window);
    }
}

impl XwmHandler for Wayluck {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        // Its events come only once it's started.
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
        windows::map_request(&window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        menus::mapped(self, window);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        menus::gone(self, &window);
        windows::unmapped(self, &window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        menus::gone(self, &window);
        windows::destroyed(self, &window);
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
        windows::configure_request(self, &window, (x, y), (w, h));
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        windows::property_changed(self, &window, property);
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        windows::fullscreen_request(self, &window, true);
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        windows::fullscreen_request(self, &window, false);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<X11Window>,
    ) {
        menus::moved(self, &window);
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

    fn allow_selection_access(&mut self, _xwm: XwmId, selection: SelectionTarget) -> bool {
        selection::may_paste(self, selection)
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        selection::paste(self, selection, mime_type, fd);
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        selection::copied(self, selection, mime_types);
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        selection::cleared(self, selection);
    }
}
