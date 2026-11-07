//! Frames: copying what a source shows into a session's buffer, once
//! something in it changed.

use crate::{
    Wayluck,
    capture::screencopy,
    render::rounded::AsGles,
    render::{self, Element},
};
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{
            Bind, ExportMem, ImportAll, ImportMem, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker,
            element::{AsRenderElements, Element as _, Kind},
            gles::GlesRenderbuffer,
        },
    },
    output::Output,
    reexports::{
        wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::{
            self, ExtImageCopyCaptureFrameV1, FailureReason,
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, Resource, backend::ClientId,
            protocol::wl_output::Transform as WlTransform,
        },
    },
    utils::{Clock, Monotonic, Physical, Point, Rectangle, Scale, Transform},
};
use std::time::Duration;

use super::{FrameData, Source, Waiting, sessions::send_constraints};

impl Wayluck {
    /// Is a frame of output waiting (for step 3's capture_stopped)?
    pub fn image_frames_waiting(&self, output: &Output) -> bool {
        let source = Source::Output(output.clone());
        (self.image_capture.frames.iter()).any(|w| {
            (self.image_capture.sessions.iter())
                .any(|s| s.resource == w.session && s.source == source)
        })
    }
}

/// After a frame was drawn: copy the frames that are due. A session's
/// first, at once; a later one if its source changed since.
pub fn capture_frames<R>(state: &mut Wayluck, renderer: &mut R)
where
    R: Renderer
        + ImportAll
        + ImportMem
        + Offscreen<GlesRenderbuffer>
        + ExportMem
        + Bind<Dmabuf>
        + AsGles,
    R::TextureId: Clone + Texture + Send + 'static,
    R::Error: 'static,
{
    let waiting = std::mem::take(&mut state.image_capture.frames);
    let mut later = Vec::new();
    for w in waiting {
        let Some(at) = (state.image_capture.sessions.iter()).position(|s| s.resource == w.session)
        else {
            w.frame.failed(FailureReason::Stopped);
            continue;
        };
        let source = state.image_capture.sessions[at].source.clone();
        let Some(size) = state.source_size(&source) else {
            state.stop_capture_sessions(&source);
            w.frame.failed(FailureReason::Stopped);
            continue;
        };
        // Its size changed (a window resized): the client hears the new
        // one, and makes a new buffer for its next frame.
        let session = &mut state.image_capture.sessions[at];
        if size != session.size {
            session.size = size;
            session.damage = OutputDamageTracker::new(size, 1.0, Transform::Normal);
            session.copied = false;
            send_constraints(&session.resource, size, state.image_capture.dmabuf.as_ref());
            w.frame.failed(FailureReason::BufferConstraints);
            continue;
        }
        let cursors = session.cursors;
        let (elements, scale) = source_elements(state, &source, renderer, cursors);
        let session = &mut state.image_capture.sessions[at];
        let damage = session.damage.damage_output(1, &elements).ok();
        let damage: Vec<Rectangle<i32, Physical>> =
            damage.and_then(|(d, _)| d.cloned()).unwrap_or_default();
        if session.copied && damage.is_empty() {
            later.push(w); // nothing new yet
            continue;
        }
        let first = !session.copied;
        let drawn: Vec<_> = elements.iter().collect();
        let all = Rectangle::from_size(size);
        match screencopy::copy_into(renderer, size, scale, &drawn, all, &w.buffer) {
            Ok(()) => {
                let session = &mut state.image_capture.sessions[at];
                session.copied = true;
                w.frame.transform(WlTransform::Normal);
                let damage = if first { vec![all] } else { damage };
                for d in damage {
                    w.frame.damage(d.loc.x, d.loc.y, d.size.w, d.size.h);
                }
                let now = Duration::from(Clock::<Monotonic>::new().now());
                let secs = now.as_secs();
                w.frame
                    .presentation_time((secs >> 32) as u32, secs as u32, now.subsec_nanos());
                w.frame.ready();
            }
            Err(e) => {
                tracing::warn!("image capture: {e}");
                w.frame.failed(FailureReason::Unknown);
            }
        }
    }
    state.image_capture.frames.extend(later);
}

/// What a source shows, and at what scale: an output as it's drawn (the
/// cursor only if asked for); a window alone, from its corner.
fn source_elements<R>(
    state: &mut Wayluck,
    source: &Source,
    renderer: &mut R,
    cursors: bool,
) -> (Vec<Element<R>>, f64)
where
    R: Renderer + ImportAll + ImportMem + AsGles,
    R::TextureId: Clone + Texture + Send + 'static,
{
    match source {
        Source::Output(output) => {
            let elements = render::elements(state, output, renderer)
                .into_iter()
                .filter(|e| cursors || e.kind() != Kind::Cursor)
                .collect();
            (elements, output.current_scale().fractional_scale())
        }
        Source::Window(id) => {
            let Some(window) = state.windows.get(*id).cloned() else {
                return (Vec::new(), 1.0);
            };
            let corner = window.geometry().loc;
            let at = Point::<i32, Physical>::from((-corner.x, -corner.y));
            let elements = window.render_elements(renderer, at, Scale::from(1.0), 1.0);
            (elements, 1.0)
        }
        Source::Gone => (Vec::new(), 1.0),
    }
}

// ---- sources ----
impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_image_copy_capture_frame_v1::{Error, Request};
        let mut inner = data.inner.lock().unwrap();
        if inner.captured && !matches!(request, Request::Destroy) {
            frame.post_error(Error::AlreadyCaptured, "captured already");
            return;
        }
        match request {
            Request::AttachBuffer { buffer } => inner.buffer = Some(buffer),
            Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                // Every frame is copied whole, so the client's damage is
                // only checked.
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    frame.post_error(Error::InvalidBufferDamage, "invalid damage");
                }
            }
            Request::Capture => {
                let Some(buffer) = inner.buffer.clone() else {
                    frame.post_error(Error::NoBuffer, "no buffer attached");
                    return;
                };
                inner.captured = true;
                drop(inner);
                let Some(session) =
                    (state.image_capture.sessions.iter()).find(|s| s.resource == data.session)
                else {
                    frame.failed(FailureReason::Stopped);
                    return;
                };
                if !screencopy::fits(&buffer, session.size) {
                    frame.failed(FailureReason::BufferConstraints);
                    return;
                }
                let source = session.source.clone();
                state.image_capture.frames.push(Waiting {
                    frame: frame.clone(),
                    session: data.session.clone(),
                    buffer,
                });
                if let Source::Output(output) = &source {
                    state.capture_activity(output);
                }
                // A frame is drawn to see: anything that changed since the
                // last copy (while none was waiting) is in this one. If
                // nothing did, it waits for something to (capture_frames).
                state.queue_redraw();
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        frame: &ExtImageCopyCaptureFrameV1,
        _data: &FrameData,
    ) {
        state.image_capture.frames.retain(|w| &w.frame != frame);
    }
}
