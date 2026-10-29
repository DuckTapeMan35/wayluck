//! Drawing a frame, whatever the backend: nested (winit) or on hardware (DRM).

use crate::{Wayluck, layers::Side, windows::Windows};
use smithay::{
    backend::renderer::{
        Color32F, ImportAll, ImportMem, Renderer, Texture,
        damage::{Error as DamageError, OutputDamageTracker},
        element::{
            AsRenderElements, Kind,
            memory::MemoryRenderBufferRenderElement,
            solid::{SolidColorBuffer, SolidColorRenderElement},
            surface::WaylandSurfaceRenderElement,
        },
    },
    desktop::layer_map_for_output,
    output::Output,
    render_elements,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::shell::wlr_layer::Layer,
};
use std::time::Duration;

/// The parts of an output that changed, in its pixels; None: nothing did.
pub type Damage = Option<Vec<Rectangle<i32, Physical>>>;

/// The background, where no window is.
pub const CLEAR: [f32; 4] = [0.1, 0.1, 0.12, 1.0];

render_elements! {
    /// Anything wayluck draws: clients' surfaces, and borders.
    pub Element<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Border = SolidColorRenderElement,
    Cursor = MemoryRenderBufferRenderElement<R>,
}

/// Draw output into a framebuffer (nested: the window's), over the clear
/// colour. Returns what changed since the frame `age` frames ago (0:
/// unknown, all of it).
pub fn render_output<R>(
    state: &mut Wayluck,
    output: &Output,
    renderer: &mut R,
    framebuffer: &mut R::Framebuffer<'_>,
    damage_tracker: &mut OutputDamageTracker,
    age: usize,
) -> Result<Damage, DamageError<R::Error>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + Send + 'static,
{
    // Front to back: the top window first, its border just behind it, so a
    // window's border never covers the window above it.
    let elements = elements(state, output, renderer);
    let result = damage_tracker.render_output(renderer, framebuffer, age, &elements, CLEAR)?;
    Ok(result.damage.cloned())
}

/// What output shows, front to back: the cursor (where wayluck draws it),
/// bars and overlays, every window on it with its border, then wallpapers
/// and what else is below windows. Each backend draws these its own way.
pub fn elements<R>(state: &mut Wayluck, output: &Output, renderer: &mut R) -> Vec<Element<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let Some(area) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    // Front to back: the top window first, its border just behind it, so a
    // window's border never covers the window above it.
    let mut elements: Vec<Element<R>> = Vec::new();
    let pointer = state.seat.get_pointer().unwrap().current_location();
    if area.to_f64().contains(pointer) {
        let at = pointer - area.loc.to_f64();
        elements.extend(state.cursor.elements(renderer, at, scale));
    }
    let shown: Vec<_> = state.space.elements().rev().cloned().collect();
    // Bars and overlays above windows; then the windows; then what's below.
    let above = state.layers(output, Side::Above);
    elements.extend(layer_elements(output, renderer, scale, above));
    for window in shown {
        let Some(loc) = state.space.element_location(&window) else {
            continue;
        };
        let at = (loc - window.geometry().loc - area.loc).to_physical_precise_round(scale);
        elements.extend(window.render_elements::<Element<R>>(renderer, at, scale, 1.0));
        let id = window
            .toplevel()
            .and_then(|t| Windows::id_of(t.wl_surface()));
        if let Some(border) = id.and_then(|id| state.windows.border_mut(id)) {
            let content = Rectangle::new(loc, window.geometry().size);
            elements.extend(
                border
                    .elements(content, area.loc, scale)
                    .map(Element::Border),
            );
        }
    }
    let below = state.layers(output, Side::Below);
    elements.extend(layer_elements(output, renderer, scale, below));
    elements
}

/// The layer surfaces of `layers` on output (with their popups), front first.
fn layer_elements<R>(
    output: &Output,
    renderer: &mut R,
    scale: Scale<f64>,
    layers: &[Layer],
) -> Vec<Element<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let map = layer_map_for_output(output);
    let mut elements = Vec::new();
    for &layer in layers {
        // A layer's surfaces come back to front: the last is on top.
        for surface in map.layers_on(layer).rev() {
            let Some(geo) = map.layer_geometry(surface) else {
                continue;
            };
            let at = geo.loc.to_physical_precise_round(scale);
            elements.extend(surface.render_elements::<Element<R>>(renderer, at, scale, 1.0));
        }
    }
    elements
}

/// After a frame is on screen: tell the clients on output, so they draw
/// their next one, and drop what's gone.
pub fn frame_done(state: &mut Wayluck, output: &Output) {
    let now = state.start_time.elapsed();
    for window in state.space.elements() {
        window.send_frame(output, now, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.send_frame(output, now, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    drop(map);
    state.space.refresh();
    state.popups.cleanup();
}

/// A window's border, drawn around it: outside what it shows, so it fits
/// whatever size the client really drew at.
pub struct Border {
    width: i32,
    /// Top, right, bottom, left. Kept from frame to frame, so the damage
    /// tracker knows when a side really changed.
    sides: [SolidColorBuffer; 4],
}

impl Border {
    /// width pixels wide; colours 0xAARRGGBB, top, right, bottom, left.
    pub fn new(width: u32, colors: [u32; 4]) -> Self {
        let mut border = Self {
            width: 0,
            sides: Default::default(),
        };
        border.set(width, colors);
        border
    }

    pub fn set(&mut self, width: u32, colors: [u32; 4]) {
        self.width = width as i32;
        for (side, color) in self.sides.iter_mut().zip(colors) {
            side.set_color(argb(color));
        }
    }

    /// Its sides around content (in global coordinates), on an output
    /// whose top left is origin.
    fn elements(
        &mut self,
        content: Rectangle<i32, Logical>,
        origin: Point<i32, Logical>,
        scale: Scale<f64>,
    ) -> impl Iterator<Item = SolidColorRenderElement> + '_ {
        let w = self.width;
        let (x, y, cw, ch) = (content.loc.x, content.loc.y, content.size.w, content.size.h);
        let rects = [
            Rectangle::new((x - w, y - w).into(), (cw + 2 * w, w).into()),
            Rectangle::new((x + cw, y).into(), (w, ch).into()),
            Rectangle::new((x - w, y + ch).into(), (cw + 2 * w, w).into()),
            Rectangle::new((x - w, y).into(), (w, ch).into()),
        ];
        self.sides
            .iter_mut()
            .zip(rects)
            .filter(move |_| w > 0)
            .map(move |(side, rect)| {
                side.resize(rect.size);
                let at = (rect.loc - origin).to_physical_precise_round(scale);
                SolidColorRenderElement::from_buffer(side, at, scale, 1.0, Kind::Unspecified)
            })
    }
}

/// 0xAARRGGBB as smithay's colour: components 0 to 1, premultiplied.
fn argb(c: u32) -> Color32F {
    let part = |shift: u32| ((c >> shift) & 0xff) as f32 / 255.0;
    let a = part(24);
    Color32F::new(part(16) * a, part(8) * a, part(0) * a, a)
}
