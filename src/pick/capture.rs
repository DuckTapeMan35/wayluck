//! Live thumbnails: each monitor captured (ext-image-copy-capture), frame
//! after frame, over a connection of the picker's own. A frame comes into
//! shared memory; the next is asked for once the last was drawn (next), so
//! frames come no faster than the picker shows them.

use std::{
    fs::File,
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::FileExt,
    },
};

use smithay::reexports::rustix::fs::{MemfdFlags, memfd_create};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
        wl_output::{self, WlOutput},
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

pub struct Captures {
    connection: Connection,
    shm: WlShm,
    /// One for each monitor asked for, in the order asked.
    pub thumbnails: Vec<Thumbnail>,
    /// The monitors' wl_outputs, and their names once told.
    outputs: Vec<(WlOutput, Option<String>)>,
    /// A thumbnail got a new frame since this was last cleared.
    pub fresh: bool,
}

#[derive(Default)]
pub struct Thumbnail {
    session: Option<ExtImageCopyCaptureSessionV1>,
    /// The size the session asks for (as it says, then done).
    asked: (i32, i32),
    buffer: Option<Shm>,
    /// The frame being captured, if one is.
    frame: Option<ExtImageCopyCaptureFrameV1>,
    /// The newest frame, until the picker takes it to draw.
    pub image: Option<Image>,
}

/// A frame's pixels: XRGB8888, rows of width * 4 bytes.
pub struct Image {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

/// A buffer in shared memory: the client's side of it is a file.
struct Shm {
    buffer: WlBuffer,
    file: File,
    width: i32,
    height: i32,
}

impl Captures {
    /// Start capturing the monitors with these names. The queue is to be
    /// dispatched with the captures as its connection has events (window.rs).
    pub fn start(
        names: &[String],
    ) -> Result<(Captures, EventQueue<Captures>), Box<dyn std::error::Error>> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<Captures>(&connection)?;
        let qh = queue.handle();
        let shm: WlShm = globals.bind(&qh, 1..=1, ())?;
        let sources: ExtOutputImageCaptureSourceManagerV1 = globals.bind(&qh, 1..=1, ())?;
        let copier: ExtImageCopyCaptureManagerV1 = globals.bind(&qh, 1..=1, ())?;
        // Every output, by name (wl_output 4 says it).
        let outputs = (globals.contents().clone_list().into_iter())
            .filter(|g| g.interface == "wl_output" && g.version >= 4)
            .enumerate()
            .map(|(at, g)| {
                let output = globals.registry().bind(g.name, 4, &qh, at);
                (output, None)
            })
            .collect();
        let mut captures = Captures {
            connection,
            shm,
            thumbnails: Vec::new(),
            outputs,
            fresh: false,
        };
        queue.roundtrip(&mut captures)?;
        for (at, name) in names.iter().enumerate() {
            let mut thumbnail = Thumbnail::default();
            let output = (captures.outputs.iter()).find(|(_, n)| n.as_ref() == Some(name));
            if let Some((output, _)) = output {
                let source = sources.create_source(output, &qh, ());
                thumbnail.session = Some(copier.create_session(&source, Options::empty(), &qh, at));
                source.destroy();
            }
            captures.thumbnails.push(thumbnail);
        }
        captures.connection.flush()?;
        Ok((captures, queue))
    }

    /// The connection, to wait on.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Ask for the next frame of every thumbnail whose last was taken.
    pub fn next(&mut self, qh: &QueueHandle<Captures>) {
        for at in 0..self.thumbnails.len() {
            let t = &self.thumbnails[at];
            if t.image.is_none() && t.frame.is_none() {
                self.capture(at, qh);
            }
        }
        let _ = self.connection.flush();
    }

