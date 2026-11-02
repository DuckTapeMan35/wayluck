//! A Wayland client for tests: one window, filled with a colour, that
//! records what it's told (focus, keys). It runs on a thread of its own.

use super::{Session, wait_for};
use std::{
    io::Write,
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{Arc, Mutex},
};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer, wl_callback, wl_callback::WlCallback, wl_compositor::WlCompositor,
        wl_keyboard, wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer,
        wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
        wl_shm_pool::WlShmPool, wl_surface::WlSurface,
    },
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

/// What the window has been told so far.
#[derive(Default, Clone)]
pub struct Seen {
    /// It's been configured and drawn.
    pub mapped: bool,
    /// It has the keyboard now.
    pub focused: bool,
    /// Keys pressed while it had the keyboard (evdev codes).
    pub keys: Vec<u32>,
    /// The pointer is over it now.
    pub pointed: bool,
}

pub struct Window {
    seen: Arc<Mutex<Seen>>,
}

impl Window {
    /// A window of session's, filled with argb (0xAARRGGBB), mapped.
    pub fn open(session: &Session, argb: u32) -> Window {
        let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
        let conn = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<Client>(&conn).unwrap();
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).unwrap();
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).unwrap();
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).unwrap();
        let _seat: WlSeat = globals.bind(&qh, 5..=9, ()).unwrap();
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title("victim".into());
        surface.commit();

        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut client = Client {
            seen: seen.clone(),
            shm,
            surface,
            argb,
            size: (200, 200),
        };
        std::thread::spawn(move || {
            // Until wayluck goes, at the end of the test.
            while queue.blocking_dispatch(&mut client).is_ok() {}
        });
        let window = Window { seen };
        wait_for(
            || window.seen().mapped.then_some(()),
            "the window to be mapped",
        );
        window
    }

    /// What it's been told, so far.
    pub fn seen(&self) -> Seen {
        self.seen.lock().unwrap().clone()
    }
}

struct Client {
    seen: Arc<Mutex<Seen>>,
    shm: WlShm,
    surface: WlSurface,
    argb: u32,
    size: (i32, i32),
}

impl Client {
    /// A buffer of its size, all its colour, attached and committed.
    fn draw(&mut self, qh: &QueueHandle<Client>) {
        let (w, h) = self.size;
        let mut file = tempfile();
        let pixels: Vec<u8> = (0..w * h).flat_map(|_| self.argb.to_le_bytes()).collect();
        file.write_all(&pixels).unwrap();
        let pool = self.shm.create_pool(file.as_fd(), w * h * 4, qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, w, h);
        self.surface.commit();
    }
}

/// An unnamed file for a buffer's pixels.
fn tempfile() -> std::fs::File {
    let path = std::env::temp_dir().join(format!(
        "wayluck-test-buffer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file
}

impl Dispatch<XdgSurface, ()> for Client {
    fn event(
        client: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            client.draw(qh);
            conn.display().sync(qh, ());
        }
    }
}

impl Dispatch<WlCallback, ()> for Client {
    fn event(
        client: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            client.seen.lock().unwrap().mapped = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Client {
    fn event(
        client: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event
            && width > 0
            && height > 0
        {
            client.size = (width, height);
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<WlSeat, ()> for Client {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event
            && let wayland_client::WEnum::Value(caps) = capabilities
        {
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Client {
    fn event(
        client: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut seen = client.seen.lock().unwrap();
        match event {
            wl_keyboard::Event::Enter { .. } => seen.focused = true,
            wl_keyboard::Event::Leave { .. } => seen.focused = false,
            wl_keyboard::Event::Key {
                key,
                state: wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => seen.keys.push(key),
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for Client {
    fn event(
        client: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut seen = client.seen.lock().unwrap();
        match event {
            wl_pointer::Event::Enter { .. } => seen.pointed = true,
            wl_pointer::Event::Leave { .. } => seen.pointed = false,
            _ => {}
        }
    }
}

/// Objects whose events don't matter here.
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Client {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(),
                _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlCompositor, WlShm, WlShmPool, WlBuffer, WlSurface);

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

/// What a client listing windows (ext-foreign-toplevel-list, like lswt) is
/// told: each window's title and app id.
pub fn list_windows(session: &Session) -> Vec<(String, String)> {
    use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    };

    #[derive(Default)]
    struct Lister {
        /// By handle: title, app id.
        windows: Vec<(ExtForeignToplevelHandleV1, String, String)>,
    }

    impl Dispatch<ExtForeignToplevelListV1, ()> for Lister {
        fn event(
            lister: &mut Self,
            _: &ExtForeignToplevelListV1,
            event: ext_foreign_toplevel_list_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
                lister
                    .windows
                    .push((toplevel, String::new(), String::new()));
            }
        }

        wayland_client::event_created_child!(Lister, ExtForeignToplevelListV1, [
            ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
        ]);
    }

    impl Dispatch<ExtForeignToplevelHandleV1, ()> for Lister {
        fn event(
            lister: &mut Self,
            handle: &ExtForeignToplevelHandleV1,
            event: ext_foreign_toplevel_handle_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let Some(entry) = lister.windows.iter_mut().find(|(h, ..)| h == handle) else {
                return;
            };
            match event {
                ext_foreign_toplevel_handle_v1::Event::Title { title } => entry.1 = title,
                ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => entry.2 = app_id,
                _ => {}
            }
        }
    }

    impl Dispatch<WlRegistry, GlobalListContents> for Lister {
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

    let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
    let conn = Connection::from_socket(stream).unwrap();
    let (globals, mut queue) = registry_queue_init::<Lister>(&conn).unwrap();
    let _list: ExtForeignToplevelListV1 = globals.bind(&queue.handle(), 1..=1, ()).unwrap();
    let mut lister = Lister::default();
    // The windows, then what each is called.
    queue.roundtrip(&mut lister).unwrap();
    queue.roundtrip(&mut lister).unwrap();
    (lister.windows.into_iter())
        .map(|(_, title, app_id)| (title, app_id))
        .collect()
}
