//! A window manager for tests: it shows one scene on the first output,
//! places every window it hears of at the same rectangle in it, and
//! focuses it. It offers that scene to the picker, as a workspace. It records what wayluck tells it, and does what a test asks
//! (close a window, make it fullscreen). It runs on a thread of its own.

use super::{Session, wait_for};
use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry::WlRegistry,
};

/// Client code made from wayluck's window manager protocol.
mod protocol {
    #![allow(
        dead_code,
        non_camel_case_types,
        non_upper_case_globals,
        unused_imports
    )]
    #![allow(clippy::all)]

    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocol/wayluck-wm-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocol/wayluck-wm-unstable-v1.xml");
}
use protocol::{
    wayluck_wm_manager_v1::{self, WayluckWmManagerV1},
    wayluck_wm_output_v1::{self, WayluckWmOutputV1},
    wayluck_wm_scene_v1::{self, WayluckWmSceneV1},
    wayluck_wm_transaction_v1::{self, WayluckWmTransactionV1},
    wayluck_wm_window_v1::{self, WayluckWmWindowV1},
};

/// Where it places every window, in its scene (x, y, width, height).
pub const PLACE: (i32, i32, i32, i32) = (100, 100, 400, 300);

/// What it's heard of a window.
#[derive(Clone, Debug, Default)]
pub struct Heard {
    /// wayluck's id for it.
    pub id: u32,
    pub title: String,
    pub app_id: String,
    /// Its parent's id, if it has one.
    pub parent: Option<u32>,
    /// min width, min height, max width, max height (0: no limit).
    pub size_hints: Option<[i32; 4]>,
    /// Each fullscreen_request (true) and unfullscreen_request (false).
    pub fullscreen_requests: Vec<bool>,
    pub closed: bool,
}

/// What it's been told so far.
#[derive(Default)]
struct Log {
    ready: bool,
    windows: Vec<Heard>,
    /// Transactions wayluck refused.
    failed: usize,
    /// Workspaces the user chose to share.
    shared: usize,
}

pub struct Wm {
    conn: Connection,
    qh: QueueHandle<Client>,
    log: Arc<Mutex<Log>>,
    windows: Arc<Mutex<Vec<(WayluckWmWindowV1, u32)>>>,
    manager: WayluckWmManagerV1,
    /// Its one scene.
    scene: WayluckWmSceneV1,
}

impl Wm {
    /// Bound as session's window manager, once wayluck has told it
    /// everything there is (ready).
    pub fn start(session: &Session) -> Wm {
        let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
        let conn = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<Client>(&conn).unwrap();
        let qh = queue.handle();
        let manager: WayluckWmManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let windows = Arc::new(Mutex::new(Vec::new()));
        let scene = manager.scene(&qh, ());
        let mut client = Client {
            output: None,
            log: log.clone(),
            windows: windows.clone(),
            scene: scene.clone(),
        };
        std::thread::spawn(move || {
            // Until wayluck goes, at the end of the test.
            while queue.blocking_dispatch(&mut client).is_ok() {}
        });
        let wm = Wm {
            conn,
            qh,
            log,
            windows,
            manager,
            scene,
        };
        wait_for(
            || wm.log.lock().unwrap().ready.then_some(()),
            "the window manager to be told everything",
        );
        wm
    }

    /// The window titled so, once it's heard of it.
    pub fn window(&self, title: &str) -> Heard {
        wait_for(
            || {
                let log = self.log.lock().unwrap();
                log.windows.iter().find(|w| w.title == title).cloned()
            },
            &format!("the window manager to hear of {title}"),
        )
    }

    /// What it's heard of window id, once f is true of it.
    pub fn wait_for(&self, id: u32, what: &str, f: impl Fn(&Heard) -> bool) -> Heard {
        wait_for(
            || {
                let log = self.log.lock().unwrap();
                log.windows.iter().find(|w| w.id == id && f(w)).cloned()
            },
            what,
        )
    }

    /// The titles of the windows it's heard of, so far.
    pub fn titles(&self) -> Vec<String> {
        let log = self.log.lock().unwrap();
        log.windows.iter().map(|w| w.title.clone()).collect()
    }

    /// Transactions wayluck refused, so far.
    pub fn failed(&self) -> usize {
        self.log.lock().unwrap().failed
    }

