//! Drawing a frame, whatever the backend: nested (winit) or on hardware (DRM).

pub mod cursor;
pub mod rounded;

use crate::{Wayluck, layers::Side, scenes::Scene};
use rounded::{AsGles, BorderElement, ClipIds, ClippedSurface, Shaders};
use smithay::{
    backend::renderer::{
        Color32F, ImportAll, ImportMem, Renderer, Texture,
        damage::{Error as DamageError, OutputDamageTracker},
        element::{
            AsRenderElements, Element as _, Id, Kind,
            memory::MemoryRenderBufferRenderElement,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
        },
        gles::element::PixelShaderElement,
    },
    desktop::{PopupManager, Window, layer_map_for_output},
    output::Output,
    render_elements,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::{seat::WaylandFocus, shell::wlr_layer::Layer},
};
use std::time::Duration;

/// The parts of an output that changed, in its pixels; None: nothing did.
pub type Damage = Option<Vec<Rectangle<i32, Physical>>>;

/// The background, where no window is.
pub const CLEAR: [f32; 4] = [0.1, 0.1, 0.12, 1.0];

render_elements! {
    /// Anything wayluck draws: clients' surfaces, and borders.
    pub Element<R> where R: ImportAll + ImportMem + AsGles;
    Surface = WaylandSurfaceRenderElement<R>,
    Clipped = ClippedSurface<R>,
    Border = BorderElement,
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
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: Clone + Texture + Send + 'static,
{
    // Front to back: the top window first, its border just behind it, so a
    // window's border never covers the window above it.
    let elements = elements(state, output, renderer);
    let result = damage_tracker.render_output(renderer, framebuffer, age, &elements, CLEAR)?;
    Ok(result.damage.cloned())
}

/// What output shows, front to back: the cursor (where wayluck draws it),
/// bars and overlays, the windows of the scene it shows with their borders,
/// then wallpapers and what else is below windows. Each backend draws these its own way.
pub fn elements<R>(state: &mut Wayluck, output: &Output, renderer: &mut R) -> Vec<Element<R>>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
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

    // Bars and overlays above windows; then the windows; then what's below.
    // A virtual output's are its template's.
    let layers_from = state.layers_from(output);
    let above = state.layers(output, Side::Above);
    if let Some(from) = &layers_from {
        elements.extend(layer_elements(from, renderer, scale, above));
    }

    // X's menus and tooltips, where they put themselves
    // (only while X has the keyboard). The last mapped on top.
    for menu in state.x_menus_shown().iter().rev() {
        if let Some(x11) = menu.x11_surface() {
            let at = x11.last_configure().loc - area.loc;
            elements.extend(window_elements(renderer, menu, at, scale, 0, None));
        }
    }

    // The windows of the scene it shows, where they are in it: the scene
    // is drawn from the output's corner. Top window first.
    let shown: Vec<_> = (state.scenes.shown_on(output))
        .map(|scene| scene.windows().rev().collect())
        .unwrap_or_default();
    for (id, at) in shown {
        let Some(window) = state.windows.get(id).cloned() else {
            continue;
        };
        // The window, then its border behind it (with its corners' radius).
        let Some(border) = state.windows.border_mut(id) else {
            elements.extend(window_elements(renderer, &window, at, scale, 0, None));
            continue;
        };
        let ids = Some(&mut border.clip_ids);
        elements.extend(window_elements(
            renderer,
            &window,
            at,
            scale,
            border.radius,
            ids,
        ));
        let content = Rectangle::new(at, window.geometry().size);
        elements.extend(
            border
                .element(renderer, content, Point::default())
                .map(Element::Border),
        );
    }

    // The outlines on the scene it shows, behind its windows: they're
    // around them (and a floating window over one hides it).
    if let Some(scene) = state.scenes.shown_on_mut(output) {
        elements.extend(outline_elements(renderer, scene));
    }

    let below = state.layers(output, Side::Below);
    if let Some(from) = &layers_from {
        elements.extend(layer_elements(from, renderer, scale, below));
    }

    elements
}

/// A window, its rectangle's top left at `at` (on the output): its popups,
/// then its surfaces with the rectangle's corners cut off, rounded radius pixels.
pub fn window_elements<R>(
    renderer: &mut R,
    window: &Window,
    at: Point<i32, Logical>,
    scale: Scale<f64>,
    radius: u32,
    mut ids: Option<&mut ClipIds>,
) -> Vec<Element<R>>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let corner = window.geometry().loc;
    let drawn_at = (at - corner).to_physical_precise_round(scale);
    let Some(surface) = window.wl_surface() else {
        return Vec::new();
    };
    let surface = &*surface;
    let mut elements = Vec::new();
    // Popups first (they're above), whole.
    for (popup, offset) in PopupManager::popups_for_surface(surface) {
        let popup_at =
            drawn_at + (corner + offset - popup.geometry().loc).to_physical_precise_round(scale);
        let popup: Vec<WaylandSurfaceRenderElement<R>> = render_elements_from_surface_tree(
            renderer,
            popup.wl_surface(),
            popup_at,
            scale,
            1.0,
            Kind::Unspecified,
        );
        elements.extend(popup.into_iter().map(Element::Surface));
    }
    let surfaces: Vec<WaylandSurfaceRenderElement<R>> = render_elements_from_surface_tree(
        renderer,
        surface,
        drawn_at,
        scale,
        1.0,
        Kind::Unspecified,
    );
    let Some(shaders) = Shaders::get(renderer).filter(|_| radius > 0) else {
        elements.extend(surfaces.into_iter().map(Element::Surface));
        return elements;
    };
    let rect = Rectangle::new(at, window.geometry().size).to_physical_precise_round(scale);
    let radius = radius as f64 * scale.x;
    // Each surface keeps its id from frame to frame; those gone are dropped.
    let mut old = ids.as_deref_mut().map(std::mem::take).unwrap_or_default();
    let mut kept = ClipIds::new();
    elements.extend(surfaces.into_iter().map(|s| {
        let own = s.id().clone();
        let id = old.remove(&own).unwrap_or_else(Id::new);
        kept.insert(own, id.clone());
        Element::Clipped(ClippedSurface::new(
            s,
            id,
            shaders.clip.clone(),
            scale,
            rect,
            radius,
        ))
    }));
    if let Some(ids) = ids {
        *ids = kept;
    }
    elements
}

