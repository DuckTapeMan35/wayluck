//! Running nested, as a window in another session: for development. The
//! window is wayluck's one output.

use crate::{Wayluck, render};
use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::Transform,
};

pub fn init(
    event_loop: &mut EventLoop<Wayluck>,
    state: &mut Wayluck,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;

    // The window, as an output clients see (wl_output): its size is the
    // window's, and it follows when the window is resized.
    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    let output = Output::new(
        "WINIT-1".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "wayluck".into(),
            model: "winit".into(),
        },
    );
    let _global = output.create_global::<Wayluck>(&state.display_handle);
    // Flipped180: what winit's GL surface needs to come out the right way up.
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    // The output's place in the space: windows are drawn where they overlap it.
    state.space.map_output(&output, (0, 0));

    // Redraws only what changed since the last frame (nothing yet but the clear).
    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    event_loop
        .handle()
        .insert_source(winit, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                let mode = Mode {
                    size,
                    refresh: 60_000,
                };
                if let Some(old) = output.current_mode() {
                    output.delete_mode(old);
                }
                output.change_current_state(Some(mode), None, None, None);
                output.set_preferred(mode);
                state.wm_output_changed(&output);
            }
            WinitEvent::Redraw => {
                let rendered = match backend.bind() {
                    Ok((renderer, mut framebuffer)) => render::render_output(
                        state,
                        &output,
                        renderer,
                        &mut framebuffer,
                        &mut damage_tracker,
                        0,
                    ),
                    Err(e) => return tracing::warn!("binding the window: {e}"),
                };
                match rendered {
                    Ok(damage) => {
                        if let Err(e) = backend.submit(damage.as_deref()) {
                            tracing::warn!("showing the frame: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("rendering: {e}"),
                }
                render::frame_done(state, &output);
                backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            WinitEvent::Input(event) => state.handle_input(event),
            WinitEvent::Focus(false) => state.release_all_keys(),
            _ => {}
        })?;
    Ok(())
}
