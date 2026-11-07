//! Input from the backend, passed on to clients: keys to the focused window,
//! the pointer to whatever is under it.

mod focus;
mod keyboard;
mod pointer;
mod seat;

use crate::Wayluck;
pub use focus::KeyboardFocus;
use smithay::backend::input::{InputBackend, InputEvent};

impl Wayluck {
    pub fn handle_input<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => self.on_key::<I>(event),
            InputEvent::PointerMotionAbsolute { event } => self.on_motion::<I>(event),
            InputEvent::PointerButton { event } => self.on_button::<I>(event),
            InputEvent::PointerAxis { event } => self.on_axis::<I>(event),
            InputEvent::PointerMotion { event } => self.on_relative_motion::<I>(event),
            _ => {}
        }
    }
}
