//! A Wayland client for tests: one window, filled with a colour, that
//! records what it's told (focus, keys), and copies and pastes. It runs on
//! a thread of its own.

use super::{Session, wait_for};
use std::{
    cell::OnceCell,
    io::{Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback,
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::WlDataDeviceManager,
        wl_data_offer::{self, WlDataOffer},
        wl_data_source::{self, WlDataSource},
        wl_keyboard,
        wl_keyboard::WlKeyboard,
        wl_pointer,
        wl_pointer::WlPointer,
        wl_region::WlRegion,
        wl_registry::WlRegistry,
        wl_seat,
        wl_seat::WlSeat,
        wl_shm,
        wl_shm::WlShm,
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1,
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};
use wayland_protocols::xdg::{
    activation::v1::client::{
        xdg_activation_token_v1::{self, XdgActivationTokenV1},
        xdg_activation_v1::XdgActivationV1,
    },
    shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::{self, XdgToplevel},
        xdg_wm_base::{self, XdgWmBase},
    },
};

/// What the window has been told so far.
#[derive(Default, Clone)]
pub struct Seen {
    /// It's been configured and drawn.
    pub mapped: bool,
    /// It has the keyboard now.
    pub focused: bool,
    /// The pointer is over it now.
    pub pointed: bool,
    /// Keys pressed while it had the keyboard (evdev codes).
    pub keys: Vec<u32>,
    /// Pointer buttons pressed (true) and released (false) over it.
    pub buttons: Vec<(u32, bool)>,
}

pub struct Window {
    seen: Arc<Mutex<Seen>>,
    conn: Connection,
    qh: QueueHandle<Client>,
    clip: Arc<Mutex<Clip>>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
    compositor: WlCompositor,
    surface: WlSurface,
    /// Asking for what's behind it blurred: the global, and its object
    /// once it asks.
    effects: Option<ExtBackgroundEffectManagerV1>,
    effect: OnceCell<ExtBackgroundEffectSurfaceV1>,
    seat: WlSeat,
    activation: Option<XdgActivationV1>,
}

/// A token being made: wayluck's answer, once it's come.
type Token = Arc<Mutex<Option<String>>>;

/// The window's side of the clipboard.
#[derive(Default)]
struct Clip {
    /// The serial of its last keyboard enter: setting the clipboard takes
    /// one (it's the keyboard focus's to set).
    serial: u32,
    /// What it copied, handed to whoever pastes it.
    copied: Option<String>,
    /// Offers made to it, and their mime types, until they're the
    /// clipboard (or not).
    offers: Vec<(WlDataOffer, Vec<String>)>,
    /// What's on the clipboard, as offered to it (while it has the
    /// keyboard).
    selection: Option<(WlDataOffer, Vec<String>)>,
}

/// The mime type text is copied and pasted as.
const TEXT: &str = "text/plain;charset=utf-8";

impl Window {
    /// A window of session's ("victim"), filled with argb (0xAARRGGBB), mapped.
    pub fn open(session: &Session, argb: u32) -> Window {
        Self::titled(session, argb, "victim")
    }

