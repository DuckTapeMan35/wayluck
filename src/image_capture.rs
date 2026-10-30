//! ext-image-capture-source and ext-image-copy-capture: the standard way to
//! capture an output or a single window (screen sharing a window, through
//! xdg-desktop-portal-wlr). smithay doesn't have them; this does, into
//! shared memory or into dmabufs, on the GPU.
//!
//! A source is an output or a window (by its handle from the window list).
//! A session on a source says what buffer to give it:
//! XRGB8888 at the source's size, in shared memory or a dmabuf on wayluck's
//! GPU (if the backend says which: DmabufTarget), and again if that changes
//! (a frame made for the old size fails, so the client makes a new buffer).
//! A session made for a picker's thumbnail has smaller frames:
//! the source drawn shrunk to fit. A session's first frame is copied at
//! once; each later one once something in the source changed (the session's
//! own damage tracker says what). A window being captured keeps drawing even
//! off screen: it gets frame callbacks while it has a session. Cursor
//! sessions aren't offered yet: they stop at once (paint_cursors draws the
//! cursor into frames instead).
//!

use crate::{
    Wayluck,
    render::{self, Element},
    screencopy,
    windows::WindowId,
};
use smithay::{
    backend::{
        allocator::{Fourcc, Modifier, dmabuf::Dmabuf, format::FormatSet},
        renderer::{
            Bind, ExportMem, ImportAll, ImportMem, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker,
            element::{AsRenderElements, utils::RescaleRenderElement},
            gles::GlesRenderbuffer,
        },
    },
    output::Output,
    reexports::{
        calloop::{
            RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::ext::{
            foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
            image_capture_source::v1::server::{
                ext_foreign_toplevel_image_capture_source_manager_v1::{
                    self, ExtForeignToplevelImageCaptureSourceManagerV1,
                },
                ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
                ext_output_image_capture_source_manager_v1::{
                    self, ExtOutputImageCaptureSourceManagerV1,
                },
            },
            image_copy_capture::v1::server::{
                ext_image_copy_capture_cursor_session_v1::{
                    self, ExtImageCopyCaptureCursorSessionV1,
                },
                ext_image_copy_capture_frame_v1::{
                    self, ExtImageCopyCaptureFrameV1, FailureReason,
                },
                ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1, Options},
                ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
            },
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::ClientId,
            protocol::{wl_buffer::WlBuffer, wl_output::Transform as WlTransform, wl_shm},
        },
    },
    utils::{Clock, Monotonic, Physical, Point, Rectangle, Scale, Size, Transform},
};
use std::{collections::HashMap, sync::Mutex, time::Duration};

/// What a capture source captures.
#[derive(Clone, PartialEq)]
pub enum Source {
    Output(Output),
    Window(WindowId),
    /// It was gone already when the source was made.
    Gone,
}

/// A frame's state, kept on it until it's captured.
pub struct FrameData {
    session: ExtImageCopyCaptureSessionV1,
    inner: Mutex<FrameInner>,
}

#[derive(Default)]
struct FrameInner {
    buffer: Option<WlBuffer>,
    captured: bool,
}

/// A session made from a cursor session: there's none to give yet.
pub struct Stopped;

#[derive(Default)]
pub struct ImageCapture {
    /// Where frames may be dmabufs; None: only shared memory (the backend
    /// sets it, if it knows its GPU).
    pub dmabuf: Option<DmabufTarget>,
    sessions: Vec<Session>,
    /// Frames waiting to be copied.
    frames: Vec<Waiting>,
    /// Captured windows' frame callbacks, by window.
    pacers: HashMap<WindowId, RegistrationToken>,
}

/// The GPU wayluck draws with, and the modifiers it can draw XRGB8888
/// dmabufs with: what a client allocates a frame's dmabuf on.
pub struct DmabufTarget {
    pub device: u64,
    pub modifiers: Vec<Modifier>,
}