    /// Capture a frame of thumbnail at into its buffer (made first, if it
    /// has none of the size asked for).
    fn capture(&mut self, at: usize, qh: &QueueHandle<Captures>) {
        let (width, height) = self.thumbnails[at].asked;
        let Some(session) = self.thumbnails[at].session.clone() else {
            return;
        };
        if width <= 0 || height <= 0 {
            return; // not told the size yet
        }
        let t = &mut self.thumbnails[at];
        if t.buffer
            .as_ref()
            .is_none_or(|b| (b.width, b.height) != (width, height))
        {
            if let Some(old) = t.buffer.take() {
                old.buffer.destroy();
            }
            match shm_buffer(&self.shm, width, height, qh) {
                Ok(buffer) => t.buffer = Some(buffer),
                Err(e) => return eprintln!("wayluck pick: a thumbnail's buffer: {e}"),
            }
        }
        let Some(buffer) = &t.buffer else { return };
        let frame = session.create_frame(qh, at);
        frame.attach_buffer(&buffer.buffer);
        frame.damage_buffer(0, 0, width, height);
        frame.capture();
        t.frame = Some(frame);
    }
}

/// A buffer of width × height XRGB8888 pixels in a new memfd.
fn shm_buffer(
    shm: &WlShm,
    width: i32,
    height: i32,
    qh: &QueueHandle<Captures>,
) -> Result<Shm, Box<dyn std::error::Error>> {
    let stride = width * 4;
    let len = stride * height;
    let fd: OwnedFd = memfd_create("wayluck-pick", MemfdFlags::CLOEXEC)?;
    let file = File::from(fd);
    file.set_len(len as u64)?;
    let pool: WlShmPool = shm.create_pool(file.as_fd(), len, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    Ok(Shm {
        buffer,
        file,
        width,
        height,
    })
}

impl Dispatch<ExtImageCopyCaptureSessionV1, usize> for Captures {
    fn event(
        captures: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        at: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let t = &mut captures.thumbnails[*at];
        match event {
            Event::BufferSize { width, height } => t.asked = (width as i32, height as i32),
            // Told all of what to give: the first frame (or the next, at the
            // new size, if one failed for the old).
            Event::Done => {
                if t.frame.is_none() && t.image.is_none() {
                    captures.capture(*at, qh);
                }
            }
            Event::Stopped => {
                if let Some(session) = t.session.take() {
                    session.destroy();
                }
            }
            // Shared memory is what it's given: no need to know the rest.
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, usize> for Captures {
    fn event(
        captures: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        at: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let t = &mut captures.thumbnails[*at];
        match event {
            Event::Ready => {
                frame.destroy();
                t.frame = None;
                let Some(shm) = &t.buffer else { return };
                let mut pixels = vec![0; (shm.width * shm.height * 4) as usize];
                if let Err(e) = shm.file.read_exact_at(&mut pixels, 0) {
                    return eprintln!("wayluck pick: reading a thumbnail: {e}");
                }
                t.image = Some(Image {
                    width: shm.width,
                    height: shm.height,
                    pixels,
                });
                captures.fresh = true;
            }
            // Its size changed (the session says the new one, then done:
            // the next frame comes then), or it stopped.
            Event::Failed { .. } => {
                frame.destroy();
                t.frame = None;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, usize> for Captures {
    fn event(
        captures: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        at: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            captures.outputs[*at].1 = Some(name);
        }
    }
}

/// Objects with no events worth hearing.
macro_rules! ignore {
    ($($interface:ty, $data:ty);* $(;)?) => {$(
        impl Dispatch<$interface, $data> for Captures {
            fn event(
                _: &mut Self,
                _: &$interface,
                _: <$interface as wayland_client::Proxy>::Event,
                _: &$data,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore! {
    WlRegistry, GlobalListContents;
    WlShm, ();
    WlShmPool, ();
    WlBuffer, ();
    ExtOutputImageCaptureSourceManagerV1, ();
    ExtImageCaptureSourceV1, ();
    ExtImageCopyCaptureManagerV1, ();
}
