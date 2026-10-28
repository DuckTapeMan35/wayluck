//! xdg-decoration: every client is told its decorations are the
//! compositor's, i.e. not to draw a title bar or shadows. wayluck draws none
//! either: a desktop shell may, relative to the windows the wm lays out.

use smithay::{
    delegate_xdg_decoration,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    wayland::shell::xdg::{ToplevelSurface, decoration::XdgDecorationHandler},
};

use crate::Wayluck;

impl XdgDecorationHandler for Wayluck {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        server_side(&toplevel);
    }

    // Whatever the client would like, the answer is the same.
    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: Mode) {
        server_side(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        server_side(&toplevel);
    }
}
delegate_xdg_decoration!(Wayluck);

/// Tell the client not to decorate: in its next configure, or its first
/// one if it hasn't had it yet.
fn server_side(toplevel: &ToplevelSurface) {
    toplevel.with_pending_state(|s| s.decoration_mode = Some(Mode::ServerSide));
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}
