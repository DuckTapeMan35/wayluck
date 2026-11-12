//! A taskbar: a zwlr-foreign-toplevel-management client recording what it's
//! told of windows (as of each one's last done), and asking things of them.

use super::{Session, wait_for};
use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_output::WlOutput, wl_registry::WlRegistry, wl_seat::WlSeat},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, State, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

/// What it's been told of a window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub title: String,
    pub app_id: String,
    pub activated: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    /// How many of its outputs it's on.
    pub outputs: usize,
    /// Its parent's title.
    pub parent: Option<String>,
}

#[derive(Default)]
struct Seen {
    /// Each window: its handle, what's being told, what was at its last
    /// done, and whether it's closed.
    windows: Vec<(ZwlrForeignToplevelHandleV1, Entry, Option<Entry>, bool)>,
}

pub struct Taskbar {
    conn: Connection,
    seat: WlSeat,
    seen: Arc<Mutex<Seen>>,
}

struct Client {
    seen: Arc<Mutex<Seen>>,
}

impl Taskbar {
    pub fn open(session: &Session) -> Taskbar {
        let stream = UnixStream::connect(session.dir().join(&session.wayland_display)).unwrap();
        let conn = Connection::from_socket(stream).unwrap();
        let (globals, mut queue) = registry_queue_init::<Client>(&conn).unwrap();
        let qh = queue.handle();
        let _output: WlOutput = globals.bind(&qh, 1..=4, ()).unwrap();
        let seat: WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
        let _manager: ZwlrForeignToplevelManagerV1 = globals.bind(&qh, 3..=3, ()).unwrap();
        conn.flush().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut client = Client { seen: seen.clone() };
        std::thread::spawn(move || while queue.blocking_dispatch(&mut client).is_ok() {});
        Taskbar { conn, seat, seen }
    }

    /// The windows it's been told of (not closed), as of each one's last
    /// done, once f is true of them.
    pub fn wait_for(&self, what: &str, f: impl Fn(&[Entry]) -> bool) -> Vec<Entry> {
        wait_for(
            || {
                let windows = self.windows();
                f(&windows).then_some(windows)
            },
            what,
        )
    }

    pub fn windows(&self) -> Vec<Entry> {
        let seen = self.seen.lock().unwrap();
        let open = seen.windows.iter().filter(|(.., closed)| !closed);
        open.filter_map(|(_, _, done, _)| done.clone()).collect()
    }

    /// Ask for the window titled so to be brought forward.
    pub fn activate(&self, title: &str) {
        self.handle(title).activate(&self.seat);
        self.conn.flush().unwrap();
    }

    /// Ask for the window titled so to be closed.
    pub fn close(&self, title: &str) {
        self.handle(title).close();
        self.conn.flush().unwrap();
    }

    /// Ask for the window titled so to be fullscreen.
    pub fn fullscreen(&self, title: &str) {
        self.handle(title).set_fullscreen(None);
        self.conn.flush().unwrap();
    }

    fn handle(&self, title: &str) -> ZwlrForeignToplevelHandleV1 {
        let seen = self.seen.lock().unwrap();
        let found = (seen.windows.iter()).find(|(_, e, _, closed)| e.title == title && !closed);
        found.expect("no such window").0.clone()
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Client {
    fn event(
        client: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            let mut seen = client.seen.lock().unwrap();
            seen.windows.push((toplevel, Entry::default(), None, false));
        }
    }

    wayland_client::event_created_child!(Client, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Client {
    fn event(
        client: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        let mut seen = client.seen.lock().unwrap();
        // Its parent's title first: the entry holds the list.
        let parent = match &event {
            Event::Parent { parent } => Some(parent.as_ref().and_then(|p| {
                let found = seen.windows.iter().find(|(h, ..)| h == p);
                found.map(|(_, e, ..)| e.title.clone())
            })),
            _ => None,
        };
        let Some((_, entry, done, closed)) = seen.windows.iter_mut().find(|(h, ..)| h == handle)
        else {
            return;
        };
        match event {
            Event::Title { title } => entry.title = title,
            Event::AppId { app_id } => entry.app_id = app_id,
            Event::OutputEnter { .. } => entry.outputs += 1,
            Event::OutputLeave { .. } => entry.outputs -= 1,
            Event::State { state } => {
                let states: Vec<u32> = (state.chunks_exact(4))
                    .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
                    .collect();
                let has = |s: State| states.contains(&u32::from(s));
                entry.activated = has(State::Activated);
                entry.maximized = has(State::Maximized);
                entry.fullscreen = has(State::Fullscreen);
            }
            Event::Parent { .. } => entry.parent = parent.flatten(),
            Event::Done => *done = Some(entry.clone()),
            Event::Closed => *closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WlOutput,
        _: <WlOutput as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as wayland_client::Proxy>::Event,
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
