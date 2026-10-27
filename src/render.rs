//! Drawing a frame, whatever the backend: winit now, DRM later.

use std::time::Duration;

use smithay::{
    backend::renderer::{
        ImportAll, Renderer, Texture,
        damage::{Error as DamageError, OutputDamageTracker},
        element::surface::WaylandSurfaceRenderElement,
    },
    output::Output,
    utils::{Physical, Rectangle},
};

use crate::Wayluck;

/// The parts of an output that changed, in its pixels; None: nothing did.
pub type Damage = Option<Vec<Rectangle<i32, Physical>>>;

/// The background, where no window is.
pub const CLEAR: [f32; 4] = [0.1, 0.1, 0.12, 1.0];

/// Draw output: every window shown on it, over the clear colour. Returns
/// what changed since the frame `age` frames ago (0: unknown, all of it).
pub fn render_output<R>(
    state: &Wayluck,
    output: &Output,
    renderer: &mut R,
    framebuffer: &mut R::Framebuffer<'_>,
    damage_tracker: &mut OutputDamageTracker,
    age: usize,
) -> Result<Damage, DamageError<R::Error>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + Texture + 'static,
{
    let result = smithay::desktop::space::render_output::<_, WaylandSurfaceRenderElement<R>, _, _>(
        output,
        renderer,
        framebuffer,
        1.0,
        age,
        [&state.space],
        &[],
        damage_tracker,
        CLEAR,
    )?;
    Ok(result.damage.cloned())
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
    state.space.refresh();
}
