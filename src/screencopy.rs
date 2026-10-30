//! wlr-screencopy: screenshots (grim) and screen sharing (xdg-desktop-
//! portal-wlr). A client asks for a frame of an output, or of part of one,
//! and gets told the buffer it must give: shared memory, XRGB8888, its
//! size. When it gives one, the next frame drawn (for copy_with_damage: the
//! next one in which something changed) is drawn again, offscreen, and
//! copied into it.

use crate::{
    Wayluck,
    render::{self, Element},
};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ExportMem, ImportAll, ImportMem, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker, gles::GlesRenderbuffer,
        },
    },
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            protocol::{wl_buffer::WlBuffer, wl_shm},
        },
    },
    utils::{Buffer, Clock, Monotonic, Physical, Rectangle, Size, Transform},
    wayland::shm::{with_buffer_contents, with_buffer_contents_mut},
};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

const VERSION: u32 = 3;

/// How long without a frame asked for (and none waiting) before an output
/// counts as no longer captured.
const CAPTURE_GONE: Duration = Duration::from_secs(1);

/// A frame a client asked for: of which output, which part (in its
/// pixels), and whether with the cursor.
pub struct FrameData {
    output: Output,
    region: Rectangle<i32, Physical>,
    cursor: bool,
    /// copy was asked for already: a frame copies once.
    used: Mutex<bool>,
}

/// A copy waiting for the output's next frame.
pub struct Copy {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    /// Only once something changed (copy_with_damage).
    with_damage: bool,
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for Wayluck {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_manager_v1::Request;
        let (frame, cursor, output, logical) = match request {
            Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => (frame, overlay_cursor, output, None),
            Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => (
                frame,
                overlay_cursor,
                output,
                Some(Rectangle::new((x, y).into(), (width, height).into())),
            ),
            Request::Destroy => return,
            _ => return,
        };
        let output = Output::from_resource(&output);
        let region = output.as_ref().and_then(|o| region_of(o, logical));
        let (Some(output), Some(region)) = (output, region) else {
            // An output that's gone, or a region outside it.
            let frame = data_init.init(frame, None);
            frame.failed();
            return;
        };
        let frame = data_init.init(
            frame,
            Some(FrameData {
                output,
                region,
                cursor: cursor != 0,
                used: Mutex::new(false),
            }),
        );
        let size = region.size;
        frame.buffer(
            wl_shm::Format::Xrgb8888,
            size.w as u32,
            size.h as u32,
            size.w as u32 * 4,
        );
        if frame.version() >= 3 {
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, Option<FrameData>> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &Option<FrameData>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_frame_v1::Request;
        let (buffer, with_damage) = match request {
            Request::Copy { buffer } => (buffer, false),
            Request::CopyWithDamage { buffer } => (buffer, true),
            _ => return,
        };
        let Some(data) = data else {
            return; // failed already
        };
        if std::mem::replace(&mut *data.used.lock().unwrap(), true) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "copied already",
            );
            return;
        }
        if !fits(&buffer, data.region.size) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::InvalidBuffer,
                "not the buffer asked for",
            );
            return;
        }
        state.screencopies.push(Copy {
            frame: frame.clone(),
            buffer,
            with_damage,
        });
        state.capture_activity(&data.output);
        // A plain copy doesn't wait for something to change.
        if !with_damage {
            state.queue_redraw();
        }
    }
}

/// The part of output a client asked for, in the output's pixels: all of
/// it, or a region (logical, from its top left), cut to the output. None:
/// nothing of it is on the output.
fn region_of(
    output: &Output,
    logical: Option<Rectangle<i32, smithay::utils::Logical>>,
) -> Option<Rectangle<i32, Physical>> {
    let size = output.current_mode()?.size;
    let all = Rectangle::from_size(size);
    let Some(logical) = logical else {
        return Some(all);
    };
    let scale = output.current_scale().fractional_scale();
    let region = logical.to_f64().to_physical(scale).to_i32_round();
    region.intersection(all).filter(|r| !r.is_empty())
}

