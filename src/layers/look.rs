//! How the window manager has layer surfaces look: rounded corners, a
//! shadow, blur behind them. Kept by their surface, from frame to frame.

use crate::render::{
    blur::{Behind, Mode},
    rounded::ClipIds,
    shadow::{Shadow, Spec},
};

/// What a transaction changes of a layer surface's look.
pub enum LookChange {
    Corners(u32),
    Shadow(Spec),
    Blur(Mode),
}

/// A layer surface's look, and what's kept to draw it.
#[derive(Default)]
pub struct Look {
    /// How round its corners are (0: square).
    pub radius: u32,
    pub shadow: Option<Shadow>,
    pub blur: Mode,
    /// Its blur, kept from frame to frame.
    pub behind: Behind,
    /// The ids its surfaces are drawn with, cut at the corners.
    pub clip_ids: ClipIds,
}

impl Look {
    pub fn change(&mut self, change: LookChange) {
        match change {
            LookChange::Corners(radius) => self.radius = radius,
            LookChange::Shadow(spec) => match &mut self.shadow {
                Some(shadow) => shadow.set(spec),
                None => self.shadow = Some(Shadow::new(spec)),
            },
            LookChange::Blur(mode) => self.blur = mode,
        }
    }
}