impl DmabufTarget {
    /// From the renderer's formats: the XRGB8888 ones' modifiers.
    pub fn new(device: u64, formats: &FormatSet) -> Self {
        let modifiers = (formats.iter())
            .filter(|f| f.code == Fourcc::Xrgb8888)
            .map(|f| f.modifier)
            .collect();
        DmabufTarget { device, modifiers }
    }
}

struct Session {
    resource: ExtImageCopyCaptureSessionV1,
    source: Source,
    cursors: bool,
    /// How big its frames may be, if smaller than the source
    fit: Option<Size<i32, Physical>>,
    /// The buffer size it last told its client.
    size: Size<i32, Physical>,
    /// What the source last showed, to tell when it changed, and where.
    damage: OutputDamageTracker,
    /// Has it copied a frame yet? (Its first is copied at once.)
    copied: bool,
    /// Its frame, while there is one (one at a time).
    frame: Option<ExtImageCopyCaptureFrameV1>,
}

struct Waiting {
    frame: ExtImageCopyCaptureFrameV1,
    session: ExtImageCopyCaptureSessionV1,
    buffer: WlBuffer,
}

impl Wayluck {
    pub fn image_capture_globals(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, ExtOutputImageCaptureSourceManagerV1, _>(1, ());
        dh.create_global::<Wayluck, ExtForeignToplevelImageCaptureSourceManagerV1, _>(1, ());
        dh.create_global::<Wayluck, ExtImageCopyCaptureManagerV1, _>(1, ());
    }

    /// How big a source's frames are now; None: it's gone.
    fn source_size(&self, source: &Source) -> Option<Size<i32, Physical>> {
        let size = match source {
            Source::Output(output) => {
                self.space.output_geometry(output)?;
                output.current_mode()?.size
            }
            Source::Window(id) => {
                let size = self.windows.get(*id)?.geometry().size;
                (size.w, size.h).into()
            }
            Source::Gone => return None,
        };
        (size.w > 0 && size.h > 0).then_some(size)
    }

    /// A new session on source (painting the cursor in, with cursors), its
    /// frames no bigger than fit if given: its client hears what buffer to
    /// give it.
    pub fn start_session(
        &mut self,
        session: ExtImageCopyCaptureSessionV1,
        source: Source,
        cursors: bool,
        fit: Option<Size<i32, Physical>>,
    ) {
        let Some(full) = self.source_size(&source) else {
            session.stopped();
            return;
        };
        let (size, _) = frame_size(full, fit);
        send_constraints(&session, size, self.image_capture.dmabuf.as_ref());
        if let Source::Window(id) = source {
            self.start_window_pacing(id);
        }
        self.image_capture.sessions.push(Session {
            resource: session,
            source,
            cursors,
            fit,
            size,
            damage: OutputDamageTracker::new(size, 1.0, Transform::Normal),
            copied: false,
            frame: None,
        });
    }

    /// The sessions on a source end (it's gone): their clients hear, and
    /// frames waiting fail.
    pub fn stop_capture_sessions(&mut self, gone: &Source) {
        let (stopped, kept) = std::mem::take(&mut self.image_capture.sessions)
            .into_iter()
            .partition(|s: &Session| &s.source == gone);
        self.image_capture.sessions = kept;
        for session in stopped {
            session.resource.stopped();
            self.fail_frames_of(&session.resource);
        }
        if let Source::Window(id) = gone {
            self.stop_window_pacing(*id);
        }
    }

    fn fail_frames_of(&mut self, session: &ExtImageCopyCaptureSessionV1) {
        let (failed, kept) = std::mem::take(&mut self.image_capture.frames)
            .into_iter()
            .partition(|w: &Waiting| &w.session == session);
        self.image_capture.frames = kept;
        for waiting in failed {
            waiting.frame.failed(FailureReason::Stopped);
        }
    }

    /// Is a frame of output waiting (for step 3's capture_stopped)?
    pub fn image_frames_waiting(&self, output: &Output) -> bool {
        let source = Source::Output(output.clone());
        (self.image_capture.frames.iter()).any(|w| {
            (self.image_capture.sessions.iter())
                .any(|s| s.resource == w.session && s.source == source)
        })
    }

