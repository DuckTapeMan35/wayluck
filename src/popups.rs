//! Popups: menus, tooltips, dropdowns. They belong to the window that made
//! them, not to the window manager: wayluck places them where their client
//! asks (kept on the output), draws and hit-tests them with their window
//! (smithay's PopupManager does both), and gives menus the grab that closes
//! them on a click in another client's window.

use smithay::{
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy,
        find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{Seat, pointer::Focus},
    reexports::wayland_server::protocol::{wl_seat::WlSeat, wl_surface::WlSurface},
    utils::Serial,
    wayland::shell::xdg::{PopupSurface, PositionerState},
};

use crate::{Wayluck, windows::Windows};

impl Wayluck {
    /// A client made a popup: placed, and kept with its window.
    pub fn new_popup(&mut self, popup: PopupSurface, positioner: PositionerState) {
        self.place_popup(&popup, &positioner);
        if let Err(e) = self.popups.track_popup(PopupKind::Xdg(popup)) {
            tracing::warn!("a popup went before it was kept: {e}");
        }
    }

    /// A client asked to move a popup (it no longer fits, say).
    pub fn reposition_popup(
        &mut self,
        popup: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        self.place_popup(&popup, &positioner);
        popup.send_repositioned(token);
    }

    /// Where the positioner says, moved (flipped, slid) so it stays on the
    /// output, if its client allows. Popup geometry is relative to its
    /// parent, so the output is too.
    fn place_popup(&self, popup: &PopupSurface, positioner: &PositionerState) {
        let kind = PopupKind::Xdg(popup.clone());
        let on_output = || {
            let root = find_popup_root_surface(&kind).ok()?;
            let window = self.windows.get(Windows::id_of(&root)?)?;
            let window_at = self.space.element_location(window)?;
            let output = self.space.outputs().next()?;
            let mut target = self.space.output_geometry(output)?;
            target.loc -= window_at + get_popup_toplevel_coords(&kind);
            Some(positioner.get_unconstrained_geometry(target))
        };
        let geometry = on_output().unwrap_or_else(|| positioner.get_geometry());
        popup.with_pending_state(|s| s.geometry = geometry);
    }

    /// After a popup's commit: its first one is answered with its first
    /// configure. false: surface isn't a popup.
    pub fn popup_committed(&mut self, surface: &WlSurface) -> bool {
        self.popups.commit(surface);
        let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) else {
            return false;
        };
        if !popup.is_initial_configure_sent() {
            // Can only fail for a popup whose parent is gone.
            let _ = popup.send_configure();
        }
        true
    }

    /// A menu wants the keyboard and pointer until it's done: a click on
    /// another client's window closes it (and any it opened). Clicks on its
    /// own windows, and keys, go to its client, which closes it as it sees fit.
    pub fn grab_popup(&mut self, popup: PopupSurface, seat: WlSeat, serial: Serial) {
        let Some(seat) = Seat::<Self>::from_resource(&seat) else {
            return;
        };
        let kind = PopupKind::Xdg(popup);
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let mut grab = match self.popups.grab_popup(root, kind, &seat, serial) {
            Ok(grab) => grab,
            Err(e) => return tracing::debug!("a popup's grab refused: {e:?}"),
        };
        // Only in answer to the input that opened it: a grab some other
        // grab already holds is refused, popups and all.
        let ours = |s: Option<Serial>| s.unwrap_or(serial);
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(ours(grab.previous_serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial) || pointer.has_grab(ours(grab.previous_serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }
}
