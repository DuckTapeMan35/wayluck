//! Running nested, as a window in another session: for development. The
//! window is wayluck's one output.

use crate::{
    Wayluck,
    image_capture::{self, DmabufTarget},
    render, screencopy,
};
use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{ImportDma, damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{EventLoop, ping::make_ping},
    utils::Transform,
    wayland::dmabuf::DmabufFeedbackBuilder,
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

    // What changed since the last frame drawn, for screen sharing: the
    // window's damage is since its buffer's age (all of it, when that's
    // unknown, as after a frame that wasn't shown).
    let mut changes = OutputDamageTracker::from_output(&output);

    dmabuf(state, &backend);

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
                // Bars stretch to the new size; windows get what they leave.
                layer_map_for_output(&output).arrange();
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

                // Did anything change since the last frame? (Not the damage
                // drawn above: that's since the buffer's age.)
                let elements = render::elements(state, &output, backend.renderer());
                let changed = matches!(changes.damage_output(1, &elements),
                    Ok((Some(d), _)) if !d.is_empty());
                // Screenshots and screen sharing waiting for this frame.
                screencopy::capture(state, &output, backend.renderer(), changed);
                screencopy::capture_virtual(state, backend.renderer());
                image_capture::capture_frames(state, backend.renderer());
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

/// linux-dmabuf: the formats the renderer can import, and the importing.
/// Version 4 tells clients which GPU wayluck renders on, if EGL can tell;
/// version 3 otherwise.
fn dmabuf(state: &mut Wayluck, backend: &Rc<RefCell<WinitGraphicsBackend<GlesRenderer>>>) {
    let mut b = backend.borrow_mut();
    let formats = b.renderer().dmabuf_formats();
    let render_node = EGLDevice::device_for_display(b.renderer().egl_context().display())
        .ok()
        .and_then(|device| device.try_get_render_node().ok().flatten());
    let feedback = render_node.and_then(|node| {
        DmabufFeedbackBuilder::new(node.dev_id(), formats.clone())
            .build()
            .ok()
    });
    // Captures may be dmabufs on its GPU too, if it's known.
    let render_formats = b.renderer().egl_context().dmabuf_render_formats();
    state.image_capture.dmabuf =
        render_node.map(|node| DmabufTarget::new(node.dev_id(), render_formats));
    let dh = state.display_handle.clone();
    match feedback {
        Some(feedback) => {
            tracing::info!("linux-dmabuf v4, on {:?}", render_node);
            state
                .dmabuf_state
                .create_global_with_default_feedback::<Wayluck>(&dh, &feedback);
        }
        None => {
            tracing::info!("linux-dmabuf v3 (no render node)");
            state.dmabuf_state.create_global::<Wayluck>(&dh, formats);
        }
    }
    let renderer = backend.clone();
    state.import_dmabuf = Some(Box::new(move |dmabuf| {
        renderer
            .borrow_mut()
            .renderer()
            .import_dmabuf(dmabuf, None)
            .is_ok()
    }));
}