    /// A window got its first session: frame callbacks while it's off
    /// screen (on screen, its output gives it them), at 60 Hz.
    fn start_window_pacing(&mut self, id: WindowId) {
        if self.image_capture.pacers.contains_key(&id) {
            return;
        }
        let every = Duration::from_nanos(1_000_000_000 / 60);
        let pacer =
            self.loop_handle
                .insert_source(Timer::from_duration(every), move |_, _, state| {
                    let Some(window) = state.windows.get(id).cloned() else {
                        return TimeoutAction::Drop;
                    };
                    let on_screen = !state.space.outputs_for_element(&window).is_empty();
                    let some_output = state.monitors().next().cloned();
                    if let (false, Some(output)) = (on_screen, some_output) {
                        let now = state.start_time.elapsed();
                        window.send_frame(&output, now, Some(Duration::ZERO), |_, _| {
                            Some(output.clone())
                        });
                    }
                    TimeoutAction::ToDuration(every)
                });
        match pacer {
            Ok(token) => {
                self.image_capture.pacers.insert(id, token);
            }
            Err(e) => tracing::warn!("can't pace captured window {id}: {e}"),
        }
    }

    /// Its last session went: no more frame callbacks of its own.
    fn stop_window_pacing(&mut self, id: WindowId) {
        let source = Source::Window(id);
        if self
            .image_capture
            .sessions
            .iter()
            .any(|s| s.source == source)
        {
            return;
        }
        if let Some(token) = self.image_capture.pacers.remove(&id) {
            self.loop_handle.remove(token);
        }
    }
}

/// How big a session's frames of a source this big are, and how much
/// smaller than the source that is: as big as the source, or shrunk to fit
/// in fit, keeping its shape.
fn frame_size(
    source: Size<i32, Physical>,
    fit: Option<Size<i32, Physical>>,
) -> (Size<i32, Physical>, f64) {
    let Some(fit) = fit else {
        return (source, 1.0);
    };
    let shrink = (fit.w as f64 / source.w as f64)
        .min(fit.h as f64 / source.h as f64)
        .min(1.0);
    let w = (source.w as f64 * shrink).round().max(1.0) as i32;
    let h = (source.h as f64 * shrink).round().max(1.0) as i32;
    ((w, h).into(), shrink)
}

/// Tell a session's client what buffer to give it: shared memory, or a
/// dmabuf on wayluck's GPU (if dmabuf says which).
fn send_constraints(
    session: &ExtImageCopyCaptureSessionV1,
    size: Size<i32, Physical>,
    dmabuf: Option<&DmabufTarget>,
) {
    session.buffer_size(size.w as u32, size.h as u32);
    session.shm_format(wl_shm::Format::Xrgb8888);
    if let Some(target) = dmabuf.filter(|t| !t.modifiers.is_empty()) {
        session.dmabuf_device(target.device.to_ne_bytes().to_vec());
        let modifiers = (target.modifiers.iter())
            .flat_map(|m| u64::from(*m).to_ne_bytes())
            .collect();
        session.dmabuf_format(Fourcc::Xrgb8888 as u32, modifiers);
    }
    session.done();
}

