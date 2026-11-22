//! A window manager for tests: it shows one scene on the first output,
//! places every window it hears of at the same rectangle in it, and
//! focuses it. It offers that scene to the picker, as a workspace.
//! It records what wayluck tells it, and does what a test asks
//! (close a window, make it fullscreen). It runs on a thread of its own.

use super::{Session, wait_for};
use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
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
    wayluck_wm_layer_v1::{self, WayluckWmLayerV1},
    wayluck_wm_manager_v1::{self, WayluckWmManagerV1},
    wayluck_wm_output_v1::{self, WayluckWmOutputV1},
    wayluck_wm_scene_v1::{self, WayluckWmSceneV1},
    wayluck_wm_transaction_v1::{self, WayluckWmTransactionV1},
    wayluck_wm_window_v1::{self, WayluckWmWindowV1},
    wayluck_wm_workspace_v1::{self, WayluckWmWorkspaceV1},
};

/// A window to place: its id, where (x, y, width, height), and whether
/// instantly.
pub type Placing = (u32, (i32, i32, i32, i32), bool);

/// Changes to make in a transaction.
type Build = Box<dyn FnOnce(&WayluckWmTransactionV1) + Send>;

/// A window manager's workspaces, by name.
type Workspaces = Arc<Mutex<Vec<(String, WayluckWmWorkspaceV1)>>>;

/// Where it places every window, in its scene (x, y, width, height).
pub const PLACE: (i32, i32, i32, i32) = (100, 100, 400, 300);

/// What it's heard of a window.
#[derive(Clone, Debug, Default)]
pub struct Heard {
    /// wayluck's id for it.
    pub id: u32,
    pub title: String,
    pub app_id: String,
    /// The workspaces it’s been asked to be on, by name (None: all)
    pub workspace_requests: Vec<Option<String>>,
    /// Its parent's id, if it has one.
    pub parent: Option<u32>,
    /// min width, min height, max width, max height (0: no limit).
    pub size_hints: Option<[i32; 4]>,
    /// Each fullscreen_request (true) and unfullscreen_request (false).
    pub fullscreen_requests: Vec<bool>,
    /// The kind it's said to be (the protocol's value), once it's told.
    pub kind: Option<u32>,
    /// Its hints (the protocol's bits), once it's told.
    pub hints: Option<u32>,
    /// Whether it wants attention, once it's told.
    pub attention: Option<bool>,
    /// How many times it's asked to be brought forward.
    pub activate_requests: usize,
    /// How many dones it's had.
    pub dones: usize,
    /// What it had been told at its first done (without this).
    pub at_first_done: Option<Box<Heard>>,
    pub closed: bool,
}

/// What it's heard of an output.
#[derive(Clone, Debug, Default)]
pub struct HeardOutput {
    pub name: String,
    /// Where it is (x, y, width, height).
    pub geometry: Option<(i32, i32, i32, i32)>,
    /// It's gone.
    pub removed: bool,
    /// Its object, to put workspaces on it.
    proxy: Option<WayluckWmOutputV1>,
    /// Its object's id, to tell outputs apart.
    object: u32,
}

/// What it's heard of a layer surface.
#[derive(Clone, Debug, Default)]
pub struct HeardLayer {
    pub namespace: String,
    /// Its layer (the protocol's value), once it's told.
    pub layer: Option<u32>,
    /// It's on an output the window manager knows.
    pub on_output: bool,
    pub dones: usize,
    pub closed: bool,
}

/// What it's been told so far.
#[derive(Default)]
struct Log {
    ready: bool,
    windows: Vec<Heard>,
    /// New windows are left for the test to place.
    unplaced: bool,
    /// Transactions wayluck refused.
    failed: usize,
    /// Workspaces the user chose to share.
    shared: usize,
    /// What its outputs leave windows (x, y, width, height): the last heard.
    usable: Option<(i32, i32, i32, i32)>,
    /// Outputs, as heard of.
    outputs: Vec<HeardOutput>,
    /// Layer surfaces, as heard of.
    layers: Vec<HeardLayer>,
    /// The workspaces asked to be shown, by name, in order
    workspaces_asked: Vec<String>,
    /// Transactions committed and not answered yet: the next waits for none.
    unanswered: u32,
    /// Changes from wayluck's events that wait for it (made in one, then).
    later: Vec<Build>,
}

