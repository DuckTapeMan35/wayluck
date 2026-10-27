//! smithay's per-protocol handlers.

mod compositor;
mod xdg_shell;

pub use xdg_shell::handle_commit;

use smithay::{
    delegate_data_device, delegate_output, delegate_seat,
    input::{Seat, SeatHandler, SeatState, pointer::CursorImageStatus},
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    wayland::{
        output::OutputHandler,
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                set_data_device_focus,
            },
        },
    },
};

use crate::Wayluck;

// wl_seat: the keyboard and pointer clients get input from. Empty for now;
// xdg-shell needs one to exist (popup grabs and moves name a seat).
impl SeatHandler for Wayluck {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}

    /// The clipboard follows keyboard focus: only the focused client may
    /// read it (or set it).
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}
delegate_seat!(Wayluck);

// wl_data_device_manager: the clipboard, and drag and drop. smithay does the
// work; wayluck only says who has focus.
impl SelectionHandler for Wayluck {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Wayluck {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for Wayluck {}
impl ServerDndGrabHandler for Wayluck {}
delegate_data_device!(Wayluck);

// wl_output: wayluck's outputs, as clients see them.
impl OutputHandler for Wayluck {}
delegate_output!(Wayluck);
