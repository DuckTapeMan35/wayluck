//! X's docks (panels and bars: windows whose type says dock): like
//! layer-shell bars, they place themselves and are no window manager's.
//! Shown where they put themselves, above windows, and they take the
//! pointer; the keyboard stays where it is.

use crate::Wayluck;
use smithay::{
    desktop::{Window, WindowSurfaceType},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
    xwayland::{X11Surface, xwm::WmWindowType},
};

impl Wayluck {
    /// The X dock's surface at a point (global), and where that surface's
    /// origin is.
    pub fn x_dock_under(
        &self,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.x_docks.iter().rev().find_map(|dock| {
            let x11 = dock.x11_surface()?;
            let origin = x11.last_configure().loc;
            let (surface, loc) = x11.surface_under(at, origin, WindowSurfaceType::ALL)?;
            Some((surface, loc.to_f64()))
        })
    }

    fn is_x_dock(&self, window: &X11Surface) -> bool {
        self.x_docks.iter().any(|d| d.x11_surface() == Some(window))
    }
}

/// Is window a dock, by its type?
pub(super) fn is_dock(window: &X11Surface) -> bool {
    window.window_type() == Some(WmWindowType::Dock)
}

/// A dock, shown: drawn where it says from now on.
pub(super) fn mapped(state: &mut Wayluck, window: X11Surface) {
    tracing::info!(
        "X: dock {} at {:?}",
        window.window_id(),
        window.last_configure()
    );
    if !state.is_x_dock(&window) {
        state.x_docks.push(Window::new_x11_window(window));
        state.queue_redraw();
        state.reserved_changed();
    }
}

/// A dock moved or resized itself.
pub(super) fn moved(state: &mut Wayluck, window: &X11Surface) {
    if state.is_x_dock(window) {
        state.queue_redraw();
    }
}

/// An X window was hidden or went: if it was a dock, it's drawn no more.
pub(super) fn gone(state: &mut Wayluck, window: &X11Surface) {
    let before = state.x_docks.len();
    state.x_docks.retain(|d| d.x11_surface() != Some(window));
    if state.x_docks.len() != before {
        state.queue_redraw();
        state.reserved_changed();
    }
}
