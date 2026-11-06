//! The clipboard beyond what wl_data_device does: the primary selection
//! (select to copy, middle-click to paste), and data-control, through which
//! clipboard tools (wl-copy, wl-paste --watch, cliphist) read and set both
//! without a window. Without data-control, wl-copy opens an invisible window
//! for the keyboard focus a client needs to set the clipboard, and the
//! window manager lays it out like any other. smithay does the work.

use smithay::wayland::selection::{
    ext_data_control,
    primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
    wlr_data_control,
};

use crate::Wayluck;

impl PrimarySelectionHandler for Wayluck {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

// wlr-data-control: what clipboard tools speak today.
impl wlr_data_control::DataControlHandler for Wayluck {
    fn data_control_state(&mut self) -> &mut wlr_data_control::DataControlState {
        &mut self.wlr_data_control_state
    }
}

// ext-data-control: the same, standardised; newer tools prefer it.
impl ext_data_control::DataControlHandler for Wayluck {
    fn data_control_state(&mut self) -> &mut ext_data_control::DataControlState {
        &mut self.ext_data_control_state
    }
}
