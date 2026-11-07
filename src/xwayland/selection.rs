//! The clipboard and the primary selection between X and Wayland, by the
//! Wayland rule: only while an X window has the keyboard may X programs
//! paste what's on them, or set them. A program in the background can
//! neither read what was copied in a Wayland window nor replace it.

use crate::Wayluck;
use smithay::wayland::selection::{
    SelectionTarget,
    data_device::{
        clear_data_device_selection, current_data_device_selection_userdata,
        request_data_device_client_selection, set_data_device_selection,
    },
    primary_selection::{
        clear_primary_selection, current_primary_selection_userdata,
        request_primary_client_selection, set_primary_selection,
    },
};
use std::os::fd::OwnedFd;

/// What Wayland clients last copied, as what it can be pasted as (None:
/// nothing, or it's X's): for an Xwayland that starts after.
#[derive(Default)]
pub struct WaylandSelections {
    clipboard: Option<Vec<String>>,
    primary: Option<Vec<String>>,
}

impl WaylandSelections {
    fn of(&mut self, ty: SelectionTarget) -> &mut Option<Vec<String>> {
        match ty {
            SelectionTarget::Clipboard => &mut self.clipboard,
            SelectionTarget::Primary => &mut self.primary,
        }
    }
}

impl Wayluck {
    /// A Wayland client set a selection (mime types: what it can be pasted
    /// as), or cleared it (None): X's side follows. Kept for an Xwayland
    /// that isn't ready yet, which is told when it is.
    pub fn wayland_selection_set(&mut self, ty: SelectionTarget, mime_types: Option<Vec<String>>) {
        *self.wayland_selections.of(ty) = mime_types.clone();
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(e) = xwm.new_selection(ty, mime_types)
        {
            tracing::warn!("X: can't take the {ty:?} selection: {e}");
        }
    }
}

/// Xwayland is ready: X's side gets what Wayland clients copied before.
pub(super) fn ready(state: &mut Wayluck) {
    for ty in [SelectionTarget::Clipboard, SelectionTarget::Primary] {
        let mime_types = state.wayland_selections.of(ty).clone();
        if mime_types.is_some() {
            state.wayland_selection_set(ty, mime_types);
        }
    }
}

/// An X program asks to paste what a Wayland client copied: only while
/// an X window has the keyboard.
pub(super) fn may_paste(state: &Wayluck, selection: SelectionTarget) -> bool {
    let allowed = state.x_has_keyboard();
    if !allowed {
        tracing::info!(
            "X: refused to paste the {selection:?} selection (no X window has the keyboard)"
        );
    }
    allowed
}

/// It may: the Wayland client that copied it writes it to fd.
pub(super) fn paste(
    state: &mut Wayluck,
    selection: SelectionTarget,
    mime_type: String,
    fd: OwnedFd,
) {
    let sent = match selection {
        SelectionTarget::Clipboard => {
            request_data_device_client_selection(&state.seat, mime_type, fd).is_ok()
        }
        SelectionTarget::Primary => {
            request_primary_client_selection(&state.seat, mime_type, fd).is_ok()
        }
    };
    if !sent {
        tracing::debug!("X: nothing to paste from the {selection:?} selection");
    }
}

/// An X program copied: what's on Wayland's side becomes that, only
/// while an X window has the keyboard.
pub(super) fn copied(state: &mut Wayluck, selection: SelectionTarget, mime_types: Vec<String>) {
    if !state.x_has_keyboard() {
        return tracing::info!(
            "X: refused to set the {selection:?} selection (no X window has the keyboard)"
        );
    }
    // Wayland's side is X's now: no Wayland client's to tell X of.
    *state.wayland_selections.of(selection) = None;
    let dh = &state.display_handle;
    match selection {
        SelectionTarget::Clipboard => set_data_device_selection(dh, &state.seat, mime_types, ()),
        SelectionTarget::Primary => set_primary_selection(dh, &state.seat, mime_types, ()),
    }
}

/// What an X program copied is gone (the program went, or gave it up):
/// so is Wayland's side of it, if that's still X's.
pub(super) fn cleared(state: &mut Wayluck, selection: SelectionTarget) {
    let dh = &state.display_handle;
    match selection {
        SelectionTarget::Clipboard => {
            if current_data_device_selection_userdata(&state.seat).is_some() {
                clear_data_device_selection(dh, &state.seat);
            }
        }
        SelectionTarget::Primary => {
            if current_primary_selection_userdata(&state.seat).is_some() {
                clear_primary_selection(dh, &state.seat);
            }
        }
    }
}
