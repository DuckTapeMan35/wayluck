//! Live thumbnails: each monitor and window captured (ext-image-copy-capture;
//! a window found by its id in ext-foreign-toplevel-list), frame after
//! frame, small (wayluck draws them shrunk: wayluck_picker_host_v1's
//! thumbnail), over a connection of the picker's own. A frame comes into a
//! dmabuf on wayluck's GPU when it offers one (the picker draws it from
//! there: nothing is copied), else into shared memory. The next is asked for
//! once the last was drawn (next), so frames come no faster than the picker
//! shows them.

use crate::protocol::picker::client::wayluck_picker_host_v1::WayluckPickerHostV1;
use smithay::{
    backend::{
        allocator::{
            Allocator, Buffer as _, Fourcc, Modifier,
            dmabuf::{AsDmabuf, Dmabuf},
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{DrmNode, NodeType},
    },
    reexports::rustix::fs::{MemfdFlags, memfd_create},
};
use std::{
    fs::File,
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::FileExt,
    },
};
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
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    },
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};

/// What a thumbnail shows.
#[derive(Clone)]
pub enum Source {
    /// A monitor, by its wl_output's name.
    Monitor(String),
    /// A window, by its ext-foreign-toplevel-list identifier.
    Window(u32),
    /// A workspace, by its id from wayluck.
    Workspace(u32),
}

pub struct Captures {
    connection: Connection,
    shm: WlShm,
    /// To hand dmabufs over, if wayluck takes them.
    linux_dmabuf: Option<ZwpLinuxDmabufV1>,
    /// The GPU dmabufs are made on, once a session says which.
    gpu: Option<(u64, GbmAllocator<File>)>,
    /// One for each monitor asked for, in the order asked.
    pub thumbnails: Vec<Thumbnail>,
    /// The monitors' wl_outputs, and their names once told.
    outputs: Vec<(WlOutput, Option<String>)>,
    /// The windows' handles, and their ids once told.
    toplevels: Vec<(ExtForeignToplevelHandleV1, Option<String>)>,
    /// A thumbnail got a new frame since this was last cleared.
    pub fresh: bool,
}

#[derive(Default)]
pub struct Thumbnail {
    session: Option<ExtImageCopyCaptureSessionV1>,
    /// The size the session asks for (as it says, then done).
    asked: (i32, i32),
    /// The GPU it may take a dmabuf on, and the modifiers it takes for
    /// XRGB8888 (as it says, then done; none: only shared memory).
    asked_dmabuf: Option<(u64, Vec<Modifier>)>,
    buffer: Option<Buffer>,
    /// The frame being captured, if one is.
    frame: Option<ExtImageCopyCaptureFrameV1>,
    /// The newest frame, until the picker takes it to draw.
    pub image: Option<Image>,
}

/// A frame, to draw.
pub enum Image {
    /// Copied out of shared memory: XRGB8888, rows of width * 4 bytes.
    Pixels {
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    },
    /// Drawn into this dmabuf, on the GPU.
    Dmabuf(Dmabuf),
}

/// What a thumbnail's frames are captured into.
struct Buffer {
    buffer: WlBuffer,
    width: i32,
    height: i32,
    memory: Memory,
}

enum Memory {
    /// Shared memory: the client's side of it is a file.
    Shm(File),
    Dmabuf(Dmabuf),
}

