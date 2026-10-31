//! Rounded corners: drawn with GLES shaders of wayluck's own, whatever the
//! backend (nested, the window's GlesRenderer; on hardware, a MultiRenderer
//! around one).

use smithay::{
    backend::{
        drm::DrmDeviceFd,
        renderer::{
            ImportAll, Renderer, Texture,
            element::{
                Element, Id, Kind, RenderElement, UnderlyingStorage,
                surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture},
            },
            gles::{
                GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, Uniform,
                UniformName, UniformType, element::PixelShaderElement,
            },
            multigpu::{Error as MultiError, MultiFrame, MultiRenderer, gbm::GbmGlesBackend},
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
    },
    utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform},
};

/// A renderer that draws with GLES underneath: wayluck's shaders can draw
/// with it.
pub trait AsGles: Renderer {
    /// The GlesRenderer, to compile shaders with.
    fn gles(&mut self) -> &mut GlesRenderer;
    /// A frame of it as the GlesFrame underneath, to draw with them.
    fn gles_frame<'a, 'f, 'b>(frame: &'a mut Self::Frame<'f, 'b>) -> &'a mut GlesFrame<'f, 'b>
    where
        'b: 'f;
    /// A GLES error as one of its own.
    fn error(e: GlesError) -> Self::Error;
}

impl AsGles for GlesRenderer {
    fn gles(&mut self) -> &mut GlesRenderer {
        self
    }
    fn gles_frame<'a, 'f, 'b>(frame: &'a mut GlesFrame<'f, 'b>) -> &'a mut GlesFrame<'f, 'b>
    where
        'b: 'f,
    {
        frame
    }
    fn error(e: GlesError) -> GlesError {
        e
    }
}

/// The DRM backend's.
type Gbm = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;

impl AsGles for MultiRenderer<'_, '_, Gbm, Gbm> {
    fn gles(&mut self) -> &mut GlesRenderer {
        self.as_mut()
    }
    fn gles_frame<'a, 'f, 'b>(
        frame: &'a mut MultiFrame<'_, '_, 'f, 'b, Gbm, Gbm>,
    ) -> &'a mut GlesFrame<'f, 'b>
    where
        'b: 'f,
    {
        frame.as_mut()
    }
    fn error(e: GlesError) -> MultiError<Gbm, Gbm> {
        MultiError::Render(e)
    }
}

/// A border around a window with rounded corners: a ring, width pixels
/// wide, outside a rectangle whose corners are rounded radius pixels. Each
/// side its own colour, meeting the next across the corner, diagonally.
const BORDER: &str = include_str!("shaders/border.frag");

/// A window's surfaces with the corners of its rectangle cut off, rounded
/// (smithay's texture shader, plus that).
const CLIP: &str = include_str!("shaders/clip.frag");

/// wayluck's shaders, compiled once for each GL context.
#[derive(Clone)]
pub struct Shaders {
    pub border: GlesPixelProgram,
    pub clip: GlesTexProgram,
}

impl Shaders {
    /// renderer's (compiled now, if they weren't).
    pub fn get<R: AsGles>(renderer: &mut R) -> Option<Shaders> {
        let gles = renderer.gles();
        if let Some(shaders) = gles.egl_context().user_data().get::<Shaders>() {
            return Some(shaders.clone());
        }
        let shaders = match Self::compile(gles) {
            Ok(shaders) => shaders,
            Err(e) => {
                tracing::warn!("can't compile wayluck's shaders: {e}");
                return None;
            }
        };
        let kept = shaders.clone();
        gles.egl_context().user_data().insert_if_missing(|| kept);
        Some(shaders)
    }

    fn compile(gles: &mut GlesRenderer) -> Result<Shaders, GlesError> {
        let border = gles.compile_custom_pixel_shader(
            BORDER,
            &[
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("width", UniformType::_1f),
                UniformName::new("top", UniformType::_4f),
                UniformName::new("right", UniformType::_4f),
                UniformName::new("bottom", UniformType::_4f),
                UniformName::new("left", UniformType::_4f),
            ],
        )?;
        let clip = gles.compile_custom_texture_shader(
            CLIP,
            &[
                UniformName::new("to_window_scale", UniformType::_2f),
                UniformName::new("to_window_offset", UniformType::_2f),
                UniformName::new("window_size", UniformType::_2f),
                UniformName::new("radius", UniformType::_1f),
            ],
        )?;
        Ok(Shaders { border, clip })
    }
}

/// The uniforms for a border: radius and width in pixels, colours (top,
/// right, bottom, left) as premultiplied RGBA.
pub fn border_uniforms(radius: f32, width: f32, colors: [[f32; 4]; 4]) -> Vec<Uniform<'static>> {
    let [top, right, bottom, left] = colors;
    vec![
        Uniform::new("radius", radius),
        Uniform::new("width", width),
        Uniform::new("top", top),
        Uniform::new("right", right),
        Uniform::new("bottom", bottom),
        Uniform::new("left", left),
    ]
}

/// A border drawn by the shader: smithay's pixel shader element, for any
/// renderer that's GLES underneath.
#[derive(Clone)]
pub struct BorderElement(pub PixelShaderElement);

