//! What the rest of wayluck asks about layer surfaces (bars, wallpapers;
//! handlers/layer_shell.rs keeps them): the room they leave windows, and
//! under the pointer, and when one has the keyboard instead of the focused window

use crate::Wayluck;
use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle},
    wayland::shell::wlr_layer::{KeyboardInteractivity, Layer},
};
use std::cell::Cell;

/// Above windows, front first.
const ABOVE: [Layer; 2] = [Layer::Overlay, Layer::Top];
/// Below windows, front first.
const BELOW: [Layer; 2] = [Layer::Bottom, Layer::Background];
/// The same with a window fullscreen: bars go behind it, and only overlays
/// (notifications, say) stay in front.
const ABOVE_FULLSCREEN: [Layer; 1] = [Layer::Overlay];
const BELOW_FULLSCREEN: [Layer; 3] = [Layer::Top, Layer::Bottom, Layer::Background];

/// Which side of the windows.
#[derive(Clone, Copy)]
pub enum Side {
    Above,
    Below,
}

/// The usable area last told to the window manager, kept on its output.
type Told = Cell<Option<Rectangle<i32, Logical>>>;

impl Wayluck {
    /// The part of output that layer surfaces' exclusive zones leave
    /// windows, in global coordinates.
    pub fn usable(&self, output: &Output) -> Option<Rectangle<i32, Logical>> {
        let area = self.space.output_geometry(output)?;
        let mut zone = layer_map_for_output(output).non_exclusive_zone();
        zone.loc += area.loc;
        Some(zone)
    }

    /// output's layer surfaces changed: drawn anew, and if the room they
    /// leave windows did too, the window manager hears (only then: a bar
    /// redrawing its clock every second mustn't relayout the windows).
    pub fn layers_changed(&mut self, output: &Output) {
        self.queue_redraw();
        let usable = self.usable(output);
        output.user_data().insert_if_missing(Told::default);
        let told = output.user_data().get::<Told>().unwrap();
        if told.get() != usable {
            tracing::debug!("{}: usable area now {usable:?}", output.name());
            told.set(usable);
            self.wm_output_changed(output);
        }
    }

    /// The layers on one side of the windows on output, front first.
    pub fn layers(&self, output: &Output, side: Side) -> &'static [Layer] {
        let fullscreen = self.windows.fullscreen_on(&self.space, output);
        match (side, fullscreen) {
            (Side::Above, false) => &ABOVE,
            (Side::Below, false) => &BELOW,
            (Side::Above, true) => &ABOVE_FULLSCREEN,
            (Side::Below, true) => &BELOW_FULLSCREEN,
        }
    }

    /// The layer surface (or its popup) at `at` on one side of the windows,
    /// front first, and where that surface's origin is.
    pub fn layer_surface_under(
        &self,
        side: Side,
        at: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.layer_at(side, at)
            .map(|(_, surface, origin)| (surface, origin))
    }

    /// The same, with the layer surface it belongs to.
    fn layer_at(
        &self,
        side: Side,
        at: Point<f64, Logical>,
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        let output = self.space.outputs().find(|o| {
            self.space
                .output_geometry(o)
                .is_some_and(|g| g.to_f64().contains(at))
        })?;
        let area = self.space.output_geometry(output)?;
        let map = layer_map_for_output(output);
        let local = at - area.loc.to_f64();
        self.layers(output, side).iter().find_map(|&layer| {
            let surface = map.layer_under(layer, local)?;
            let geo = map.layer_geometry(surface)?;
            surface
                .surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL)
                .map(|(s, offset)| (surface.clone(), s, (offset + geo.loc + area.loc).to_f64()))
        })
    }

    /// After a layer surface's commit: if it now asks for the keyboard
    /// exclusively (a launcher, say), or just appeared above windows and
    /// takes keys when asked (a picker, say), it gets it; if it stopped
    /// taking keys at all, it gives the keyboard back.
    pub fn layer_wants_keyboard(&mut self, layer: &LayerSurface) {
        let holds = self.layer_focus.as_ref() == Some(layer);
        let appeared = just_shown(layer)
            && ABOVE.contains(&layer.layer())
            && layer.can_receive_keyboard_focus();
        if (exclusive(layer) || appeared) && !holds {
            self.layer_focus = Some(layer.clone());
            self.give_keyboard();
        } else if holds && !layer.can_receive_keyboard_focus() {
            self.layer_gone(layer);
        }
    }

    /// A click at `at`. On a layer surface that takes keys, it gets the
    /// keyboard (true). Anywhere else, one that took it when clicked gives
    /// it back to the focused window (an exclusive one keeps it).
    pub fn layer_clicked(&mut self, at: Point<f64, Logical>) -> bool {
        let below = || {
            // Wallpapers and the like only where no window is.
            self.space
                .element_under(at)
                .is_none()
                .then(|| self.layer_at(Side::Below, at))
                .flatten()
        };
        let clicked = self
            .layer_at(Side::Above, at)
            .or_else(below)
            .map(|(layer, ..)| layer);
        match clicked.filter(LayerSurface::can_receive_keyboard_focus) {
            Some(layer) => {
                self.layer_focus = Some(layer);
                self.give_keyboard();
                true
            }
            None => {
                if self.layer_focus.as_ref().is_some_and(|l| !exclusive(l)) {
                    self.layer_focus = None;
                    self.give_keyboard();
                }
                false
            }
        }
    }

    /// A layer surface went away or stopped taking keys: if it had the
    /// keyboard, the focused window gets it back.
    pub fn layer_gone(&mut self, layer: &LayerSurface) {
        if self.layer_focus.as_ref() == Some(layer) {
            self.layer_focus = None;
            self.give_keyboard();
        }
    }
}

/// Does layer take the keyboard from windows while it's shown? Only above
/// them: below, exclusive means what on-demand does.
pub fn exclusive(layer: &LayerSurface) -> bool {
    layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
        && ABOVE.contains(&layer.layer())
        && shown(layer)
}

/// Has layer something on screen (a buffer)?
fn shown(layer: &LayerSurface) -> bool {
    with_renderer_surface_state(layer.wl_surface(), |s| s.buffer().is_some()) == Some(true)
}

/// Whether a layer surface was shown at its last commit, kept on it.
type WasShown = Cell<bool>;

/// Did layer just appear: shown now, but not at its last commit? (Its
/// first buffer, or one after hiding with none.)
fn just_shown(layer: &LayerSurface) -> bool {
    layer.user_data().insert_if_missing(WasShown::default);
    let was = layer.user_data().get::<WasShown>().unwrap();
    let now = shown(layer);
    !was.replace(now) && now
}
