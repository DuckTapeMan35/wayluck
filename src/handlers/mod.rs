//! smithay's per-protocol handlers.

mod clipboard;
mod compositor;
mod decoration;
mod dmabuf;
mod layer_shell;
mod syncobj;
mod xdg_shell;
mod xwayland;

use crate::{Wayluck, focus::KeyboardFocus};
pub use compositor::compositor_state_of;
pub use layer_shell::layer_committed;
use smithay::{
    delegate_dispatch2,
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::{CursorImageStatus, Focus},
    },
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::Serial,
    wayland::{
        output::OutputHandler,
        pointer_constraints::PointerConstraintsHandler,
        selection::{
            SelectionHandler,
            data_device::{
                DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
            },
            primary_selection::set_primary_focus,
        },
    },
};
pub use xdg_shell::handle_commit;

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
    /// the focused client may read them (or set them). Clipboard tools don't
    /// need it (clipboard.rs).
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&KeyboardFocus>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|f| dh.get_client(f.surface().id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }
}

// wl_data_device_manager: the clipboard, and drag and drop. smithay does the
// work; wayluck only says who has focus.
impl SelectionHandler for Wayluck {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Wayluck {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

/// A client started dragging something with the pointer: smithay carries it
/// to wherever it's dropped (drawing no icon for it yet). Without a pointer
/// grab to go with it (touch: wayluck has none), it's cancelled.
impl WaylandDndGrabHandler for Wayluck {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        let pointer = seat.get_pointer();
        let start = pointer.as_ref().and_then(|p| p.grab_start_data());
        let (GrabType::Pointer, Some(pointer), Some(start)) = (type_, pointer, start) else {
            return source.cancel();
        };
        let grab = DnDGrab::new_pointer(&self.display_handle, start, source, seat);
        pointer.set_grab(self, grab, serial, Focus::Keep);
    }
}

impl DndGrabHandler for Wayluck {}

// Pointer lock and confinement (games, 3D views): not offered yet, but
// smithay's pointer needs the handler.
impl PointerConstraintsHandler for Wayluck {}

// wl_output: wayluck's outputs, as clients see them.
impl OutputHandler for Wayluck {}

// Every smithay protocol's requests go to its state.
delegate_dispatch2!(Wayluck);