impl Captures {
    /// Start capturing these sources, a thumbnail for each (host makes
    /// them), its frames no bigger than fit. The queue is to be dispatched
    /// with the captures as its connection has events (window.rs).
    pub fn start(
        connection: Connection,
        host: &WayluckPickerHostV1,
        sources: &[Source],
        fit: (u32, u32),
    ) -> Result<(Captures, EventQueue<Captures>), Box<dyn std::error::Error>> {
        let (globals, mut queue) = registry_queue_init::<Captures>(&connection)?;

        let qh = queue.handle();

        let shm: WlShm = globals.bind(&qh, 1..=1, ())?;
        let output_sources: ExtOutputImageCaptureSourceManagerV1 = globals.bind(&qh, 1..=1, ())?;
        // Windows, if they can be captured.
        let window_sources: Option<ExtForeignToplevelImageCaptureSourceManagerV1> =
            globals.bind(&qh, 1..=1, ()).ok();
        let list: Option<ExtForeignToplevelListV1> = globals.bind(&qh, 1..=1, ()).ok();
        // Version 3: buffers made at once (create_immed).
        let linux_dmabuf = globals.bind(&qh, 3..=3, ()).ok();
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
            linux_dmabuf,
            gpu: None,
            thumbnails: Vec::new(),
            outputs,
            toplevels: Vec::new(),
            fresh: false,
        };
        // The outputs' names, and the windows with their ids.
        queue.roundtrip(&mut captures)?;
        for (at, wanted) in sources.iter().enumerate() {
            let mut thumbnail = Thumbnail::default();
            let source = match wanted {
                Source::Monitor(name) => (captures.outputs.iter())
                    .find(|(_, n)| n.as_ref() == Some(name))
                    .map(|(output, _)| output_sources.create_source(output, &qh, ())),
                Source::Window(id) => {
                    let id = id.to_string();
                    (captures.toplevels.iter())
                        .find(|(_, i)| i.as_ref() == Some(&id))
                        .zip(window_sources.as_ref())
                        .map(|((handle, _), manager)| manager.create_source(handle, &qh, ()))
                }
                Source::Workspace(id) => Some(host.workspace_source(*id, &qh, ())),
            };
            if let Some(source) = source {
                let (w, h) = fit;
                thumbnail.session = Some(host.thumbnail(&source, w, h, &qh, at));
                source.destroy();
            }
            captures.thumbnails.push(thumbnail);
        }
        // Windows made from now on have no thumbnail: no more news of them.
        if let Some(list) = list {
            list.stop();
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
        let fits = (self.thumbnails[at].buffer.as_ref())
            .is_some_and(|b| (b.width, b.height) == (width, height));
        if !fits {
            if let Some(old) = self.thumbnails[at].buffer.take() {
                old.buffer.destroy();
            }
            // A dmabuf if wayluck takes one and one can be made; shared
            // memory if not.
            let buffer = match self.dmabuf_buffer(at, qh) {
                Some(Ok(buffer)) => Ok(buffer),
                Some(Err(e)) => {
                    eprintln!("wayluck pick: no dmabuf for a thumbnail ({e}): shared memory");
                    shm_buffer(&self.shm, width, height, qh)
                }
                None => shm_buffer(&self.shm, width, height, qh),
            };
            match buffer {
                Ok(buffer) => self.thumbnails[at].buffer = Some(buffer),
                Err(e) => return eprintln!("wayluck pick: a thumbnail's buffer: {e}"),
            }
        }
        let t = &mut self.thumbnails[at];
        let Some(buffer) = &t.buffer else { return };
        let frame = session.create_frame(qh, at);
        frame.attach_buffer(&buffer.buffer);
        frame.damage_buffer(0, 0, width, height);
        frame.capture();
        t.frame = Some(frame);
    }

    /// A dmabuf of the size thumbnail at asks for, on the GPU it says;
    /// None if it takes none (or wayluck can't be handed one).
    fn dmabuf_buffer(
        &mut self,
        at: usize,
        qh: &QueueHandle<Captures>,
    ) -> Option<Result<Buffer, Box<dyn std::error::Error>>> {
        let t = &self.thumbnails[at];
        let (device, modifiers) = t.asked_dmabuf.clone()?;
        let (width, height) = t.asked;
        let linux_dmabuf = self.linux_dmabuf.as_ref()?;
        let made = (|| {
            if self.gpu.as_ref().is_none_or(|(d, _)| *d != device) {
                self.gpu = Some((device, open_gpu(device)?));
            }
            let (_, allocator) = self.gpu.as_mut().ok_or("no GPU")?;
            let gbm = allocator.create_buffer(
                width as u32,
                height as u32,
                Fourcc::Xrgb8888,
                &modifiers,
            )?;
            let dmabuf = gbm.export()?;
            // Handed over: each plane, then the buffer made of them.
            let params = linux_dmabuf.create_params(qh, ());
            let modifier = u64::from(dmabuf.format().modifier);
            let planes = (dmabuf.handles())
                .zip(dmabuf.offsets())
                .zip(dmabuf.strides())
                .enumerate();
            for (plane, ((fd, offset), stride)) in planes {
                let (hi, lo) = ((modifier >> 32) as u32, modifier as u32);
                params.add(fd, plane as u32, offset, stride, hi, lo);
            }
            let flags = zwp_linux_buffer_params_v1::Flags::empty();
            let format = Fourcc::Xrgb8888 as u32;
            let buffer = params.create_immed(width, height, format, flags, qh, ());
            params.destroy();
            Ok(Buffer {
                buffer,
                width,
                height,
                memory: Memory::Dmabuf(dmabuf),
            })
        })();
        Some(made)
    }
}

