//! X's menus and tooltips (override-redirect windows): they place
//! themselves, anywhere, and are no window manager's. They're shown, and
//! take the pointer, only while an X window has the keyboard, so an X
//! program can't draw over a Wayland window, or catch clicks meant for it,
//! while that window is in use.

use crate::Wayluck;
use smithay::{
    desktop::{Window, WindowSurfaceType},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
    xwayland::X11Surface,
};

impl Wayluck {
    /// X's menus and tooltips shown now, bottom first: all of them while an
    /// X window has the keyboard, none otherwise.
    pub fn x_menus_shown(&self) -> &[Window] {
        if self.x_has_keyboard() {
            &self.x_menus
        } else {
            &[]
        }
    }

    /// The shown X menu's surface at a point (global), and where that
    /// surface's origin is.
    pub fn x_menu_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.x_menus_shown().iter().rev().find_map(|menu| {
            let x11 = menu.x11_surface()?;
            let origin = x11.last_configure().loc;
            let (surface, loc) = x11.surface_under(at, origin, WindowSurfaceType::ALL)?;
            Some((surface, loc.to_f64()))
        })
    }

    /// Is window one of X's menus (mapped)?
    fn is_x_menu(&self, window: &X11Surface) -> bool {
        self.x_menus.iter().any(|m| m.x11_surface() == Some(window))
    }
}

/// A menu or tooltip, mapped: shown where it says (while X has the
/// keyboard), above every window, below bars and overlays.
pub(super) fn mapped(state: &mut Wayluck, window: X11Surface) {
    tracing::debug!(
        "X: menu {} at {:?}",
        window.window_id(),
        window.last_configure()
    );
    if !state.is_x_menu(&window) {
        state.x_menus.push(Window::new_x11_window(window));
        state.queue_redraw();
    }
}

/// A menu moved itself.
pub(super) fn moved(state: &mut Wayluck, window: &X11Surface) {
    if state.is_x_menu(window) {
        state.queue_redraw();
    }
}

/// An X window was hidden or went: if it was a menu, it's drawn no more.
pub(super) fn gone(state: &mut Wayluck, window: &X11Surface) {
    let before = state.x_menus.len();
    state.x_menus.retain(|m| m.x11_surface() != Some(window));
    if state.x_menus.len() != before {
        state.queue_redraw();
    }
}
