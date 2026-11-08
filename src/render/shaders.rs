//! wayluck's own GLES shaders, and drawing with them whatever the backend
//! (nested, the window's GlesRenderer; on hardware, a MultiRenderer around
//! one).

use smithay::{
    backend::{
        drm::DrmDeviceFd,
        renderer::{
            Renderer,
            element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
            gles::{
                GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, UniformName,
                UniformType, element::PixelShaderElement,
            },
            multigpu::{Error as MultiError, MultiFrame, MultiRenderer, gbm::GbmGlesBackend},
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
    },
    utils::{Buffer, Physical, Rectangle, Scale, user_data::UserDataMap},
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

/// A shadow: the blur of a rectangle with rounded corners, inside the
/// element as far as the blur reaches; not drawn under what casts it.
const SHADOW: &str = include_str!("shaders/shadow.frag");

/// wayluck's shaders, compiled once for each GL context.
#[derive(Clone)]
pub struct Shaders {
    pub border: GlesPixelProgram,
    pub clip: GlesTexProgram,
    pub shadow: GlesPixelProgram,
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
                UniformName::new("beyond", UniformType::_4f),
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
        let shadow = gles.compile_custom_pixel_shader(
            SHADOW,
            &[
                UniformName::new("blur", UniformType::_1f),
                UniformName::new("spread", UniformType::_1f),
                UniformName::new("offset", UniformType::_2f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("color", UniformType::_4f),
            ],
        )?;
        Ok(Shaders {
            border,
            clip,
            shadow,
        })
    }
}

/// Something one of wayluck's pixel shaders draws (a border, an outline, a
/// shadow): smithay's pixel shader element, for any renderer that's GLES
/// underneath.
#[derive(Clone)]
pub struct ShaderElement(pub PixelShaderElement);

impl Element for ShaderElement {
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

impl<R: AsGles> RenderElement<R> for ShaderElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        let frame = R::gles_frame(frame);
        RenderElement::<GlesRenderer>::draw(&self.0, frame, src, dst, damage, opaque_regions, cache)
            .map_err(R::error)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}