/// Is buffer the one a frame of size asked for: shared memory, XRGB8888,
/// that size, rows of 4 bytes a pixel?
pub fn fits(buffer: &WlBuffer, size: Size<i32, Physical>) -> bool {
    with_buffer_contents(buffer, |_, _, b| {
        b.format == wl_shm::Format::Xrgb8888
            && b.width == size.w
            && b.height == size.h
            && b.stride == size.w * 4
    })
    .unwrap_or(false)
}

/// After an output's frame was drawn (changed: something on it did): copy
/// it for the clients waiting on it.
pub fn capture<R>(state: &mut Wayluck, output: &Output, renderer: &mut R, changed: bool)
where
    R: Renderer + ImportAll + ImportMem + Offscreen<GlesRenderbuffer> + ExportMem,
    R::TextureId: Clone + Texture + Send + 'static,
    R::Error: 'static,
{
    let mine = |c: &Copy| c.is_of(output) && (changed || !c.with_damage);
    let (now, later) = std::mem::take(&mut state.screencopies)
        .into_iter()
        .partition(mine);
    state.screencopies = later;
    if now.is_empty() {
        return;
    }
    let elements = render::elements(state, output, renderer);
    for copy in now {
        let Some(Some(data)) = copy.frame.data::<Option<FrameData>>() else {
            continue;
        };
        // The cursor only if asked for (where wayluck draws it at all).
        let drawn: Vec<&Element<R>> = elements
            .iter()
            .filter(|e| data.cursor || !matches!(e, Element::Cursor(_)))
            .collect();
        let Some(size) = output.current_mode().map(|m| m.size) else {
            copy.frame.failed();
            continue;
        };
        let scale = output.current_scale().fractional_scale();
        match copy_into(renderer, size, scale, &drawn, data.region, &copy.buffer) {
            Ok(()) => {
                copy.frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
                if copy.with_damage {
                    let size = data.region.size;
                    copy.frame.damage(0, 0, size.w as u32, size.h as u32);
                }
                let now = Duration::from(Clock::<Monotonic>::new().now());
                let secs = now.as_secs();
                copy.frame
                    .ready((secs >> 32) as u32, secs as u32, now.subsec_nanos());
            }
            Err(e) => {
                tracing::warn!("screencopy: {e}");
                copy.frame.failed();
            }
        }
    }
}

/// After a frame was drawn (on any output): copies of virtual outputs. A
/// virtual output is drawn for nothing else (virtual_output.rs); for
/// copy_with_damage, only once something in it changed since its last.
pub fn capture_virtual<R>(state: &mut Wayluck, renderer: &mut R)
where
    R: Renderer + ImportAll + ImportMem + Offscreen<GlesRenderbuffer> + ExportMem,
    R::TextureId: Clone + Texture + Send + 'static,
    R::Error: 'static,
{
    let wanted: Vec<Output> = (state.virtuals.iter())
        .map(|v| v.output.clone())
        .filter(|o| state.screencopies.iter().any(|c| c.is_of(o)))
        .collect();
    for output in wanted {
        let elements = render::elements(state, &output, renderer);
        let Some(v) = state.virtuals.iter_mut().find(|v| v.output == output) else {
            continue;
        };
        let changed =
            matches!(v.damage.damage_output(1, &elements), Ok((Some(d), _)) if !d.is_empty());
        capture(state, &output, renderer, changed);
    }
}

impl Copy {
    /// Is it a copy of output?
    fn is_of(&self, output: &Output) -> bool {
        let data = self
            .frame
            .data::<Option<FrameData>>()
            .and_then(Option::as_ref);
        data.is_some_and(|d| &d.output == output)
    }
}

