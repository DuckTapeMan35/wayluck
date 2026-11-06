//! The clipboard and drag and drop between Wayland clients (smithay does
//! the work: wayluck says who has the keyboard), and the clipboard's
//! side that X programs paste from.

mod control;

use crate::Wayluck;
use smithay::{
    input::{
        Seat,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::Focus,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::Serial,
    wayland::selection::{
        SelectionHandler, SelectionSource, SelectionTarget,
        data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler},
    },
};
use std::os::fd::OwnedFd;

impl SelectionHandler for Wayluck {
    /// A selection set from X; Wayland clients' carry their own data.
    type SelectionUserData = ();

    /// A Wayland client set the clipboard (or primary selection), or
    /// cleared it: X's side follows, for X programs to paste once one has
    /// the keyboard.
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        self.wayland_selection_set(ty, source.map(|s| s.mime_types()));
    }

    /// A Wayland client pastes what an X program copied: X's side hands
    /// it over.
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(e) = xwm.send_selection(ty, mime_type, fd)
        {
            tracing::warn!("X: can't hand over the {ty:?} selection: {e}");
        }
    }
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
