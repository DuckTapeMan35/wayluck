//! The seat, and pointer lock and confinement.

use super::KeyboardFocus;
use crate::Wayluck;
use smithay::{
    input::{Seat, SeatHandler, SeatState, pointer::CursorImageStatus},
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    wayland::{
        pointer_constraints::PointerConstraintsHandler,
        selection::{data_device::set_data_device_focus, primary_selection::set_primary_focus},
    },
};

// wl_seat: the keyboard and pointer clients get input from. Empty for now;
// xdg-shell needs one to exist (popup grabs and moves name a seat).
impl SeatHandler for Wayluck {
    type KeyboardFocus = KeyboardFocus;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    /// A client set the pointer's image (or hid it): drawn from now on,
    /// where wayluck draws it.
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor.status = image;
        if self.cursor.drawn {
            self.queue_redraw();
        }
    }

    /// The clipboard and the primary selection follow keyboard focus: only
    /// the focused client may read them (or set them). Clipboard tools don't need it.
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&KeyboardFocus>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|f| dh.get_client(f.surface().id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }
}

// Pointer lock and confinement (games, 3D views): not offered yet, but
// smithay's pointer needs the handler.
impl PointerConstraintsHandler for Wayluck {}