impl Wayluck {
    /// A frame of output was asked for: if nothing was capturing it, the
    /// window manager hears it is now, and a virtual output's windows are
    /// paced (virtual_output.rs). Captures are watched for stopping.
    pub fn capture_activity(&mut self, output: &Output) {
        let now = Instant::now();
        if let Some((_, last)) = self.captures.iter_mut().find(|(o, _)| o == output) {
            *last = now;
            return;
        }
        if self.captures.is_empty() {
            // Every quarter of a second, while anything is captured.
            let every = Duration::from_millis(250);
            let watch =
                self.loop_handle
                    .insert_source(Timer::from_duration(every), move |_, _, state| {
                        state.captures_stopped();
                        if state.captures.is_empty() {
                            TimeoutAction::Drop
                        } else {
                            TimeoutAction::ToDuration(every)
                        }
                    });
            if let Err(e) = watch {
                tracing::warn!("can't watch captures: {e}");
            }
        }
        self.captures.push((output.clone(), now));
        tracing::debug!("{}: captured", output.name());
        self.wm_capture(output, true);
        self.start_pacing(output);
    }

    pub fn screencopy_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, ZwlrScreencopyManagerV1, _>(VERSION, ());
    }

    /// Outputs no frame was asked for in CAPTURE_GONE, with none waiting:
    /// not captured any more.
    fn captures_stopped(&mut self) {
        let now = Instant::now();
        let captures = std::mem::take(&mut self.captures);
        // A frame waiting for something to change: still capturing.
        let waiting = |o: &Output| {
            self.screencopies.iter().any(|c| c.is_of(o)) || self.image_frames_waiting(o)
        };
        let (stopped, still): (Vec<_>, Vec<_>) =
            (captures.into_iter()).partition(|(o, last)| now - *last > CAPTURE_GONE && !waiting(o));
        self.captures = still;
        for (output, _) in stopped {
            tracing::debug!("{}: not captured", output.name());
            self.wm_capture(&output, false);
            self.stop_pacing(&output);
        }
    }

    /// output is gone: the copies waiting for it fail.
    pub fn fail_screencopies(&mut self, output: &Output) {
        let (gone, kept) = std::mem::take(&mut self.screencopies)
            .into_iter()
            .partition(|c: &Copy| c.is_of(output));
        self.screencopies = kept;
        for copy in gone {
            copy.frame.failed();
        }
    }
}

/// Draw elements offscreen in a picture of size (at scale),
/// and copy region of it into buffer (shared memory). Also for ext-image-copy-capture.
pub fn copy_into<R>(
    renderer: &mut R,
    size: Size<i32, Physical>,
    scale: f64,
    elements: &[&Element<R>],
    region: Rectangle<i32, Physical>,
    buffer: &WlBuffer,
) -> Result<(), Box<dyn std::error::Error>>
where
    R: Renderer + ImportAll + ImportMem + Offscreen<GlesRenderbuffer> + ExportMem,
    R::TextureId: Clone + Texture + Send + 'static,
    R::Error: 'static,
{
    let mut target: GlesRenderbuffer = renderer.create_buffer(
        Fourcc::Xrgb8888,
        size.to_logical(1).to_buffer(1, Transform::Normal),
    )?;
    let mut framebuffer = renderer.bind(&mut target)?;
    // Upright, whatever the output's transform: what a screenshot shows.
    let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
    tracker.render_output(renderer, &mut framebuffer, 0, elements, render::CLEAR)?;
    let at: Rectangle<i32, Buffer> = Rectangle::new(
        (region.loc.x, region.loc.y).into(),
        (region.size.w, region.size.h).into(),
    );
    let mapping = renderer.copy_framebuffer(&framebuffer, at, Fourcc::Xrgb8888)?;
    let pixels = renderer.map_texture(&mapping)?;
    let row = region.size.w as usize * 4;
    with_buffer_contents_mut(buffer, |pool, len, b| {
        // SAFETY: smithay maps the client's pool at `pool`, `len` bytes
        // long, for as long as this closure runs (and turns a client that
        // shrinks it meanwhile into a protocol error, not a crash).
        let pool = unsafe { std::slice::from_raw_parts_mut(pool, len) };
        for (y, src) in pixels.chunks_exact(row).enumerate() {
            let start = b.offset as usize + y * b.stride as usize;
            if let Some(dst) = pool.get_mut(start..start + row) {
                dst.copy_from_slice(src);
            }
        }
    })?;
    Ok(())
}
