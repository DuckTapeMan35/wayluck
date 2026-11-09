//! Drawing a frame, whatever the backend: nested (winit) or on hardware (DRM).

pub mod blur;
pub mod cursor;
pub mod resized;
pub mod rounded;
pub mod shaders;
pub mod shadow;
pub mod text;

use crate::{
    Wayluck,
    layers::{Side, look::Look},
    surfaces::background_effect::blur_region,
    wm::scenes::Scene,
};
use resized::Resized;
use rounded::{ClipIds, ClippedSurface};
use shaders::{AsGles, ShaderElement, Shaders};
use smithay::{
    backend::renderer::{
        Color32F, ImportAll, ImportMem, Renderer, Texture,
        damage::{Error as DamageError, OutputDamageTracker},
        element::{
            Element as _, Id, Kind,
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
        },
        gles::element::PixelShaderElement,
    },
    desktop::{PopupManager, Window, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    render_elements,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::{seat::WaylandFocus, shell::wlr_layer::Layer},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// The parts of an output that changed, in its pixels; None: nothing did.
pub type Damage = Option<Vec<Rectangle<i32, Physical>>>;

/// The background, where no window is.
pub const CLEAR: [f32; 4] = [0.1, 0.1, 0.12, 1.0];

render_elements! {
    /// Anything wayluck draws: clients' surfaces, and borders.
    pub Element<R> where R: ImportAll + ImportMem + AsGles;
    Surface = WaylandSurfaceRenderElement<R>,
    Clipped = ClippedSurface<R>,
    Shader = ShaderElement,
    Memory = MemoryRenderBufferRenderElement<R>,
    Solid = SolidColorRenderElement,
    Resized = resized::Resized<R>,
    Blur = blur::BlurElement,
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

    // The picker, while it's up here, covers everything else.
    if let Some(picker) = state.picker_elements(output, renderer) {
        elements.extend(picker);
        return elements;
    }

    elements.extend(output_elements(state, output, renderer, scale));
    elements
}

/// What output shows under the cursor and the picker, front to back, drawn
/// from its top left corner at scale: bars and overlays, the windows of the
/// scene it shows with their borders, then wallpapers and what else is
/// below windows.
pub fn output_elements<R>(
    state: &mut Wayluck,
    output: &Output,
    renderer: &mut R,
    scale: Scale<f64>,
) -> Vec<Element<R>>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let Some(area) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let mut elements = Vec::new();

    // Bars and overlays above windows; then the windows; then what's below.
    // A virtual output's are its template's.
    let layers_from = state.layers_from(output);
    let above = state.layers(output, Side::Above);
    let looks = &mut state.layer_looks;
    if let Some(from) = &layers_from {
        elements.extend(layer_elements(from, renderer, scale, above, looks));
    }

    // X's menus and tooltips, where they put themselves
    // (only while X has the keyboard). The last mapped on top.
    for menu in state.x_menus_shown().iter().rev() {
        if let Some(x11) = menu.x11_surface() {
            let at = x11.last_configure().loc - area.loc;
            elements.extend(window_elements(renderer, menu, at, scale, 0, None));
        }
    }

    // X's docks, where they put themselves, below its menus.
    for dock in state.x_docks.iter().rev() {
        if let Some(x11) = dock.x11_surface() {
            let at = x11.last_configure().loc - area.loc;
            elements.extend(window_elements(renderer, dock, at, scale, 0, None));
        }
    }

    // The windows of the scene it shows, where they are in it: the scene
    // is drawn from the output's corner. Top window first.
    let shown: Vec<_> = (state.scenes.shown_on(output))
        .map(|scene| scene.windows().rev().collect())
        .unwrap_or_default();
    let now = Instant::now();
    let whole = Rectangle::from_size(area.size.to_physical_precise_round(scale));
    // Windows blurred behind: where in elements what's below each starts,
    // and where it is, top window first.
    let mut blurred = Vec::new();
    for (id, at) in shown {
        let Some(window) = state.windows.get(id).cloned() else {
            continue;
        };

        // On its way somewhere: drawn where it is now, stretched to that
        // size (whatever size its client drew).
        let size = window.geometry().size;
        let moving = state.windows.animated_at(id, now);
        let (at, drawn_size) = moving.map_or((at, size), |r| (r.loc, r.size));
        // The window, then its border behind it (with its corners' radius).
        let border = state.windows.border_mut(id);
        let radius = border.as_ref().map_or(0, |b| b.radius);
        let ids = border.map(|b| &mut b.clip_ids);
        let mut drawn = window_elements(renderer, &window, at, scale, radius, ids);
        if moving.is_some()
            && let Some(ids) = state.windows.animation_ids(id)
            && size.w > 0
            && size.h > 0
        {
            let origin = at.to_physical_precise_round(scale);
            let by = Scale::from((
                drawn_size.w as f64 / size.w as f64,
                drawn_size.h as f64 / size.h as f64,
            ));
            drawn = Resized::many(drawn, (origin, by), Point::default(), whole, scale, ids);
        }
        elements.extend(drawn);

        let content = Rectangle::new(at, drawn_size);
        let part = match state.windows.blur_mode(id) {
            blur::Mode::On => Some(blur::Part::Whole),
            blur::Mode::Off => None,
            // Where it asks, stretched with it if it's on its way somewhere.
            blur::Mode::Client => blur_region(&window).filter(|r| !r.is_empty()).map(|rects| {
                let (sx, sy) = (
                    drawn_size.w as f64 / size.w.max(1) as f64,
                    drawn_size.h as f64 / size.h.max(1) as f64,
                );
                let rects = (rects.into_iter())
                    .map(|r| {
                        r.to_f64()
                            .upscale((sx, sy))
                            .to_physical(scale)
                            .to_i32_round()
                    })
                    .collect();
                blur::Part::Only(rects)
            }),
        };
        if let Some(part) = part {
            let window = content.to_physical_precise_round(scale);
            blurred.push((elements.len(), id, window, part, radius as f64 * scale.x));
        }

        let mut around = (content, radius);
        if let Some(border) = state.windows.border_mut(id) {
            around = border.outer(content);
            elements.extend(
                border
                    .element(renderer, content, Point::default())
                    .map(Element::Shader),
            );
        }
        // Its shadow behind both.
        if let Some(shadow) = state.windows.shadow_mut(id) {
            let (around, radius) = around;
            elements.extend(
                shadow
                    .element(renderer, around, radius, Point::default())
                    .map(Element::Shader),
            );
        }
    }

    // The outlines on the scene it shows, behind its windows: they're
    // around them (and a floating window over one hides it).
    if let Some(scene) = state.scenes.shown_on_mut(output) {
        elements.extend(outline_elements(renderer, scene));
    }

    let below = state.layers(output, Side::Below);
    let looks = &mut state.layer_looks;
    if let Some(from) = &layers_from {
        elements.extend(layer_elements(from, renderer, scale, below, looks));
    }

    // Each blurred window's blur, behind it: the lowest first, so a window
    // above it blurs it too.
    let strength = state.windows.blur;
    for (at, id, window, part, radius) in blurred.into_iter().rev() {
        let behind = state.windows.behind_mut(id);
        let blur = behind.element(
            renderer,
            &elements[at..],
            window,
            part,
            whole.size,
            radius,
            strength,
            scale,
        );
        if let Some(blur) = blur {
            elements.insert(at, Element::Blur(blur));
        }
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
    ids: Option<&mut ClipIds>,
) -> Vec<Element<R>>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let Some(surface) = window.wl_surface() else {
        return Vec::new();
    };
    let geometry = window.geometry();
    let rect = Rectangle::new(at, geometry.size);
    surface_elements(renderer, &surface, geometry.loc, rect, scale, radius, ids)
}

/// A surface (a window's, a layer surface's) drawn so that the rectangle
/// corner (in its own coordinates) is at rect's top left: its popups, then
/// its surfaces with rect's corners cut off, rounded radius pixels.
fn surface_elements<R>(
    renderer: &mut R,
    surface: &WlSurface,
    corner: Point<i32, Logical>,
    rect: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    radius: u32,
    mut ids: Option<&mut ClipIds>,
) -> Vec<Element<R>>
where
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: Clone + Texture + Send + 'static,
{
    let drawn_at = (rect.loc - corner).to_physical_precise_round(scale);
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
    let rect = rect.to_physical_precise_round(scale);
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
        .map(|shader| Element::Shader(ShaderElement(shader)))
        .collect()
}

/// The layer surfaces of `layers` on output (with their popups), front first.
fn layer_elements<R>(
    output: &Output,
    renderer: &mut R,
    scale: Scale<f64>,
    layers: &[Layer],
    looks: &mut HashMap<WlSurface, Look>,
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
            let Some(rect) = map.layer_geometry(surface) else {
                continue;
            };
            // As the window manager has it look: its corners cut off, its
            // shadow behind it.
            let mut look = looks.get_mut(surface.wl_surface());
            let radius = look.as_ref().map_or(0, |l| l.radius);
            let ids = look.as_deref_mut().map(|l| &mut l.clip_ids);
            let corner = Point::default();
            let surface = surface.wl_surface();
            elements.extend(surface_elements(
                renderer, surface, corner, rect, scale, radius, ids,
            ));
            if let Some(shadow) = look.and_then(|l| l.shadow.as_mut()) {
                let at = Point::default();
                let shadow = shadow.element(renderer, rect, radius, at);
                elements.extend(shadow.map(Element::Shader));
            }
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
    for menu in state.x_menus.iter().chain(&state.x_docks) {
        menu.send_frame(output, now, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    // What the picker shows of windows on no monitor.
    for window in state.picker_windows(output) {
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
    state.popups.cleanup();
    // Windows on their way somewhere: drawn again (and once more when
    // they get there, where they're placed).
    if state.windows.animations_going(Instant::now()) {
        state.queue_redraw();
    }
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

    /// What it and the window inside it cover: content grown by its width,
    /// and how round that's corners are.
    pub fn outer(&self, content: Rectangle<i32, Logical>) -> (Rectangle<i32, Logical>, u32) {
        let w = self.width.max(0);
        let radius = if self.radius > 0 {
            self.radius + w as u32
        } else {
            0
        };
        let size = (content.size.w + 2 * w, content.size.h + 2 * w);
        (
            Rectangle::new(content.loc - Point::from((w, w)), size.into()),
            radius,
        )
    }

    /// It around content (in global coordinates), on an output whose top
    /// left is origin; None if it has no width (or can't be drawn).
    fn element<R: AsGles>(
        &mut self,
        renderer: &mut R,
        content: Rectangle<i32, Logical>,
        origin: Point<i32, Logical>,
    ) -> Option<ShaderElement> {
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
        self.shader.clone().map(ShaderElement)
    }
}

/// 0xAARRGGBB as smithay's colour: components 0 to 1, premultiplied.
pub fn argb(c: u32) -> Color32F {
    let part = |shift: u32| ((c >> shift) & 0xff) as f32 / 255.0;
    let a = part(24);
    Color32F::new(part(16) * a, part(8) * a, part(0) * a, a)
}
