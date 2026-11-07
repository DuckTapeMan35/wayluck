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

mod frames;
mod sessions;
mod sources;

pub use frames::capture_frames;

use crate::{Wayluck, windows::WindowId, wm::scenes::SceneId};
use smithay::{
    backend::{
        allocator::{Fourcc, Modifier, format::FormatSet},
        renderer::damage::OutputDamageTracker,
    },
    output::Output,
    reexports::{
        calloop::RegistrationToken,
        wayland_protocols::ext::{
            image_capture_source::v1::server::{
                ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
                ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
            },
            image_copy_capture::v1::server::{
                ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
                ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
                ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
            },
        },
        wayland_server::{DisplayHandle, protocol::wl_buffer::WlBuffer},
    },
    utils::{Physical, Size},
};
use std::sync::Mutex;

/// What a capture source captures.
#[derive(Clone, PartialEq)]
pub enum Source {
    Output(Output),
    Window(WindowId),
    /// A scene's windows, as laid out in it, shown or not
    Scene(SceneId),
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
    /// Captured windows' and scenes' frame callbacks.
    pacers: Vec<(Source, RegistrationToken)>,
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
}
