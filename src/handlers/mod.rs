//! smithay's per-protocol handlers.

use crate::Wayluck;
use smithay::{delegate_dispatch2, wayland::output::OutputHandler};

// wl_output: wayluck's outputs, as clients see them.
impl OutputHandler for Wayluck {}

// Every smithay protocol's requests go to its state.
delegate_dispatch2!(Wayluck);
