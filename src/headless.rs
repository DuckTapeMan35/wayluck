//! For tests only (the test-harness feature): one output, drawn offscreen,
//! with no window and no GPU needed (a software renderer will do). Nothing
//! is shown; captures see what's drawn.

use crate::{Wayluck, image_capture, render, screencopy};
use smithay::{
    backend::{
        allocator::Fourcc,
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{
            Bind, Offscreen,
            damage::OutputDamageTracker,
            gles::{GlesRenderbuffer, GlesRenderer},
        },
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{EventLoop, ping::make_ping},
    utils::Transform,
};
use std::{cell::RefCell, error::Error, rc::Rc};

/// Its size, and refresh rate (mHz).
const SIZE: (i32, i32) = (1280, 800);
const REFRESH: i32 = 60_000;

pub fn init(
    event_loop: &mut EventLoop<Wayluck>,
    state: &mut Wayluck,
) -> Result<(), Box<dyn Error>> {
    // A software renderer (llvmpipe) if there is one: tests run anywhere.
    let devices: Vec<EGLDevice> = EGLDevice::enumerate()?.collect();
    let device = (devices.iter())
        .find(|d| d.is_software())
        .or(devices.first())
        .cloned()
        .ok_or("no EGL device")?;
    // SAFETY: the display and context live as long as the renderer, which
    // keeps them.
    let display = unsafe { EGLDisplay::new(device)? };
    let context = EGLContext::new(&display)?;
    let mut renderer = unsafe { GlesRenderer::new(context)? };

    let mode = Mode {
        size: SIZE.into(),
        refresh: REFRESH,
    };
    let output = Output::new(
        "HEADLESS-1".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "wayluck".into(),
            model: "headless".into(),
        },
    );
    let _global = output.create_global::<Wayluck>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));

    // What's drawn goes here, and nowhere else.
    let buffer: GlesRenderbuffer = renderer.create_buffer(Fourcc::Xrgb8888, SIZE.into())?;
    let drawn = Rc::new(RefCell::new((
        renderer,
        buffer,
        OutputDamageTracker::from_output(&output),
    )));

    // Frames are drawn when something changed (Wayluck::queue_redraw).
    let (ping, asked) = make_ping()?;
    event_loop
        .handle()
        .insert_source(asked, move |_, _, state| {
            let mut drawn = drawn.borrow_mut();
            let (renderer, buffer, damage) = &mut *drawn;
            let rendered = match renderer.bind(buffer) {
                Ok(mut framebuffer) => {
                    render::render_output(state, &output, renderer, &mut framebuffer, damage, 1)
                }
                Err(e) => return tracing::warn!("binding the offscreen buffer: {e}"),
            };
            let changed = matches!(rendered, Ok(Some(_)));
            if let Err(e) = rendered {
                tracing::warn!("rendering: {e}");
            }
            // Screenshots and screen sharing waiting for this frame.
            screencopy::capture(state, &output, renderer, changed);
            screencopy::capture_virtual(state, renderer);
            image_capture::capture_frames(state, renderer);
            render::frame_done(state, &output);
        })?;
    state.redraw = Some(ping);
    state.queue_redraw();
    Ok(())
}