    /// Workspaces the user chose to share, so far.
    pub fn shared(&self) -> usize {
        self.log.lock().unwrap().shared
    }

    /// Ask window id's client to close it.
    pub fn close(&self, id: u32) {
        self.proxy(id).close();
        self.conn.flush().unwrap();
    }

    /// How windows move: over duration milliseconds, along curve (how far
    /// along at evenly spaced times).
    pub fn set_animation(&self, duration: u32, curve: &[f32]) {
        self.manager.set_animation(duration, floats(curve));
        self.conn.flush().unwrap();
    }

    /// How window id moves, whatever all windows do (None: as they do).
    pub fn set_window_animation(&self, id: u32, animation: Option<(u32, &[f32])>) {
        let window = self.proxy(id);
        match animation {
            Some((duration, curve)) => window.set_animation(duration, floats(curve)),
            None => window.unset_animation(),
        }
        self.conn.flush().unwrap();
    }

    /// Place window id at rect (x, y, width, height) in its scene: instantly
    /// (not animated), or starting from start_from.
    pub fn place(
        &self,
        id: u32,
        rect: (i32, i32, i32, i32),
        instant: bool,
        start_from: Option<(i32, i32, i32, i32)>,
    ) {
        let (x, y, w, h) = rect;
        let window = self.proxy(id);
        let tx = self.manager.transaction(&self.qh, ());
        tx.place(&self.scene, &window, x, y, w, h);
        if instant {
            tx.instant(Some(&window));
        }
        if let Some((x, y, w, h)) = start_from {
            tx.start_from(&window, x, y, w, h);
        }
        tx.commit();
        self.conn.flush().unwrap();
    }

    /// Place windows together, each (id, rect, instantly), in one
    /// transaction; all instantly if all_instant.
    pub fn place_together(&self, windows: &[Placing], all_instant: bool) {
        let tx = self.manager.transaction(&self.qh, ());
        for &(id, (x, y, w, h), instant) in windows {
            let window = self.proxy(id);
            tx.place(&self.scene, &window, x, y, w, h);
            if instant {
                tx.instant(Some(&window));
            }
        }
        if all_instant {
            tx.instant(None);
        }
        tx.commit();
        self.conn.flush().unwrap();
    }

    /// Say where window id starts from, with no place for it.
    pub fn start_from_unplaced(&self, id: u32) {
        let tx = self.manager.transaction(&self.qh, ());
        tx.start_from(&self.proxy(id), 0, 0, 10, 10);
        tx.commit();
        self.conn.flush().unwrap();
    }

    /// Make window id fullscreen (on), or not.
    pub fn fullscreen(&self, id: u32, on: bool) {
        let tx = self.manager.transaction(&self.qh, ());
        tx.fullscreen(&self.proxy(id), on.into());
        tx.commit();
        self.conn.flush().unwrap();
    }

    /// Give window id a shadow: blurred blur pixels, spread, moved by x, y,
    /// in color (0xAARRGGBB).
    pub fn shadow(&self, id: u32, blur: u32, spread: i32, (x, y): (i32, i32), color: u32) {
        let tx = self.manager.transaction(&self.qh, ());
        tx.shadow(&self.proxy(id), blur, spread, x, y, color);
        tx.commit();
        self.conn.flush().unwrap();
    }

    fn proxy(&self, id: u32) -> WayluckWmWindowV1 {
        let windows = self.windows.lock().unwrap();
        let (proxy, _) = windows
            .iter()
            .find(|(_, i)| *i == id)
            .expect("no such window");
        proxy.clone()
    }
}

/// A window to place: its id, where (x, y, width, height), and whether
/// instantly.
pub type Placing = (u32, (i32, i32, i32, i32), bool);

