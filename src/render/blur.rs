//! Blur behind windows: what's below one, blurred, drawn behind it.

use super::{
    CLEAR,
    shaders::{AsGles, Shaders},
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Color32F, Frame, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker,
            element::{
                Element, Id, Kind, RenderElement, UnderlyingStorage,
                utils::{Relocate, RelocateRenderElement},
            },
            gles::{GlesError, GlesRenderer, GlesTexProgram, GlesTexture, Uniform},
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform, user_data::UserDataMap},
};

/// Where a window is blurred behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Where its client asks (ext-background-effect), if it does.
    #[default]
    Client,
    /// Nowhere, whatever its client asks.
    Off,
    /// Behind all of it.
    On,
}

/// What of a window is blurred behind: all of it, or the parts its client
/// asked for (from its top left, in the output's pixels).
#[derive(Clone, PartialEq, Debug)]
pub enum Part {
    Whole,
    Only(Vec<Rectangle<i32, Physical>>),
}

/// How strongly: halved passes times, then doubled back as many, sampled
/// offset pixels apart each time.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Strength {
    pub passes: u32,
    pub offset: f32,
}

impl Strength {
    /// None unless 1 to 8 passes and an offset above 0.
    pub fn new(passes: u32, offset: f32) -> Option<Self> {
        ((1..=8).contains(&passes) && offset > 0.0).then_some(Self { passes, offset })
    }

    /// How far a pixel's blur reaches, in pixels: what's below a window is
    /// captured that far around it, so its edges blur what's really there.
    pub fn reach(&self) -> i32 {
        (self.offset * (1 << (self.passes + 1)) as f32).ceil() as i32
    }
}

impl Default for Strength {
    fn default() -> Self {
        Self {
            passes: 3,
            offset: 2.0,
        }
    }
}

/// The textures one window's blur goes through, kept from frame to frame:
/// made again when its size or the strength changes.
pub struct Passes {
    strength: Strength,
    /// What's below, as captured (0), then halved by each pass down.
    down: Vec<GlesTexture>,
    /// The capture blurred: as big as it.
    result: GlesTexture,
}

impl Passes {
    /// For a capture size pixels big, blurred as strength says.
    pub fn new(
        gles: &mut GlesRenderer,
        size: Size<i32, Physical>,
        strength: Strength,
    ) -> Result<Self, GlesError> {
        let mut texture = |k: u32| {
            let size = (1.max(size.w >> k), 1.max(size.h >> k));
            Offscreen::<GlesTexture>::create_buffer(gles, Fourcc::Abgr8888, size.into())
        };
        let down = (0..=strength.passes)
            .map(&mut texture)
            .collect::<Result<_, _>>()?;
        let result = texture(0)?;
        Ok(Self {
            strength,
            down,
            result,
        })
    }

    /// Whether these are for a capture size big, blurred as strength says.
    pub fn fits(&self, size: Size<i32, Physical>, strength: Strength) -> bool {
        let at = self.down[0].size();
        self.strength == strength && (at.w, at.h) == (size.w, size.h)
    }

    /// Where what's below is captured.
    pub fn capture(&mut self) -> &mut GlesTexture {
        &mut self.down[0]
    }

    /// The capture blurred into the result: halved each pass down, then
    /// doubled back up.
    pub fn run(&mut self, gles: &mut GlesRenderer) -> Result<GlesTexture, GlesError> {
        let shaders = Shaders::get(gles).ok_or(GlesError::ShaderCompileError)?;
        let offset = self.strength.offset;
        let n = self.down.len() - 1;
        for k in 1..=n {
            let from = self.down[k - 1].clone();
            pass(gles, &shaders.blur_down, &from, &mut self.down[k], offset)?;
        }
        // Up into the textures on the way down (no longer needed), the last
        // into the result: the capture stays as it is, for next time.
        for k in (1..=n).rev() {
            let from = self.down[k].clone();
            let to = if k == 1 {
                &mut self.result
            } else {
                &mut self.down[k - 1]
            };
            pass(gles, &shaders.blur_up, &from, to, offset)?;
        }
        Ok(self.result.clone())
    }
}