impl Element for BorderElement {
    fn id(&self) -> &Id {
        self.0.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.0.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.0.src()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.0.geometry(scale)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.0.opaque_regions(scale)
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.0.damage_since(scale, commit)
    }
    fn alpha(&self) -> f32 {
        self.0.alpha()
    }
    fn kind(&self) -> Kind {
        self.0.kind()
    }
}

impl<R: AsGles> RenderElement<R> for BorderElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        let frame = R::gles_frame(frame);
        RenderElement::<GlesRenderer>::draw(&self.0, frame, src, dst, damage, opaque_regions)
            .map_err(R::error)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// Where a border goes: around content, width pixels out.
pub fn border_area(content: Rectangle<i32, Logical>, width: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (content.loc.x - width, content.loc.y - width).into(),
        (content.size.w + 2 * width, content.size.h + 2 * width).into(),
    )
}

/// One of a window's surfaces (its own or a subsurface: not a popup), with
/// the corners of the window's rectangle cut off.
pub struct ClippedSurface<R: Renderer> {
    inner: WaylandSurfaceRenderElement<R>,
    program: GlesTexProgram,
    /// The scale it's drawn at, and there, the window's rectangle (where the
    /// surface is: the output's pixels) and its corners' radius.
    scale: Scale<f64>,
    window: Rectangle<i32, Physical>,
    radius: f64,
}

impl<R: Renderer + ImportAll> ClippedSurface<R> {
    pub fn new(
        inner: WaylandSurfaceRenderElement<R>,
        program: GlesTexProgram,
        scale: Scale<f64>,
        window: Rectangle<i32, Physical>,
        radius: f64,
    ) -> Self {
        Self {
            inner,
            program,
            scale,
            window,
            radius,
        }
    }

    /// The four corners cut off (squares radius wide), relative to the
    /// surface's top left.
    fn corners(&self, scale: Scale<f64>) -> [Rectangle<i32, Physical>; 4] {
        let at = self.inner.geometry(scale).loc;
        let w = self.window;
        let r = self.radius.ceil() as i32;
        let (x0, y0) = (w.loc.x - at.x, w.loc.y - at.y);
        let (x1, y1) = (x0 + w.size.w - r, y0 + w.size.h - r);
        [(x0, y0), (x1, y0), (x0, y1), (x1, y1)]
            .map(|(x, y)| Rectangle::new((x, y).into(), (r, r).into()))
    }
}

impl<R: Renderer + ImportAll> Element for ClippedSurface<R> {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    /// What the surface says is opaque, but not the corners: what's under
    /// them shows.
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let corners = self.corners(scale);
        (self.inner.opaque_regions(scale).into_iter())
            .flat_map(|r| r.subtract_rects(corners))
            .collect()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<R> RenderElement<R> for ClippedSurface<R>
where
    R: AsGles + ImportAll,
    R::TextureId: Texture + 'static,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        let tex = match self.inner.texture() {
            WaylandSurfaceTexture::Texture(texture) => texture.size(),
            WaylandSurfaceTexture::SolidColor(_) => Size::default(),
        };
        // A solid colour, or a rotated buffer: drawn as it is.
        if tex.is_empty() || self.inner.transform() != Transform::Normal || src.size.is_empty() {
            return self.inner.draw(frame, src, dst, damage, opaque_regions);
        }
        // It may be drawn elsewhere than its geometry, or scaled (captures):
        // the window's rectangle, and the radius, go with it.
        let at = self.inner.geometry(self.scale);
        let (sx, sy) = if at.size.is_empty() {
            (1.0, 1.0)
        } else {
            (
                dst.size.w as f64 / at.size.w as f64,
                dst.size.h as f64 / at.size.h as f64,
            )
        };
        let window_x = (self.window.loc.x - at.loc.x) as f64 * sx + dst.loc.x as f64;
        let window_y = (self.window.loc.y - at.loc.y) as f64 * sy + dst.loc.y as f64;
        let window_size = [
            (self.window.size.w as f64 * sx) as f32,
            (self.window.size.h as f64 * sy) as f32,
        ];
        let radius = (self.radius * sx.min(sy)) as f32;
        // Texture coordinates (0 to 1 across the buffer) to pixels: src is
        // drawn over dst.
        let (kx, ky) = (
            dst.size.w as f64 / src.size.w,
            dst.size.h as f64 / src.size.h,
        );
        let scale = [(tex.w as f64 * kx) as f32, (tex.h as f64 * ky) as f32];
        let offset = [
            (dst.loc.x as f64 - src.loc.x * kx - window_x) as f32,
            (dst.loc.y as f64 - src.loc.y * ky - window_y) as f32,
        ];
        let uniforms = vec![
            Uniform::new("to_window_scale", scale),
            Uniform::new("to_window_offset", offset),
            Uniform::new("window_size", window_size),
            Uniform::new("radius", radius),
        ];
        R::gles_frame(frame).override_default_tex_program(self.program.clone(), uniforms);
        let drawn = self.inner.draw(frame, src, dst, damage, opaque_regions);
        R::gles_frame(frame).clear_tex_program_override();
        drawn
    }

    /// None: put straight on a plane (scanned out), its corners wouldn't
    /// be cut.
    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
