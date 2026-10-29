//! The pointer's image, where wayluck draws it itself: on hardware. Nested,
//! the host draws its own cursor, so this draws nothing. A client may show
//! its own image while the pointer is over it (a text beam, say); otherwise
//! it's the cursor theme's arrow.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportAll, ImportMem, Renderer, Texture,
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::render_elements_from_surface_tree,
            },
        },
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    utils::{Logical, Point, Scale, Transform},
    wayland::compositor::with_states,
};

use crate::render::Element;

pub struct Cursor {
    /// wayluck draws it (the hardware backend says so).
    pub drawn: bool,
    /// What the client under the pointer asked for, if anything.
    pub status: CursorImageStatus,
    /// The theme's arrow and its hotspot, loaded the first time it's needed.
    arrow: Option<(MemoryRenderBuffer, Point<i32, Logical>)>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            drawn: false,
            status: CursorImageStatus::default_named(),
            arrow: None,
        }
    }
}

impl Cursor {
    /// The cursor at `at` (on the output, from its top left), front first.
    pub fn elements<R>(
        &mut self,
        renderer: &mut R,
        at: Point<f64, Logical>,
        scale: Scale<f64>,
    ) -> Vec<Element<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Clone + Texture + Send + 'static,
    {
        if !self.drawn {
            return Vec::new();
        }
        match &self.status {
            CursorImageStatus::Hidden => Vec::new(),
            // The client's own image, its hotspot where the pointer is.
            CursorImageStatus::Surface(surface) => {
                let hotspot = with_states(surface, |s| {
                    s.data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|d| d.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let top_left = (at - hotspot.to_f64()).to_physical_precise_round(scale);
                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    top_left,
                    scale,
                    1.0,
                    Kind::Cursor,
                )
            }
            // Named cursors (a hand, a text beam) come later: the arrow for all.
            CursorImageStatus::Named(_) => {
                let (buffer, hotspot) = self.arrow.get_or_insert_with(load_arrow);
                let top_left = (at - hotspot.to_f64()).to_physical(scale);
                MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    top_left,
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .map(|e| vec![Element::Cursor(e)])
                .unwrap_or_default()
            }
        }
    }
}

/// The arrow from the cursor theme (XCURSOR_THEME, at XCURSOR_SIZE), or a
/// plain one drawn here if there's no theme to be found.
fn load_arrow() -> (MemoryRenderBuffer, Point<i32, Logical>) {
    let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
    let size: u32 = std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let themed = ["default", "left_ptr"]
        .iter()
        .find_map(|name| xcursor::CursorTheme::load(&theme).load_icon(name))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| xcursor::parser::parse_xcursor(&bytes))
        // The image whose size is nearest the one asked for.
        .and_then(|images| images.into_iter().min_by_key(|i| i.size.abs_diff(size)));
    let Some(image) = themed else {
        tracing::warn!("no cursor theme {theme:?}: a plain arrow instead");
        return plain_arrow();
    };
    // pixels_rgba is R, G, B, A in memory: what DRM calls ABGR8888.
    let buffer = MemoryRenderBuffer::from_slice(
        &image.pixels_rgba,
        Fourcc::Abgr8888,
        (image.width as i32, image.height as i32),
        1,
        Transform::Normal,
        None,
    );
    (buffer, (image.xhot as i32, image.yhot as i32).into())
}

/// A 12x18 white arrow, outlined in black: so there's always a pointer.
fn plain_arrow() -> (MemoryRenderBuffer, Point<i32, Logical>) {
    const W: usize = 12;
    const H: usize = 18;
    let mut pixels = vec![0u8; W * H * 4];
    for y in 0..H {
        // A wedge from its tip at the top left, widening as it goes down.
        let width = ((y + 1) * W).div_ceil(H);
        for x in 0..width {
            let edge = x == 0 || x + 1 == width || y + 1 == H;
            let v = if edge { 0 } else { 255 };
            pixels[(y * W + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]); // BGRA
        }
    }
    let buffer = MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Argb8888,
        (W as i32, H as i32),
        1,
        Transform::Normal,
        None,
    );
    (buffer, (0, 0).into())
}