/// Draw from into the whole of to with program (one of blur's passes).
fn pass(
    gles: &mut GlesRenderer,
    program: &GlesTexProgram,
    from: &GlesTexture,
    to: &mut GlesTexture,
    offset: f32,
) -> Result<(), GlesError> {
    let size = to.size();
    let whole = Rectangle::from_size(Size::<i32, Physical>::from((size.w, size.h)));
    let src = Rectangle::from_size(from.size()).to_f64();
    let halfpixel = [0.5 / size.w as f32, 0.5 / size.h as f32];
    let from_size = from.size();
    let edge = [0.5 / from_size.w as f32, 0.5 / from_size.h as f32];
    let uniforms = [
        Uniform::new("halfpixel", halfpixel),
        Uniform::new("offset", offset),
        Uniform::new("edge", edge),
    ];
    let mut framebuffer = gles.bind(to)?;
    let mut frame = gles.render(&mut framebuffer, whole.size, Transform::Normal)?;
    frame.clear(Color32F::TRANSPARENT, &[whole])?;
    frame.render_texture_from_to(
        from,
        src,
        whole,
        &[whole],
        &[],
        Transform::Normal,
        1.0,
        Some(program),
        &uniforms,
    )?;
    // Drawn in order in the one GL context: the next pass, or the frame,
    // sees it done without waiting on this.
    let _ = frame.finish()?;
    Ok(())
}

/// A window's blur, kept from frame to frame: what's below it is blurred
/// again only when it changed.
pub struct Behind {
    passes: Option<Passes>,
    /// What of what's below was in the capture last time.
    tracker: Option<OutputDamageTracker>,
    /// The last blur, and when it changed (for the damage tracker drawing it).
    blurred: Option<GlesTexture>,
    /// What of the window it was drawn behind last time.
    part: Option<Part>,
    id: Id,
    commit: CommitCounter,
}

impl Default for Behind {
    fn default() -> Self {
        Self {
            passes: None,
            tracker: None,
            blurred: None,
            part: None,
            id: Id::new(),
            commit: CommitCounter::default(),
        }
    }
}

impl Behind {
    /// Its element, behind part of the window at `window` (in the output's pixels,
    /// output_size big; its corners rounded radius pixels), blurring below:
    /// what's drawn under it, front to back. None if it can't be drawn.
    #[allow(clippy::too_many_arguments)]
    pub fn element<R, E>(
        &mut self,
        renderer: &mut R,
        below: &[E],
        window: Rectangle<i32, Physical>,
        part: Part,
        output_size: Size<i32, Physical>,
        radius: f64,
        strength: Strength,
        scale: Scale<f64>,
    ) -> Option<BlurElement>
    where
        R: AsGles,
        R::TextureId: Texture,
        E: RenderElement<R>,
    {
        // What's below it, and around it as far as the blur reaches (on the
        // output: past it, there's nothing to blur).
        let reach = strength.reach();
        let around = Rectangle::new(
            window.loc - Point::from((reach, reach)),
            (window.size.w + 2 * reach, window.size.h + 2 * reach).into(),
        );
        let area = around.intersection(Rectangle::from_size(output_size))?;
        let fits = (self.passes.as_ref()).is_some_and(|p| p.fits(area.size, strength));
        let mut age = 1;
        if !fits {
            self.passes = Passes::new(renderer.gles(), area.size, strength)
                .inspect_err(|e| tracing::warn!("can't blur: {e}"))
                .ok();
            self.tracker = Some(OutputDamageTracker::new(
                area.size,
                scale,
                Transform::Normal,
            ));
            self.blurred = None;
            age = 0;
        }
        let passes = self.passes.as_mut()?;
        let tracker = self.tracker.as_mut()?;

        // Captured as the output shows it, from the area's top left.
        let below: Vec<_> = (below.iter())
            .map(|e| {
                RelocateRenderElement::from_element(e, area.loc.upscale(-1), Relocate::Relative)
            })
            .collect();
        let changed = {
            let mut framebuffer = renderer.bind(passes.capture()).ok()?;
            let rendered = tracker.render_output(renderer, &mut framebuffer, age, &below, CLEAR);
            match rendered {
                Ok(result) => result.damage.is_some_and(|d| !d.is_empty()),
                Err(e) => {
                    tracing::warn!("can't capture what's below to blur: {e:?}");
                    return None;
                }
            }
        };
        if changed || self.blurred.is_none() {
            let blurred = passes.run(renderer.gles());
            self.blurred = blurred
                .inspect_err(|e| tracing::warn!("can't blur: {e}"))
                .ok();
            self.commit.increment();
        }
        if self.part.as_ref() != Some(&part) {
            self.part = Some(part.clone());
            self.commit.increment();
        }
        let program = Shaders::get(renderer)?.clip;
        Some(BlurElement {
            id: self.id.clone(),
            commit: self.commit,
            texture: self.blurred.clone()?,
            area: window,
            src: window_in(area, window),
            part,
            radius,
            program,
        })
    }
}