    /// A window of session's, filled with argb (0xAARRGGBB), mapped.
    pub fn titled(session: &Session, argb: u32, title: &str) -> Window {
        let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
        let conn = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<Client>(&conn).unwrap();
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).unwrap();
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).unwrap();
        let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).unwrap();
        let seat: WlSeat = globals.bind(&qh, 5..=9, ()).unwrap();
        let manager: WlDataDeviceManager = globals.bind(&qh, 3..=3, ()).unwrap();
        let device = manager.get_data_device(&seat, &qh, ());
        let effects = globals.bind(&qh, 1..=1, ()).ok();
        let activation = globals.bind(&qh, 1..=1, ()).ok();
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title(title.into());
        surface.commit();

        let seen = Arc::new(Mutex::new(Seen::default()));
        let clip = Arc::new(Mutex::new(Clip::default()));
        let mut client = Client {
            seen: seen.clone(),
            clip: clip.clone(),
            shm,
            surface: surface.clone(),
            argb,
            size: (200, 200),
        };
        std::thread::spawn(move || {
            // Until wayluck goes, at the end of the test.
            while queue.blocking_dispatch(&mut client).is_ok() {}
        });
        let window = Window {
            seen,
            conn,
            qh,
            clip,
            manager,
            device,
            compositor,
            surface,
            effects,
            effect: OnceCell::new(),
            activation,
            seat,
        };
        wait_for(
            || window.seen().mapped.then_some(()),
            "the window to be mapped",
        );
        window
    }

    /// Ask to be brought forward (xdg-activation): with a token from its
    /// last keyboard enter (with_input), or from nothing.
    pub fn ask_activation(&self, with_input: bool) {
        let activation = self.activation.as_ref().expect("no xdg_activation_v1");
        let made: Token = Arc::default();
        let token = activation.get_activation_token(&self.qh, made.clone());
        if with_input {
            token.set_serial(self.clip.lock().unwrap().serial, &self.seat);
        }
        token.set_surface(&self.surface);
        token.commit();
        self.conn.flush().unwrap();
        let made = wait_for(|| made.lock().unwrap().clone(), "a token");
        activation.activate(made, &self.surface);
        self.conn.flush().unwrap();
    }

    /// Ask for what's behind it blurred, where rect is (x, y, width,
    /// height, from its top left); None: nowhere.
    pub fn ask_blur(&self, rect: Option<(i32, i32, i32, i32)>) {
        let manager = (self.effects.as_ref()).expect("no ext_background_effect_manager_v1");
        let effect = (self.effect)
            .get_or_init(|| manager.get_background_effect(&self.surface, &self.qh, ()));
        match rect {
            Some((x, y, w, h)) => {
                let region = self.compositor.create_region(&self.qh, ());
                region.add(x, y, w, h);
                effect.set_blur_region(Some(&region));
                region.destroy();
            }
            None => effect.set_blur_region(None),
        }
        self.surface.commit();
        self.conn.flush().unwrap();
    }

    /// What it's been told, so far.
    pub fn seen(&self) -> Seen {
        self.seen.lock().unwrap().clone()
    }

    /// Copy text (it needs the keyboard for that), and wait until wayluck
    /// has handled it.
    pub fn copy(&self, text: &str) {
        let mut clip = self.clip.lock().unwrap();
        clip.copied = Some(text.into());
        let source = self.manager.create_data_source(&self.qh, ());
        source.offer(TEXT.into());
        self.device.set_selection(Some(&source), clip.serial);
        drop(clip);
        self.sync();
    }

    /// Paste what's on the clipboard, as offered to it now (once wayluck
    /// has told it all there is). None: nothing is offered.
    pub fn paste(&self) -> Option<String> {
        self.sync();
        let (offer, mimes) = self.clip.lock().unwrap().selection.clone()?;
        if !mimes.iter().any(|m| m == TEXT) {
            return None;
        }
        let (mut reader, writer) = std::io::pipe().unwrap();
        offer.receive(TEXT.into(), writer.as_fd());
        drop(writer);
        self.conn.flush().unwrap();
        // Read on a thread of its own: the other end may never write.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = reader.read_to_string(&mut text);
            let _ = tx.send(text);
        });
        Some(
            rx.recv_timeout(super::PATIENCE)
                .expect("the paste never came"),
        )
    }

    /// Wait until wayluck has handled everything sent so far, and the
    /// window everything wayluck sent before answering.
    fn sync(&self) {
        let done = Arc::new(AtomicBool::new(false));
        self.conn.display().sync(&self.qh, done.clone());
        self.conn.flush().unwrap();
        wait_for(
            || done.load(Ordering::Relaxed).then_some(()),
            "wayluck to answer",
        );
    }
}

struct Client {
    seen: Arc<Mutex<Seen>>,
    clip: Arc<Mutex<Clip>>,
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
pub(super) fn tempfile() -> std::fs::File {
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
            // Mapped once wayluck has handled the commit (and put the
            // window where a click finds it): it answers this after.
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
            wl_keyboard::Event::Enter { serial, .. } => {
                seen.focused = true;
                client.clip.lock().unwrap().serial = serial;
            }
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
            wl_pointer::Event::Button { button, state, .. } => {
                let pressed =
                    state == wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed);
                seen.buttons.push((button, pressed));
            }
            _ => {}
        }
    }
}

/// A sync's answer: done.
impl Dispatch<WlCallback, Arc<AtomicBool>> for Client {
    fn event(
        _: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        done: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            done.store(true, Ordering::Relaxed);
        }
    }
}

/// Offers of what's on the clipboard, and which is on it now.
impl Dispatch<WlDataDevice, ()> for Client {
    fn event(
        client: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut clip = client.clip.lock().unwrap();
        match event {
            wl_data_device::Event::DataOffer { id } => clip.offers.push((id, Vec::new())),
            wl_data_device::Event::Selection { id } => {
                clip.selection = id.and_then(|id| {
                    let i = clip.offers.iter().position(|(o, _)| *o == id)?;
                    Some(clip.offers.swap_remove(i))
                });
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Client, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

/// An offer's mime types.
impl Dispatch<WlDataOffer, ()> for Client {
    fn event(
        client: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            let mut clip = client.clip.lock().unwrap();
            if let Some((_, mimes)) = clip.offers.iter_mut().find(|(o, _)| o == offer) {
                mimes.push(mime_type);
            }
        }
    }
}

/// Someone pastes what it copied: they get it.
impl Dispatch<WlDataSource, ()> for Client {
    fn event(
        client: &mut Self,
        _: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_source::Event::Send { fd, .. } = event {
            let copied = client
                .clip
                .lock()
                .unwrap()
                .copied
                .clone()
                .unwrap_or_default();
            let _ = std::fs::File::from(fd).write_all(copied.as_bytes());
        }
    }
}

/// Objects whose events don't matter here.
macro_rules! ignore {
    ($($t:ty),* $(,)?) => {$(
        impl Dispatch<$t, ()> for Client {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(),
                _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(
    WlCompositor,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSurface,
    WlDataDeviceManager,
    WlRegion,
    ExtBackgroundEffectManagerV1,
    ExtBackgroundEffectSurfaceV1,
    XdgActivationV1,
);

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

impl Dispatch<XdgActivationTokenV1, Token> for Client {
    fn event(
        _: &mut Self,
        _: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        made: &Token,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            *made.lock().unwrap() = Some(token);
        }
    }
}