/// The outlines on a scene (rounded::Outline), last first, as they're drawn
/// from the corner of an output that shows it.
fn outline_elements<R: AsGles>(renderer: &mut R, scene: &mut Scene) -> Vec<Element<R>> {
    let drawn: Vec<_> = scene
        .outlines
        .iter()
        .filter_map(|o| Some((*o, o.drawn()?)))
        .collect();
    let Some(program) = Shaders::get(renderer)
        .map(|s| s.border)
        .filter(|_| !drawn.is_empty())
    else {
        scene.drawn_outlines.clear();
        return Vec::new();
    };
    scene.drawn_outlines.truncate(drawn.len());
    let mut elements = Vec::new();
    for (i, (outline, (area, uniforms))) in drawn.into_iter().enumerate() {
        match scene.drawn_outlines.get_mut(i) {
            // As it was last drawn: unchanged, to the damage tracker too.
            Some((last, shader)) if *last == outline => {
                elements.push(shader.clone());
            }
            Some((last, shader)) => {
                shader.resize(area, None);
                shader.update_uniforms(uniforms);
                *last = outline;
                elements.push(shader.clone());
            }
            None => {
                let shader = PixelShaderElement::new(
                    program.clone(),
                    area,
                    None,
                    1.0,
                    uniforms,
                    Kind::Unspecified,
                );
                scene.drawn_outlines.push((outline, shader.clone()));
                elements.push(shader);
            }
        }
    }
    (elements.into_iter().rev())
        .map(|shader| Element::Border(BorderElement(shader)))
        .collect()
}

/// The layer surfaces of `layers` on output (with their popups), front first.
fn layer_elements<R>(
    output: &Output,
    renderer: &mut R,
    scale: Scale<f64>,
    layers: &[Layer],
) -> Vec<Element<R>>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
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
    state.space.refresh();
    // Only to windows on it: those elsewhere are paced by their own output
    for window in state.space.elements() {
        if !state.space.outputs_for_element(window).contains(output) {
            continue;
        }
        window.send_frame(output, now, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    for menu in &state.x_menus {
        menu.send_frame(output, now, Some(Duration::ZERO), |_, _| {
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
    state.popups.cleanup();
}

/// A window's border, drawn around it: outside what it shows, so it fits
/// whatever size the client really drew at. Its corners are rounded.
pub struct Border {
    width: i32,
    /// Top, right, bottom, left: 0xAARRGGBB.
    colors: [u32; 4],
    /// Drawn by the border shader: kept from frame to frame,
    /// so the damage tracker knows when it really changed. Made when it's
    /// first drawn.
    shader: Option<PixelShaderElement>,
    /// How round the window's corners are, and the border's with them.
    pub radius: u32,
    /// The ids its surfaces are drawn under, while radius stays the same.
    pub clip_ids: ClipIds,
    /// Its width or colours changed since the shader was told.
    changed: bool,
}

impl Border {
    /// width pixels wide; colours 0xAARRGGBB, top, right, bottom, left.
    pub fn new(width: u32, colors: [u32; 4], radius: u32) -> Self {
        Self {
            width: width as i32,
            colors,
            shader: None,
            radius,
            clip_ids: ClipIds::new(),
            changed: true,
        }
    }

    pub fn set(&mut self, width: u32, colors: [u32; 4], radius: u32) {
        if self.radius != radius {
            // Its surfaces are new to the damage tracker: drawn again whole.
            self.clip_ids.clear();
        }
        if self.width != width as i32 || self.colors != colors || self.radius != radius {
            self.width = width as i32;
            self.colors = colors;
            self.radius = radius;
            self.changed = true;
        }
    }

    /// It around content (in global coordinates), on an output whose top
    /// left is origin; None if it has no width (or can't be drawn).
    fn element<R: AsGles>(
        &mut self,
        renderer: &mut R,
        content: Rectangle<i32, Logical>,
        origin: Point<i32, Logical>,
    ) -> Option<BorderElement> {
        if self.width <= 0 {
            return None;
        }
        let content = Rectangle::new(content.loc - origin, content.size);
        let area = rounded::border_area(content, self.width);
        let uniforms = rounded::border_uniforms(
            self.radius as f32,
            self.width as f32,
            self.colors.map(|c| argb(c).components()),
        );
        match &mut self.shader {
            Some(shader) => {
                shader.resize(area, None);
                if self.changed {
                    shader.update_uniforms(uniforms);
                }
            }
            None => {
                let program = Shaders::get(renderer)?.border;
                let shader =
                    PixelShaderElement::new(program, area, None, 1.0, uniforms, Kind::Unspecified);
                self.shader = Some(shader);
            }
        }
        self.changed = false;
        self.shader.clone().map(BorderElement)
    }
}

/// 0xAARRGGBB as smithay's colour: components 0 to 1, premultiplied.
pub fn argb(c: u32) -> Color32F {
    let part = |shift: u32| ((c >> shift) & 0xff) as f32 / 255.0;
    let a = part(24);
    Color32F::new(part(16) * a, part(8) * a, part(0) * a, a)
}
