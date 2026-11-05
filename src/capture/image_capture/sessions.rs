//! Sessions on a source: what buffers their frames take, and keeping a
//! captured window or scene drawn while it's captured.

use crate::{Wayluck, windows::WindowId};
use smithay::{
    backend::{allocator::Fourcc, renderer::damage::OutputDamageTracker},
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols::ext::image_copy_capture::v1::server::{
            ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
            ext_image_copy_capture_frame_v1::FailureReason,
            ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1, Options},
            ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::ClientId, protocol::wl_shm,
        },
    },
    utils::{Physical, Size, Transform},
};
use std::{sync::Mutex, time::Duration};

use super::{DmabufTarget, FrameData, Session, Source, Stopped, Waiting};

impl Wayluck {
    /// How big a source's frames are now; None: it's gone.
    pub(super) fn source_size(&self, source: &Source) -> Option<Size<i32, Physical>> {
        let size = match source {
            Source::Output(output) => {
                self.space.output_geometry(output)?;
                output.current_mode()?.size
            }
            Source::Window(id) => {
                let size = self.windows.get(*id)?.geometry().size;
                (size.w, size.h).into()
            }
            Source::Scene(id) => {
                let size = self.scenes.get(*id)?.size;
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
        self.start_capture_pacing(&source);
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
        self.stop_capture_pacing(gone);
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

    /// A window or scene got its first session: frame callbacks for the
    /// windows while they're off screen (on screen, their output gives them
    /// them), at 60 Hz.
    fn start_capture_pacing(&mut self, source: &Source) {
        let paced = source.clone();
        if !matches!(paced, Source::Window(_) | Source::Scene(_))
            || self.image_capture.pacers.iter().any(|(s, _)| *s == paced)
        {
            return;
        }
        let every = Duration::from_nanos(1_000_000_000 / 60);
        let timer = Timer::from_duration(every);
        let pacer = self.loop_handle.insert_source(timer, move |_, _, state| {
            let ids: Vec<WindowId> = match &paced {
                Source::Window(id) => vec![*id],
                Source::Scene(id) => (state.scenes.get(*id).into_iter())
                    .flat_map(|scene| scene.windows().map(|(w, _)| w))
                    .collect(),
                _ => Vec::new(),
            };
            let Some(output) = state.monitors().next().cloned() else {
                return TimeoutAction::ToDuration(every);
            };
            let now = state.start_time.elapsed();
            for id in ids {
                let Some(window) = state.windows.get(id).cloned() else {
                    continue;
                };
                if state.space.outputs_for_element(&window).is_empty() {
                    window.send_frame(&output, now, Some(Duration::ZERO), |_, _| {
                        Some(output.clone())
                    });
                }
            }
            TimeoutAction::ToDuration(every)
        });
        match pacer {
            Ok(token) => self.image_capture.pacers.push((source.clone(), token)),
            Err(e) => tracing::warn!("can't pace a capture's windows: {e}"),
        }
    }

    /// Its last session went: no more frame callbacks of its own.
    fn stop_capture_pacing(&mut self, source: &Source) {
        if (self.image_capture.sessions.iter()).any(|s| s.source == *source) {
            return;
        }
        let pacers = std::mem::take(&mut self.image_capture.pacers);
        let (stopped, kept) = pacers.into_iter().partition(|(s, _)| s == source);
        self.image_capture.pacers = kept;
        for (_, token) in stopped {
            self.loop_handle.remove(token);
        }
    }
}

/// How big a session's frames of a source this big are, and how much
/// smaller than the source that is: as big as the source, or shrunk to fit
/// in fit, keeping its shape.
pub(super) fn frame_size(
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
pub(super) fn send_constraints(
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
        if let Some(gone) = gone {
            state.stop_capture_pacing(&gone.source);
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
