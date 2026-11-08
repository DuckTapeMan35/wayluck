//! A screen capturer, as xdg-desktop-portal-wlr is one: an output captured
//! with ext-image-copy-capture into shared memory, frame after frame (on a
//! thread of its own, saying how long each took to come), or once, to look
//! at its pixels.

use std::{
    fs::File,
    os::fd::AsFd,
    os::unix::net::UnixStream,
    path::Path,
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
        wl_output::WlOutput,
        wl_registry::WlRegistry,
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
    },
};
use wayland_protocols::ext::{
    image_capture_source::v1::client::{
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};

use super::Session;

/// What it's been told.
#[derive(Default)]
struct Capturer {
    /// The size frames are, once the session says.
    size: Option<(i32, i32)>,
    /// The session said what buffers it takes.
    constrained: bool,
    /// The frame asked for came (true) or failed (false).
    frame: Option<bool>,
}

/// Capture session's first output, frames frames, one after another, each
/// asked for once the last came: how long each took, as they come (a frame
/// that failed ends it).
pub fn capture_output(session: &Session, frames: usize) -> Receiver<Duration> {
    let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
    let file = session.dir().join("capture");
    let (took, heard) = channel();
    std::thread::spawn(move || {
        capture(stream, &file, frames, |asked, _| took.send(asked).is_ok());
    });
    heard
}

/// What session's first output shows, as captured once.
pub struct Screenshot {
    pub width: i32,
    pub height: i32,
    /// XRGB8888: blue, green, red and a byte unused for each pixel.
    pixels: Vec<u8>,
}

impl Screenshot {
    /// The colour at x, y, as 0xRRGGBB.
    pub fn at(&self, x: i32, y: i32) -> u32 {
        let i = ((y * self.width + x) * 4) as usize;
        let [b, g, r] = [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2]];
        u32::from_be_bytes([0, r, g, b])
    }
}

/// Capture session's first output, once.
pub fn screenshot(session: &Session) -> Screenshot {
    let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
    let file = session.dir().join("screenshot");
    let mut shot = None;
    capture(stream, &file, 1, |_, (width, height)| {
        let pixels = std::fs::read(&file).unwrap();
        shot = Some(Screenshot {
            width,
            height,
            pixels,
        });
        false
    });
    shot.expect("no frame came")
}

/// Capture the first output over stream into shared memory backed by
/// file, frames frames, one after another, each asked for once the last
/// came; each is handed to each (how long it took, the frames' size),
/// which says whether to go on. A frame that failed ends it.
fn capture(
    stream: UnixStream,
    file: &Path,
    frames: usize,
    mut each: impl FnMut(Duration, (i32, i32)) -> bool,
) {
    let conn = Connection::from_socket(stream).unwrap();
    let (globals, mut queue) = registry_queue_init::<Capturer>(&conn).unwrap();
    let qh = queue.handle();
    let output: WlOutput = globals.bind(&qh, 1..=4, ()).unwrap();
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).unwrap();
    let sources: ExtOutputImageCaptureSourceManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
    let manager: ExtImageCopyCaptureManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
    let source = sources.create_source(&output, &qh, ());
    let capture = manager.create_session(&source, Options::empty(), &qh, ());
    let mut capturer = Capturer::default();
    while !capturer.constrained {
        queue.blocking_dispatch(&mut capturer).unwrap();
    }
    let (w, h) = capturer.size.unwrap();
    let memory = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(file)
        .unwrap();
    memory.set_len((w * h * 4) as u64).unwrap();
    let pool = shm.create_pool(memory.as_fd(), w * h * 4, &qh, ());
    let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, &qh, ());
    for _ in 0..frames {
        let frame = capture.create_frame(&qh, ());
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, w, h);
        frame.capture();
        let asked = Instant::now();
        capturer.frame = None;
        while capturer.frame.is_none() {
            if queue.blocking_dispatch(&mut capturer).is_err() {
                return;
            }
        }
        frame.destroy();
        if capturer.frame != Some(true) || !each(asked.elapsed(), (w, h)) {
            return;
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for Capturer {
    fn event(
        capturer: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        match event {
            Event::BufferSize { width, height } => {
                capturer.size = Some((width as i32, height as i32));
            }
            Event::Done => capturer.constrained = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for Capturer {
    fn event(
        capturer: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => capturer.frame = Some(true),
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => capturer.frame = Some(false),
            _ => {}
        }
    }
}

/// The rest say nothing it needs.
macro_rules! ignore {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for Capturer {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore!(
    WlOutput,
    WlShm,
    WlShmPool,
    WlBuffer,
    ExtOutputImageCaptureSourceManagerV1,
    ExtImageCaptureSourceV1,
    ExtImageCopyCaptureManagerV1
);

impl Dispatch<WlRegistry, GlobalListContents> for Capturer {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