/// The GPU a session named (by its device number), to make dmabufs on: its
/// render node.
fn open_gpu(device: u64) -> Result<GbmAllocator<File>, Box<dyn std::error::Error>> {
    let node = DrmNode::from_dev_id(device)?;
    let path = (node.dev_path_with_type(NodeType::Render)).ok_or("the GPU has no render node")?;
    let file = File::options().read(true).write(true).open(path)?;
    let device = GbmDevice::new(file)?;
    Ok(GbmAllocator::new(device, GbmBufferFlags::RENDERING))
}

/// A buffer of width × height XRGB8888 pixels in a new memfd.
fn shm_buffer(
    shm: &WlShm,
    width: i32,
    height: i32,
    qh: &QueueHandle<Captures>,
) -> Result<Buffer, Box<dyn std::error::Error>> {
    let stride = width * 4;
    let len = stride * height;
    let fd: OwnedFd = memfd_create("wayluck-pick", MemfdFlags::CLOEXEC)?;
    let file = File::from(fd);
    file.set_len(len as u64)?;
    let pool: WlShmPool = shm.create_pool(file.as_fd(), len, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    Ok(Buffer {
        buffer,
        width,
        height,
        memory: Memory::Shm(file),
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
            // What to give, again from the start (the size comes first).
            Event::BufferSize { width, height } => {
                t.asked = (width as i32, height as i32);
                t.asked_dmabuf = None;
            }
            Event::DmabufDevice { device } => {
                let device = device.try_into().map(u64::from_ne_bytes);
                if let Ok(device) = device {
                    t.asked_dmabuf = Some((device, Vec::new()));
                }
            }
            Event::DmabufFormat { format, modifiers } => {
                if format == Fourcc::Xrgb8888 as u32
                    && let Some((_, taken)) = &mut t.asked_dmabuf
                {
                    // Whole 8-byte modifiers (a stray end, if any, is dropped).
                    let (each, _) = modifiers.as_chunks::<8>();
                    taken.extend(each.iter().map(|m| Modifier::from(u64::from_ne_bytes(*m))));
                }
            }
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
            // XRGB8888 is what it's given, in shared memory or a dmabuf.
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
                let Some(buffer) = &t.buffer else { return };
                t.image = Some(match &buffer.memory {
                    Memory::Dmabuf(dmabuf) => Image::Dmabuf(dmabuf.clone()),
                    Memory::Shm(file) => {
                        let (width, height) = (buffer.width, buffer.height);
                        let mut pixels = vec![0; (width * height * 4) as usize];
                        if let Err(e) = file.read_exact_at(&mut pixels, 0) {
                            return eprintln!("wayluck pick: reading a thumbnail: {e}");
                        }
                        Image::Pixels {
                            width,
                            height,
                            pixels,
                        }
                    }
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

impl Dispatch<ExtForeignToplevelListV1, ()> for Captures {
    fn event(
        captures: &mut Self,
        list: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                captures.toplevels.push((toplevel, None));
            }
            ext_foreign_toplevel_list_v1::Event::Finished => list.destroy(),
            _ => {}
        }
    }

    // The toplevel event makes a handle.
    wayland_client::event_created_child!(Captures, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Captures {
    fn event(
        captures: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } = event
            && let Some((_, id)) = captures.toplevels.iter_mut().find(|(h, _)| h == handle)
        {
            *id = Some(identifier);
        }
    }
}

ignore! {
    WlRegistry, GlobalListContents;
    WlShm, ();
    WlShmPool, ();
    WlBuffer, ();
    ExtOutputImageCaptureSourceManagerV1, ();
    ExtImageCaptureSourceV1, ();
    ZwpLinuxDmabufV1, ();
    ZwpLinuxBufferParamsV1, ();
    ExtForeignToplevelImageCaptureSourceManagerV1, ();
}
