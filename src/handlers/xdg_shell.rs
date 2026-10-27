//! xdg-shell: clients' windows (toplevels). wayluck keeps them; it doesn't
//! show them anywhere until something places them.

use crate::{
    Wayluck,
    windows::{self, Windows},
};
use smithay::{
    delegate_xdg_shell,
    desktop::Window,
    reexports::wayland_server::protocol::{wl_seat, wl_surface::WlSurface},
    utils::Serial,
    wayland::shell::xdg::{
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    },
};

impl XdgShellHandler for Wayluck {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let id = self.add_window(Window::new_wayland_window(surface));
        tracing::info!("new window {id}");
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = Windows::id_of(surface.wl_surface()) {
            tracing::info!("window {id} closed: {}", title_of(&surface));
            self.remove_window(id);
        }
    }

    // The window manager hears what a window is called, as it changes.
    fn title_changed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = Windows::id_of(surface.wl_surface()) {
            self.wm_window_title(id);
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = Windows::id_of(surface.wl_surface()) {
            self.wm_window_app_id(id);
        }
    }

    // Popups (menus, tooltips) come later.
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}
delegate_xdg_shell!(Wayluck);

/// After a commit: a toplevel's first commit is answered with its first
/// configure (it can't draw anything before that).
pub fn handle_commit(state: &mut Wayluck, surface: &WlSurface) {
    let Some(window) = Windows::id_of(surface).and_then(|id| state.windows.get(id)) else {
        return;
    };
    window.on_commit();
    let toplevel = window.toplevel().unwrap();
    if !toplevel.is_initial_configure_sent() {
        tracing::info!("configuring {}", title_of(toplevel));
        toplevel.send_configure();
    }
}

/// A toplevel's title, or "(untitled)", for the log.
fn title_of(surface: &ToplevelSurface) -> String {
    windows::names(surface)
        .0
        .unwrap_or_else(|| "(untitled)".into())
}