pub struct Wm {
    conn: Connection,
    qh: QueueHandle<Client>,
    log: Arc<Mutex<Log>>,
    windows: Arc<Mutex<Vec<(WayluckWmWindowV1, u32)>>>,
    manager: WayluckWmManagerV1,
    layers: Arc<Mutex<Vec<WayluckWmLayerV1>>>,
    workspaces: Workspaces,
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
        let layers = Arc::new(Mutex::default());
        let workspaces = Workspaces::default();
        let mut client = Client {
            manager: manager.clone(),
            output: None,
            log: log.clone(),
            windows: windows.clone(),
            scene: scene.clone(),
            layers: layers.clone(),
            workspaces: workspaces.clone(),
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
            layers,
            workspaces,
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

    /// A transaction built by build, committed once the last one is
    /// answered: wayluck refuses one sooner.
    fn send(&self, build: impl FnOnce(&WayluckWmTransactionV1)) {
        let mut log = wait_for(
            || {
                let log = self.log.lock().unwrap();
                (log.unanswered == 0).then_some(log)
            },
            "the last transaction to be answered",
        );
        commit(&mut log, &self.manager, &self.qh, build);
        drop(log);
        self.conn.flush().unwrap();
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

    /// What its output leaves windows, once it's heard (x, y, width, height).
    pub fn usable(&self) -> Option<(i32, i32, i32, i32)> {
        self.log.lock().unwrap().usable
    }

    /// The output called name, once it's heard where it is.
    pub fn output(&self, name: &str) -> HeardOutput {
        wait_for(
            || {
                let log = self.log.lock().unwrap();
                (log.outputs.iter().rev())
                    .find(|o| o.name == name && o.geometry.is_some())
                    .cloned()
            },
            &format!("the output {name}"),
        )
    }

    /// Make a virtual output, width by height, modelled on nothing. Kept
    /// as long as the window manager is.
    pub fn virtual_output(&self, width: i32, height: i32) {
        let _kept = self
            .manager
            .virtual_output(width, height, None, 60_000, &self.qh, ());
        self.conn.flush().unwrap();
    }

    /// The layer surface called namespace, once it's all heard (its first done).
    pub fn layer(&self, namespace: &str) -> HeardLayer {
        wait_for(
            || {
                let log = self.log.lock().unwrap();
                let found = log
                    .layers
                    .iter()
                    .find(|l| l.namespace == namespace && l.dones > 0);
                found.cloned()
            },
            &format!("the layer surface {namespace:?}"),
        )
    }

    /// Wait for the layer surface called namespace to be as f says.
    pub fn wait_for_layer(&self, namespace: &str, what: &str, f: impl Fn(&HeardLayer) -> bool) {
        wait_for(
            || {
                let log = self.log.lock().unwrap();
                (log.layers.iter())
                    .any(|l| l.namespace == namespace && f(l))
                    .then_some(())
            },
            what,
        );
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

    /// From now on, new windows are left for the test to place.
    pub fn stop_placing(&self) {
        self.log.lock().unwrap().unplaced = true;
    }

    /// How windows move: over duration milliseconds, along curve (how far
    /// along at evenly spaced times).
    pub fn set_curve(&self, duration: u32, curve: &[f32]) {
        self.manager.set_curve(duration, floats(curve));
        self.conn.flush().unwrap();
    }

    /// Windows pulled to where they're placed by a spring: how hard (per
    /// second squared) and how damped (1: no overshooting).
    pub fn set_spring(&self, stiffness: f64, damping: f64) {
        self.manager.set_spring(stiffness, damping);
        self.conn.flush().unwrap();
    }

    /// How window id moves, whatever all windows do (None: as they do).
    pub fn set_window_curve(&self, id: u32, curve: Option<(u32, &[f32])>) {
        let window = self.proxy(id);
        match curve {
            Some((duration, curve)) => window.set_curve(duration, floats(curve)),
            None => window.unset_animation(),
        }
        self.conn.flush().unwrap();
    }

    /// The workspaces asked to be shown so far, by name, in order.
    pub fn workspaces_asked(&self) -> Vec<String> {
        self.log.lock().unwrap().workspaces_asked.clone()
    }

    /// Cut the layer surface called namespace's corners off, rounded radius
    /// pixels.
    pub fn layer_corners(&self, namespace: &str, radius: u32) {
        let layer = self.layer_proxy(namespace);
        self.send(|tx| {
            tx.layer_corners(&layer, radius);
        });
    }

    /// Give the layer surface called namespace a shadow, as shadow does a
    /// window.
    pub fn layer_shadow(
        &self,
        namespace: &str,
        blur: u32,
        spread: i32,
        (x, y): (i32, i32),
        color: u32,
    ) {
        let layer = self.layer_proxy(namespace);
        self.send(|tx| {
            tx.layer_shadow(&layer, blur, spread, x, y, color);
        });
    }

    fn layer_proxy(&self, namespace: &str) -> WayluckWmLayerV1 {
        let log = self.log.lock().unwrap();
        let at = (log.layers.iter())
            .position(|l| l.namespace == namespace && !l.closed)
            .expect("no such layer surface");
        self.layers.lock().unwrap()[at].clone()
    }

    /// Blur behind the layer surface called namespace: 0 where its client
    /// asks, 1 nowhere, 2 behind all of it.
    pub fn layer_blur(&self, namespace: &str, mode: u32) {
        let layer = self.layer_proxy(namespace);
        self.send(|tx| {
            tx.layer_blur(&layer, mode.try_into().unwrap());
        });
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
        self.send(|tx| {
            tx.place(&self.scene, &window, x, y, w, h);
            if instant {
                tx.instant(Some(&window));
            }
            if let Some((x, y, w, h)) = start_from {
                tx.start_from(&window, x, y, w, h);
            }
        });
    }

    /// Place windows together, each (id, rect, instantly), in one
    /// transaction; all instantly if all_instant.
    pub fn place_together(&self, windows: &[Placing], all_instant: bool) {
        self.send(|tx| {
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
        });
    }

    /// Say where window id starts from, with no place for it.
    pub fn start_from_unplaced(&self, id: u32) {
        self.send(|tx| {
            tx.start_from(&self.proxy(id), 0, 0, 10, 10);
        });
    }

    /// Make window id fullscreen (on), or not.
    pub fn fullscreen(&self, id: u32, on: bool) {
        self.send(|tx| {
            tx.fullscreen(&self.proxy(id), on.into());
        });
    }

    /// Give window id a shadow: blurred blur pixels, spread, moved by x, y,
    /// in color (0xAARRGGBB).
    pub fn shadow(&self, id: u32, blur: u32, spread: i32, (x, y): (i32, i32), color: u32) {
        self.send(|tx| {
            tx.shadow(&self.proxy(id), blur, spread, x, y, color);
        });
    }

    /// Give window id the keyboard.
    pub fn focus(&self, id: u32) {
        self.send(|tx| {
            tx.focus(Some(&self.proxy(id)));
        });
    }

    /// Give window id the keyboard without waiting for the last transaction
    /// to be answered, as a window manager shouldn't.
    pub fn focus_without_waiting(&self, id: u32) {
        let window = self.proxy(id);
        commit(
            &mut self.log.lock().unwrap(),
            &self.manager,
            &self.qh,
            |tx| {
                tx.focus(Some(&window));
            },
        );
        self.conn.flush().unwrap();
    }

    /// Blur behind window id: 0 where its client asks, 1 nowhere, 2 behind all of it.
    pub fn blur(&self, id: u32, mode: u32) {
        self.send(|tx| {
            tx.blur(&self.proxy(id), mode.try_into().unwrap());
        });
    }

    /// How strongly windows are blurred behind.
    pub fn set_blur(&self, passes: u32, offset: f64) {
        self.manager.set_blur(passes, offset);
        self.conn.flush().unwrap();
    }

    /// Give window id a border width pixels wide, all in color.
    pub fn border(&self, id: u32, width: u32, color: u32) {
        self.send(|tx| {
            tx.border(&self.proxy(id), width, color, color, color, color, 0);
        });
    }

    /// Stop being the window manager.
    pub fn quit(self) {
        self.manager.destroy();
        self.conn.flush().unwrap();
    }

    /// The name programs are told it has.
    pub fn set_name(&self, name: &str) {
        self.manager.set_name(name.into());
        self.conn.flush().unwrap();
    }

    /// Make a workspace called name, listed after those made before.
    pub fn add_workspace(&self, name: &str) {
        let workspace = self.manager.workspace(&self.qh, ());
        self.send(|tx| {
            tx.workspace_name(&workspace, name.into());
        });
        self.workspaces
            .lock()
            .unwrap()
            .push((name.into(), workspace));
    }

    /// Make the workspace called name the active one, and the others not.
    pub fn activate_workspace(&self, name: &str) {
        for (n, workspace) in self.workspaces.lock().unwrap().iter() {
            self.send(|tx| {
                let state = if n == name {
                    wayluck_wm_workspace_v1::State::Active
                } else {
                    wayluck_wm_workspace_v1::State::empty()
                };
                tx.workspace_state(workspace, state);
            });
        }
    }

    pub fn workspace_on(&self, name: &str, output: &str, active: bool) {
        let output = self.output(output).proxy.unwrap();
        let workspaces = self.workspaces.lock().unwrap();
        let found = workspaces.iter().find(|(n, _)| n == name);
        let workspace = found.unwrap().1.clone();
        self.send(move |tx| {
            tx.workspace_output(&workspace, Some(&output));
            let state = match active {
                true => wayluck_wm_workspace_v1::State::Active,
                false => wayluck_wm_workspace_v1::State::empty(),
            };
            tx.workspace_state(&workspace, state);
        });
    }

    /// Destroy the workspace called name.
    pub fn remove_workspace(&self, name: &str) {
        let mut workspaces = self.workspaces.lock().unwrap();
        let at = workspaces.iter().position(|(n, _)| n == name).unwrap();
        workspaces.remove(at).1.destroy();
        self.conn.flush().unwrap();
    }

    /// Say window id is on the workspace called name (None: all).
    pub fn window_workspace(&self, id: u32, name: Option<&str>) {
        let workspaces = self.workspaces.lock().unwrap();
        let workspace = name.map(|name| {
            let found = workspaces.iter().find(|(n, _)| n == name);
            found.unwrap().1.clone()
        });
        self.send(|tx| {
            tx.window_workspace(&self.proxy(id), workspace.as_ref());
        });
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

/// A curve's values as the protocol carries them: 32-bit floats in the
/// machine's byte order.
fn floats(curve: &[f32]) -> Vec<u8> {
    curve.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

struct Client {
    manager: WayluckWmManagerV1,
    scene: WayluckWmSceneV1,
    /// The output it shows its scene on.
    output: Option<WayluckWmOutputV1>,
    log: Arc<Mutex<Log>>,
    windows: Arc<Mutex<Vec<(WayluckWmWindowV1, u32)>>>,
    /// Its objects for layer surfaces, in the order heard of (as in its log).
    layers: Arc<Mutex<Vec<WayluckWmLayerV1>>>,
    workspaces: Workspaces,
}

/// Commit a transaction built by build: the one unanswered now (log is
/// the window manager's, locked).
fn commit(
    log: &mut Log,
    manager: &WayluckWmManagerV1,
    qh: &QueueHandle<Client>,
    build: impl FnOnce(&WayluckWmTransactionV1),
) {
    let tx = manager.transaction(qh, ());
    build(&tx);
    tx.commit();
    log.unanswered += 1;
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

    /// A transaction built by build: now, or once the last one is answered,
    /// with the others waiting for it.
    fn send(
        &self,
        qh: &QueueHandle<Client>,
        build: impl FnOnce(&WayluckWmTransactionV1) + Send + 'static,
    ) {
        let mut log = self.log.lock().unwrap();
        if log.unanswered > 0 {
            log.later.push(Box::new(build));
        } else {
            commit(&mut log, &self.manager, qh, build);
        }
    }

    fn id_of(&self, proxy: &WayluckWmWindowV1) -> Option<u32> {
        let windows = self.windows.lock().unwrap();
        windows.iter().find(|(p, _)| p == proxy).map(|(_, id)| *id)
    }

    /// The name of the workspace behind proxy.
    fn workspace_name(&self, proxy: &WayluckWmWorkspaceV1) -> Option<String> {
        let workspaces = self.workspaces.lock().unwrap();
        let found = workspaces.iter().find(|(_, w)| w == proxy);
        found.map(|(name, _)| name.clone())
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
                let (output, scene) = (id.clone(), client.scene.clone());
                client.send(qh, move |tx| tx.show(&output, Some(&scene)));
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
            // Placed and focused at once, unless the test places it.
            wayluck_wm_manager_v1::Event::Window { window, id } => {
                if !client.log.lock().unwrap().unplaced {
                    let (x, y, w, h) = PLACE;
                    let (scene, placed) = (client.scene.clone(), window.clone());
                    client.send(qh, move |tx| {
                        tx.place(&scene, &placed, x, y, w, h);
                        tx.focus(Some(&placed));
                    });
                }
                client.windows.lock().unwrap().push((window, id));
                client.log.lock().unwrap().windows.push(Heard {
                    id,
                    ..Heard::default()
                });
            }
            wayluck_wm_manager_v1::Event::Ready => client.log.lock().unwrap().ready = true,
            wayluck_wm_manager_v1::Event::Layer { layer } => {
                client.layers.lock().unwrap().push(layer);
                client
                    .log
                    .lock()
                    .unwrap()
                    .layers
                    .push(HeardLayer::default());
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Client, WayluckWmManagerV1, [
        wayluck_wm_manager_v1::EVT_OUTPUT_OPCODE => (WayluckWmOutputV1, ()),
        wayluck_wm_manager_v1::EVT_WINDOW_OPCODE => (WayluckWmWindowV1, ()),
        wayluck_wm_manager_v1::EVT_SHARE_WORKSPACE_OPCODE => (WayluckWmOutputV1, ()),
        wayluck_wm_manager_v1::EVT_LAYER_OPCODE => (WayluckWmLayerV1, ()),
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
        let workspace = match &event {
            Event::WorkspaceRequest { workspace } => {
                workspace.as_ref().and_then(|w| client.workspace_name(w))
            }
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
            Event::Kind { kind } => heard.kind = kind.into_result().ok().map(u32::from),
            Event::Hints { hints } => heard.hints = hints.into_result().ok().map(|h| h.bits()),
            Event::Attention { on } => heard.attention = Some(on != 0),
            Event::ActivateRequest => heard.activate_requests += 1,
            Event::WorkspaceRequest { .. } => heard.workspace_requests.push(workspace),
            Event::Done => {
                if heard.dones == 0 {
                    heard.at_first_done = Some(Box::new(heard.clone()));
                }
                heard.dones += 1;
            }
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
        qh: &QueueHandle<Self>,
    ) {
        let mut log = client.log.lock().unwrap();
        if let wayluck_wm_transaction_v1::Event::Failed { .. } = event {
            log.failed += 1;
        }
        tx.destroy();
        // Answered: what waited for it, in one.
        log.unanswered -= 1;
        if log.unanswered == 0 && !log.later.is_empty() {
            let later = std::mem::take(&mut log.later);
            commit(&mut log, &client.manager, qh, |tx| {
                for build in later {
                    build(tx);
                }
            });
        }
    }
}

impl Dispatch<WayluckWmOutputV1, ()> for Client {
    fn event(
        client: &mut Self,
        proxy: &WayluckWmOutputV1,
        event: wayluck_wm_output_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut log = client.log.lock().unwrap();
        let object = proxy.id().protocol_id();
        if !log.outputs.iter().any(|o| o.object == object) {
            log.outputs.push(HeardOutput {
                object,
                proxy: Some(proxy.clone()),
                ..HeardOutput::default()
            });
        }
        let heard = log.outputs.iter_mut().find(|o| o.object == object).unwrap();
        match event {
            wayluck_wm_output_v1::Event::Name { name } => heard.name = name,
            wayluck_wm_output_v1::Event::Removed => heard.removed = true,
            wayluck_wm_output_v1::Event::Geometry {
                x,
                y,
                width,
                height,
            } => heard.geometry = Some((x, y, width, height)),
            wayluck_wm_output_v1::Event::Usable {
                x,
                y,
                width,
                height,
            } => log.usable = Some((x, y, width, height)),
            _ => {}
        }
    }
}

impl Dispatch<WayluckWmLayerV1, ()> for Client {
    fn event(
        client: &mut Self,
        proxy: &WayluckWmLayerV1,
        event: wayluck_wm_layer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let at = (client.layers.lock().unwrap().iter()).position(|l| l == proxy);
        let Some(at) = at else {
            return;
        };
        let mut log = client.log.lock().unwrap();
        let heard = &mut log.layers[at];
        match event {
            wayluck_wm_layer_v1::Event::Namespace { namespace } => heard.namespace = namespace,
            wayluck_wm_layer_v1::Event::Layer { layer } => {
                heard.layer = layer.into_result().ok().map(u32::from)
            }
            wayluck_wm_layer_v1::Event::Output { output } => heard.on_output = output.is_some(),
            wayluck_wm_layer_v1::Event::Done => heard.dones += 1,
            wayluck_wm_layer_v1::Event::Closed => heard.closed = true,
        }
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

impl Dispatch<WayluckWmWorkspaceV1, ()> for Client {
    fn event(
        client: &mut Self,
        proxy: &WayluckWmWorkspaceV1,
        event: wayluck_wm_workspace_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let wayluck_wm_workspace_v1::Event::ActivateRequest = event;
        if let Some(name) = client.workspace_name(proxy) {
            client.log.lock().unwrap().workspaces_asked.push(name);
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
