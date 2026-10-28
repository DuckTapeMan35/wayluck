//! xdg-shell: clients' windows (toplevels), their popups (popups.rs). wayluck keeps them;
//! it doesn't show them anywhere until something places them.

use crate::{
    Wayluck,
    windows::{self, Windows},
};
use smithay::{
    delegate_xdg_shell,
    desktop::Window,
    reexports::wayland_server::protocol::{wl_output, wl_seat, wl_surface::WlSurface},
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

    // Fullscreen is the window manager's to give: a client's asking goes
    // to it, and its answer comes in a transaction.
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<wl_output::WlOutput>,
    ) {
        fullscreen_request(self, &surface, true);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        fullscreen_request(self, &surface, false);
    }

    // Popups (menus, tooltips): popups.rs.
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        Wayluck::new_popup(self, surface, positioner);
    }

    // A dismissed menu goes without a last commit: the frame without it.
    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.queue_redraw();
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        self.grab_popup(surface, seat, serial);
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        self.reposition_popup(surface, positioner, token);
    }
}
delegate_xdg_shell!(Wayluck);

/// After a commit: a toplevel's first commit is answered with its first
/// configure (it can't draw anything before that), once the window manager,
/// if there is one, has placed it.
pub fn handle_commit(state: &mut Wayluck, surface: &WlSurface) {
    if state.popup_committed(surface) {
        return;
    }
    let Some(id) = Windows::id_of(surface) else {
        return;
    };
    let Some(window) = state.windows.get(id) else {
        return;
    };
    window.on_commit();
    let toplevel = window.toplevel().unwrap();
    if toplevel.is_initial_configure_sent() {
        return;
    }
    // A window manager decides where it goes first, so the first configure
    // has its size (Wayluck::apply sends it then).
    if state.wm.manager.is_some() && !state.windows.decided(id) {
        state.windows.wait(id);
        return;
    }
    tracing::info!("configuring {}", title_of(toplevel));
    toplevel.send_configure();
}

/// A client asked to be fullscreen (on), or to stop. With no window manager
/// to ask, the answer is no: a configure with its state as it was. (Not
/// before its first configure, which waits for the window manager.)
fn fullscreen_request(state: &mut Wayluck, surface: &ToplevelSurface, on: bool) {
    let asked =
        Windows::id_of(surface.wl_surface()).is_some_and(|id| state.wm_fullscreen_request(id, on));
    if !asked && surface.is_initial_configure_sent() {
        surface.send_configure();
    }
}

/// A toplevel's title, or "(untitled)", for the log.
fn title_of(surface: &ToplevelSurface) -> String {
    windows::names(surface)
        .0
        .unwrap_or_else(|| "(untitled)".into())
}