/// After a frame was drawn: copy the frames that are due. A session's
/// first, at once; a later one if its source changed since.
pub fn capture_frames<R>(state: &mut Wayluck, renderer: &mut R)
where
    R: Renderer + ImportAll + ImportMem + Offscreen<GlesRenderbuffer> + ExportMem + Bind<Dmabuf>,
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
        let Some(full) = state.source_size(&source) else {
            state.stop_capture_sessions(&source);
            w.frame.failed(FailureReason::Stopped);
            continue;
        };
        let (size, shrink) = frame_size(full, state.image_capture.sessions[at].fit);
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
        // Shrunk to the frame's size, from its top left corner.
        let elements: Vec<_> = (elements.into_iter())
            .map(|e| RescaleRenderElement::from_element(e, Point::default(), shrink))
            .collect();
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
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Texture + Send + 'static,
{
    match source {
        Source::Output(output) => {
            let elements = render::elements(state, output, renderer)
                .into_iter()
                .filter(|e| cursors || !matches!(e, Element::Cursor(_)))
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
impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Wayluck {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource {
            source,
            output,
        } = request
        {
            let of = Output::from_resource(&output).map_or(Source::Gone, Source::Output);
            data_init.init(source, of);
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Wayluck {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: ext_foreign_toplevel_image_capture_source_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_foreign_toplevel_image_capture_source_manager_v1::Request;
        if let Request::CreateSource {
            source,
            toplevel_handle,
        } = request
        {
            let of = window_of(&toplevel_handle)
                .filter(|id| state.windows.get(*id).is_some())
                .map_or(Source::Gone, Source::Window);
            data_init.init(source, of);
        }
    }
}

/// The window a handle from the window list is for.
fn window_of(handle: &ExtForeignToplevelHandleV1) -> Option<WindowId> {
    handle.data::<WindowId>().copied()
}

impl Dispatch<ExtImageCaptureSourceV1, Source> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _source: &ExtImageCaptureSourceV1,
        _request: ext_image_capture_source_v1::Request,
        _data: &Source,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy: sessions made from it go on.
    }
}

// ---- sessions ----

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ()> for Wayluck {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use ext_image_copy_capture_manager_v1::Request;
        match request {
            Request::CreateSession {
                session,
                source,
                options,
            } => {
                let options = match options.into_result() {
                    Ok(options) => options,
                    Err(_) => {
                        manager.post_error(
                            ext_image_copy_capture_manager_v1::Error::InvalidOption,
                            "unknown option",
                        );
                        return;
                    }
                };
                let session = data_init.init(session, ());
                let source = source.data::<Source>().cloned().unwrap_or(Source::Gone);
                let cursors = options.contains(Options::PaintCursors);
                state.start_session(session, source, cursors, None);
            }
            Request::CreatePointerCursorSession { session, .. } => {
                data_init.init(session, ());
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        session: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_session_v1::Request::CreateFrame { frame } = request {
            let mine = (state.image_capture.sessions.iter_mut()).find(|s| &s.resource == session);
            if let Some(s) = &mine
                && s.frame.as_ref().is_some_and(Resource::is_alive)
            {
                session.post_error(
                    ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                    "a frame before the last was destroyed",
                );
                return;
            }
            let frame = data_init.init(
                frame,
                FrameData {
                    session: session.clone(),
                    inner: Mutex::default(),
                },
            );
            if let Some(s) = mine {
                s.frame = Some(frame);
            }
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        session: &ExtImageCopyCaptureSessionV1,
        _data: &(),
    ) {
        let gone = (state.image_capture.sessions.iter())
            .position(|s| &s.resource == session)
            .map(|at| state.image_capture.sessions.remove(at));
        state.fail_frames_of(session);
        if let Some(Session {
            source: Source::Window(id),
            ..
        }) = gone
        {
            state.stop_window_pacing(id);
        }
    }
}

/// A session made from a cursor session: stopped from the start.
impl Dispatch<ExtImageCopyCaptureSessionV1, Stopped> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        session: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        _data: &Stopped,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        // Its frames fail: no session copies them.
        if let ext_image_copy_capture_session_v1::Request::CreateFrame { frame } = request {
            data_init.init(
                frame,
                FrameData {
                    session: session.clone(),
                    inner: Mutex::default(),
                },
            );
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, ()> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _cursor: &ExtImageCopyCaptureCursorSessionV1,
        request: ext_image_copy_capture_cursor_session_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } =
            request
        {
            data_init.init(session, Stopped).stopped();
        }
    }
}

// ---- frames ----
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
                let (first, source) = (!session.copied, session.source.clone());
                state.image_capture.frames.push(Waiting {
                    frame: frame.clone(),
                    session: data.session.clone(),
                    buffer,
                });
                if let Source::Output(output) = &source {
                    state.capture_activity(output);
                }
                // A first frame doesn't wait for something to change.
                if first {
                    state.queue_redraw();
                }
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