/// window's part of area (both in the output's pixels), in a texture
/// captured from area.
fn window_in(
    area: Rectangle<i32, Physical>,
    window: Rectangle<i32, Physical>,
) -> Rectangle<f64, Buffer> {
    let at = window.loc - area.loc;
    let (w, h) = (window.size.w as f64, window.size.h as f64);
    Rectangle::new((at.x as f64, at.y as f64).into(), (w, h).into())
}

/// What's below a window, blurred, drawn behind it (inside its rounded
/// corners).
pub struct BlurElement {
    id: Id,
    commit: CommitCounter,
    texture: GlesTexture,
    /// Where it's drawn: the window, in the output's pixels.
    area: Rectangle<i32, Physical>,
    /// The part of the texture drawn there.
    src: Rectangle<f64, Buffer>,
    /// What of the window it's drawn behind.
    part: Part,
    /// How round the window's corners are, in the output's pixels.
    radius: f64,
    /// The clip shader: cuts off its corners.
    program: GlesTexProgram,
}

impl Element for BlurElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }
    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.area
    }
    fn damage_since(
        &self,
        _scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else {
            DamageSet::from_slice(&[Rectangle::from_size(self.area.size)])
        }
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: AsGles> RenderElement<R> for BlurElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        // The clip shader's: from the texture's coordinates to pixels where
        // it's drawn (the window's rectangle there), and its corners.
        let tex = self.texture.size();
        let (kx, ky) = (
            dst.size.w as f64 / src.size.w,
            dst.size.h as f64 / src.size.h,
        );
        let scale = [(tex.w as f64 * kx) as f32, (tex.h as f64 * ky) as f32];
        let offset = [(-src.loc.x * kx) as f32, (-src.loc.y * ky) as f32];
        let window_size = [dst.size.w as f32, dst.size.h as f32];
        let shrunk = (dst.size.w as f64 / self.area.size.w.max(1) as f64)
            .min(dst.size.h as f64 / self.area.size.h.max(1) as f64);
        let uniforms = [
            Uniform::new("to_window_scale", scale),
            Uniform::new("to_window_offset", offset),
            Uniform::new("window_size", window_size),
            Uniform::new("radius", (self.radius * shrunk) as f32),
        ];
        // Only behind the parts asked for, where it's drawn.
        let damage = match &self.part {
            Part::Whole => damage.to_vec(),
            Part::Only(parts) => {
                let (sx, sy) = (
                    dst.size.w as f64 / self.area.size.w.max(1) as f64,
                    dst.size.h as f64 / self.area.size.h.max(1) as f64,
                );
                let parts: Vec<_> = (parts.iter())
                    .map(|r| r.to_f64().upscale((sx, sy)).to_i32_round())
                    .collect();
                (damage.iter())
                    .flat_map(|d| parts.iter().filter_map(|p| d.intersection(*p)))
                    .collect()
            }
        };
        if damage.is_empty() {
            return Ok(());
        }
        let frame = R::gles_frame(frame);
        frame
            .render_texture_from_to(
                &self.texture,
                src,
                dst,
                &damage,
                opaque_regions,
                Transform::Normal,
                1.0,
                Some(&self.program),
                &uniforms,
            )
            .map_err(R::error)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
