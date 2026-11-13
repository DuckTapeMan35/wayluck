//! ext-background-effect: clients asking for what's behind their surfaces
//! to be blurred, and where. smithay keeps the region in each surface's
//! state, applied with its commits.

use crate::Wayluck;
use smithay::{
    desktop::Window,
    reexports::wayland_server::{DisplayHandle, protocol::wl_surface::WlSurface},
    utils::{Logical, Rectangle},
    wayland::{
        background_effect::{
            BackgroundEffectState, BackgroundEffectSurfaceCachedState, ExtBackgroundEffectHandler,
        },
        compositor::{RectangleKind, with_states},
        seat::WaylandFocus,
    },
};

impl ExtBackgroundEffectHandler for Wayluck {}

impl Wayluck {
    pub fn background_effect_global(dh: &DisplayHandle) {
        BackgroundEffectState::new::<Self>(dh);
    }
}

pub fn blur_region(window: &Window) -> Option<Vec<Rectangle<i32, Logical>>> {
    let surface = window.wl_surface()?;
    surface_blur_region(&surface, window.geometry())
}

/// Where surface's client asks to blur behind it, as blur_region, from
/// rect's top left (in the surface's coordinates), inside rect.
pub fn surface_blur_region(
    surface: &WlSurface,
    rect: Rectangle<i32, Logical>,
) -> Option<Vec<Rectangle<i32, Logical>>> {
    let region = with_states(&surface, |states| {
        let mut cached = states
            .cached_state
            .get::<BackgroundEffectSurfaceCachedState>();
        cached.current().blur_region.clone()
    })?;
    // The surface's coordinates, added and taken away in order.
    let mut rects: Vec<Rectangle<i32, Logical>> = Vec::new();
    for (kind, rect) in region.rects {
        match kind {
            RectangleKind::Add => {
                let new = rect.subtract_rects(rects.iter().copied());
                rects.extend(new);
            }
            RectangleKind::Subtract => {
                rects = (rects.into_iter())
                    .flat_map(|r| r.subtract_rect(rect))
                    .collect();
            }
        }
    }
    let inside = Rectangle::from_size(rect.size);
    Some(
        (rects.into_iter())
            .filter_map(|r| Rectangle::new(r.loc - rect.loc, r.size).intersection(inside))
            .collect(),
    )
}
