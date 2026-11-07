//! smithay's per-protocol handlers.

mod decoration;
mod xdg_shell;

use crate::Wayluck;
use smithay::{delegate_dispatch2, wayland::output::OutputHandler};
pub use xdg_shell::handle_commit;

// wl_output: wayluck's outputs, as clients see them.
impl OutputHandler for Wayluck {}

// Every smithay protocol's requests go to its state.
delegate_dispatch2!(Wayluck);
