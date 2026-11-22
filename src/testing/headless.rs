//! For tests only (the test-harness feature): monitors drawn offscreen,
//! with no window and no GPU needed (a software renderer will do). Nothing
//! is shown; captures see what's drawn. There's one to start with; tests
//! plug in more (add_monitor).

use crate::{Wayluck, capture::image_capture, capture::screencopy, render};
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
use std::{
    error::Error,
    sync::atomic::{AtomicU64, Ordering},
};

/// The first monitor's size, and every one's refresh rate (mHz).
const SIZE: (i32, i32) = (1280, 800);
const REFRESH: i32 = 60_000;

/// Modes every monitor can show too, besides its own.
const MODES: [(i32, i32); 2] = [(1920, 1080), (1024, 768)];

/// Each monitor drawn: into its buffer (of its size), with what it last showed.
type Drawn = Vec<(Output, (i32, i32), GlesRenderbuffer, OutputDamageTracker)>;

/// Frames drawn so far.
pub static FRAMES: AtomicU64 = AtomicU64::new(0);

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

    add_monitor(state, "HEADLESS-1", SIZE);

    // Any of a monitor's modes: its buffer is made again, at that size.
    state.set_mode = Some(Box::new(|output, mode| output.modes().contains(&mode)));

    // Frames are drawn when something changed (Wayluck::queue_redraw): on
    // every monitor, each into a buffer of its own, made the first time.
    let mut drawn: Drawn = Vec::new();
    let (ping, asked) = make_ping()?;
    event_loop
        .handle()
        .insert_source(asked, move |_, _, state| {
            FRAMES.fetch_add(1, Ordering::Relaxed);
            let monitors: Vec<Output> = state.monitors().cloned().collect();
            let size = |o: &Output| o.current_mode().map_or(SIZE, |m| (m.size.w, m.size.h));
            // One that changed mode gets a buffer of its new size.
            drawn.retain(|(output, at, ..)| monitors.contains(output) && *at == size(output));
            for output in monitors {
                if !drawn.iter().any(|(o, ..)| *o == output) {
                    let size = size(&output);
                    match renderer.create_buffer(Fourcc::Xrgb8888, size.into()) {
                        Ok(buffer) => {
                            let damage = OutputDamageTracker::from_output(&output);
                            drawn.push((output.clone(), size, buffer, damage));
                        }
                        Err(e) => {
                            tracing::warn!("a buffer for {}: {e}", output.name());
                            continue;
                        }
                    }
                }
                let (_, _, buffer, damage) = drawn.iter_mut().find(|(o, ..)| *o == output).unwrap();
                let rendered = match renderer.bind(buffer) {
                    Ok(mut framebuffer) => render::render_output(
                        state,
                        &output,
                        &mut renderer,
                        &mut framebuffer,
                        damage,
                        1,
                    ),
                    Err(e) => {
                        tracing::warn!("binding the offscreen buffer: {e}");
                        continue;
                    }
                };
                let changed = matches!(rendered, Ok(Some(_)));
                if let Err(e) = rendered {
                    tracing::warn!("rendering: {e}");
                }
                // Screenshots waiting for this frame.
                screencopy::capture(state, &output, &mut renderer, changed);
                render::frame_done(state, &output);
            }
            // Screen sharing waiting for this frame.
            screencopy::capture_virtual(state, &mut renderer);
            image_capture::capture_frames(state, &mut renderer);
        })?;
    state.redraw = Some(ping);
    state.queue_redraw();
    Ok(())
}

/// The monitor called name unplugged.
pub fn remove_monitor(state: &mut Wayluck, name: &str) -> bool {
    let output = (state.monitors.iter())
        .map(|m| &m.output)
        .find(|o| o.name() == name)
        .cloned();
    if let Some(output) = &output {
        state.remove_monitor(output);
    }
    output.is_some()
}

/// A monitor plugged in, called name, size pixels wide and high.
pub fn add_monitor(state: &mut Wayluck, name: &str, size: (i32, i32)) {
    let output = Output::new(
        name.to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "wayluck".into(),
            model: "headless".into(),
            serial_number: String::new(),
        },
    );
    for other in MODES {
        output.add_mode(Mode {
            size: other.into(),
            refresh: REFRESH,
        });
    }
    let mode = Mode {
        size: size.into(),
        refresh: REFRESH,
    };
    state.add_monitor(output, mode, Transform::Normal);
}