/// A curve's values as the protocol carries them: 32-bit floats in the
/// machine's byte order.
fn floats(curve: &[f32]) -> Vec<u8> {
    curve.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

struct Client {
    scene: WayluckWmSceneV1,
    /// The output it shows its scene on.
    output: Option<WayluckWmOutputV1>,
    log: Arc<Mutex<Log>>,
    windows: Arc<Mutex<Vec<(WayluckWmWindowV1, u32)>>>,
}

impl Client {
    /// What it's heard of the window behind proxy.
    fn heard(&self, proxy: &WayluckWmWindowV1, f: impl FnOnce(&mut Heard)) {
        let Some(id) = self.id_of(proxy) else {
            return;
        };
        let mut log = self.log.lock().unwrap();
        if let Some(heard) = log.windows.iter_mut().find(|w| w.id == id) {
            f(heard);
        }
    }

    fn id_of(&self, proxy: &WayluckWmWindowV1) -> Option<u32> {
        let windows = self.windows.lock().unwrap();
        windows.iter().find(|(p, _)| p == proxy).map(|(_, id)| *id)
    }
}

impl Dispatch<WayluckWmManagerV1, ()> for Client {
    fn event(
        client: &mut Self,
        manager: &WayluckWmManagerV1,
        event: wayluck_wm_manager_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            // Its scene, on every output (there's one, headless).
            wayluck_wm_manager_v1::Event::Output { id } => {
                let tx = manager.transaction(qh, ());
                tx.show(&id, Some(&client.scene));
                tx.commit();
                client.output.get_or_insert(id);
            }
            // Its one workspace.
            wayluck_wm_manager_v1::Event::PickerOpened => {
                if let Some(output) = &client.output {
                    manager.offer_workspace(&client.scene, output, "main".into());
                }
                manager.offers_done();
            }
            wayluck_wm_manager_v1::Event::ShareWorkspace { .. } => {
                client.log.lock().unwrap().shared += 1;
            }
            // Placed and focused at once.
            wayluck_wm_manager_v1::Event::Window { window, id } => {
                let (x, y, w, h) = PLACE;
                let tx = manager.transaction(qh, ());
                tx.place(&client.scene, &window, x, y, w, h);
                tx.focus(Some(&window));
                tx.commit();
                client.windows.lock().unwrap().push((window, id));
                client.log.lock().unwrap().windows.push(Heard {
                    id,
                    ..Heard::default()
                });
            }
            wayluck_wm_manager_v1::Event::Ready => client.log.lock().unwrap().ready = true,
            _ => {}
        }
    }

    wayland_client::event_created_child!(Client, WayluckWmManagerV1, [
        wayluck_wm_manager_v1::EVT_OUTPUT_OPCODE => (WayluckWmOutputV1, ()),
        wayluck_wm_manager_v1::EVT_WINDOW_OPCODE => (WayluckWmWindowV1, ()),
        wayluck_wm_manager_v1::EVT_SHARE_WORKSPACE_OPCODE => (WayluckWmOutputV1, ()),
    ]);
}

impl Dispatch<WayluckWmWindowV1, ()> for Client {
    fn event(
        client: &mut Self,
        proxy: &WayluckWmWindowV1,
        event: wayluck_wm_window_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayluck_wm_window_v1::Event;
        // The parent's id first: heard() holds the log.
        let parent = match &event {
            Event::Parent { parent } => Some(parent.as_ref().and_then(|p| client.id_of(p))),
            _ => None,
        };
        client.heard(proxy, |heard| match event {
            Event::Title { title } => heard.title = title,
            Event::AppId { app_id } => heard.app_id = app_id,
            Event::Parent { .. } => heard.parent = parent.flatten(),
            Event::SizeHints {
                min_width,
                min_height,
                max_width,
                max_height,
            } => heard.size_hints = Some([min_width, min_height, max_width, max_height]),
            Event::FullscreenRequest => heard.fullscreen_requests.push(true),
            Event::UnfullscreenRequest => heard.fullscreen_requests.push(false),
            Event::Closed => heard.closed = true,
            _ => {}
        });
    }
}

impl Dispatch<WayluckWmTransactionV1, ()> for Client {
    fn event(
        client: &mut Self,
        tx: &WayluckWmTransactionV1,
        event: wayluck_wm_transaction_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayluck_wm_transaction_v1::Event::Failed { .. } = event {
            client.log.lock().unwrap().failed += 1;
        }
        tx.destroy();
    }
}

impl Dispatch<WayluckWmOutputV1, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WayluckWmOutputV1,
        _: wayluck_wm_output_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WayluckWmSceneV1, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WayluckWmSceneV1,
        _: wayluck_wm_scene_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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
