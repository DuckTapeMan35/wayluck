//! Running nested, as a window in another session: for development. The
//! window is wayluck's one output.

use crate::{Wayluck, render};
use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{EventLoop, ping::make_ping},
    utils::Transform,
};
use std::{cell::RefCell, rc::Rc};

/// The refresh rate of the monitor the window is on, in mHz (60 Hz if
/// winit can't tell).
fn refresh_of(backend: &WinitGraphicsBackend<GlesRenderer>) -> i32 {
    backend
        .window()
        .current_monitor()
        .and_then(|m| m.refresh_rate_millihertz())
        .map_or(60_000, |r| r as i32)
}

pub fn init(
    event_loop: &mut EventLoop<Wayluck>,
    state: &mut Wayluck,
) -> Result<(), Box<dyn std::error::Error>> {
    let (backend, winit) = winit::init::<GlesRenderer>()?;
    // Shared with the ping below, which asks the window for a frame.
    let backend = Rc::new(RefCell::new(backend));

    // The window, as an output clients see (wl_output): its size is the
    // window's, and it follows when the window is resized.
    let mode = Mode {
        size: backend.borrow().window_size(),
        refresh: refresh_of(&backend.borrow()),
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

    // Frames are drawn when something changed (Wayluck::queue_redraw), not
    // all the time: a ping asks the window for one, however many asked.
    let (ping, asked) = make_ping()?;
    let window = backend.clone();
    event_loop.handle().insert_source(asked, move |_, _, _| {
        window.borrow().window().request_redraw();
    })?;
    state.redraw = Some(ping);
    state.queue_redraw();

    event_loop
        .handle()
        .insert_source(winit, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                let mode = Mode {
                    size,
                    refresh: refresh_of(&backend.borrow()),
                };
                if let Some(old) = output.current_mode() {
                    output.delete_mode(old);
                }
                output.change_current_state(Some(mode), None, None, None);
                output.set_preferred(mode);
                state.wm_output_changed(&output);
                state.queue_redraw();
            }
            WinitEvent::Redraw => {
                let mut backend = backend.borrow_mut();
                // How old the buffer we're about to draw into is: only what
                // changed since then is drawn again (0: unknown, all of it).
                let age = backend.buffer_age().unwrap_or(0);
                let rendered = match backend.bind() {
                    Ok((renderer, mut framebuffer)) => render::render_output(
                        state,
                        &output,
                        renderer,
                        &mut framebuffer,
                        &mut damage_tracker,
                        age,
                    ),
                    Err(e) => return tracing::warn!("binding the window: {e}"),
                };
                match rendered {
                    // Nothing changed after all: nothing to show.
                    Ok(None) => {}
                    Ok(Some(damage)) => {
                        if let Err(e) = backend.submit(Some(&damage)) {
                            tracing::warn!("showing the frame: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("rendering: {e}"),
                }
                // Clients waiting to draw again may now (no more frames are
                // asked for: the next comes when something changes).
                render::frame_done(state, &output);
            }
            WinitEvent::CloseRequested => state.loop_signal.stop(),
            WinitEvent::Input(event) => state.handle_input(event),
            WinitEvent::Focus(false) => state.release_all_keys(),
            _ => {}
        })?;
    Ok(())
}
