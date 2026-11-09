//! A layer-shell client: a bar, of one colour, along the top of the output.

use super::{Session, wait_for, wayland::tempfile};
use std::{
    io::Write,
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_shm,
        wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
    },
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};

pub use zwlr_layer_shell_v1::Layer as Kind;

pub struct Layer {
    conn: Connection,
    layer: ZwlrLayerSurfaceV1,
    surface: WlSurface,
}

struct Client {
    shm: WlShm,
    surface: WlSurface,
    argb: u32,
    height: u32,
    shown: Arc<AtomicBool>,
}

impl Layer {
    /// A bar called namespace, on kind's layer, height tall along the top
    /// of the output, filled with argb: shown.
    pub fn open(session: &Session, namespace: &str, kind: Kind, height: u32, argb: u32) -> Layer {
        let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
        let conn = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<Client>(&conn).unwrap();
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).unwrap();
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).unwrap();
        let shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).unwrap();
        let surface = compositor.create_surface(&qh, ());
        let layer = shell.get_layer_surface(&surface, None, kind, namespace.into(), &qh, ());
        layer.set_size(0, height);
        layer.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(height as i32);
        surface.commit();
        let shown = Arc::new(AtomicBool::new(false));
        let mut client = Client {
            shm,
            surface: surface.clone(),
            argb,
            height,
            shown: shown.clone(),
        };
        std::thread::spawn(move || while queue.blocking_dispatch(&mut client).is_ok() {});
        wait_for(
            || shown.load(Ordering::Relaxed).then_some(()),
            "the bar to be shown",
        );
        Layer {
            conn,
            layer,
            surface,
        }
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        self.layer.destroy();
        self.surface.destroy();
        let _ = self.conn.flush();
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        client: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, .. } = event {
            layer.ack_configure(serial);
            let (w, h) = (width.max(1) as i32, client.height as i32);
            let mut file = tempfile();
            let pixels: Vec<u8> = (0..w * h).flat_map(|_| client.argb.to_le_bytes()).collect();
            file.write_all(&pixels).unwrap();
            let pool = client.shm.create_pool(file.as_fd(), w * h * 4, qh, ());
            let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
            pool.destroy();
            client.surface.attach(Some(&buffer), 0, 0);
            client.surface.damage_buffer(0, 0, w, h);
            client.surface.commit();
            client.shown.store(true, Ordering::Relaxed);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Client {
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

macro_rules! ignore {
    ($($t:ty),* $(,)?) => {$(
        impl Dispatch<$t, ()> for Client {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore!(
    WlCompositor,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSurface,
    ZwlrLayerShellV1
);
