//! What X programs are told of their window manager, as EWMH has it:
//! whatever window manager runs, X programs read it off Xwayland as they
//! would off any X window manager.

mod harness;
use harness::{
    Session,
    layer::{Kind, Layer},
    wait_for,
    wm::Wm,
    x::XWindow,
};
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Window},
    rust_connection::RustConnection,
};

/// An X program reading what's said of the window manager.
struct Reader {
    conn: RustConnection,
    root: Window,
}

impl Reader {
    fn connect(session: &Session) -> Reader {
        let (conn, screen) = RustConnection::connect(Some(&session.x_display)).unwrap();
        let root = conn.setup().roots[screen].root;
        Reader { conn, root }
    }

    fn atom(&self, name: &str) -> Atom {
        let reply = self.conn.intern_atom(false, name.as_bytes()).unwrap();
        reply.reply().unwrap().atom
    }

    /// window's property, as 32-bit values (empty if it's not set).
    fn values(&self, window: Window, name: &str) -> Vec<u32> {
        let property = self.atom(name);
        let reply = self
            .conn
            .get_property(false, window, property, AtomEnum::ANY, 0, 1024);
        let reply = reply.unwrap().reply().unwrap();
        reply.value32().map(|v| v.collect()).unwrap_or_default()
    }

    /// The name on the window that says a window manager runs.
    fn wm_name(&self) -> Option<String> {
        let check = *self.values(self.root, "_NET_SUPPORTING_WM_CHECK").first()?;
        let name = self.atom("_NET_WM_NAME");
        let reply = self
            .conn
            .get_property(false, check, name, AtomEnum::ANY, 0, 1024);
        let reply = reply.unwrap().reply().unwrap();
        Some(String::from_utf8(reply.value).unwrap())
    }

    /// Ask the window manager, as EWMH has pagers ask: a message of kind
    /// about window, with number, sent to the root window.
    fn ask(&self, window: Window, kind: &str, number: u32) {
        let kind = self.atom(kind);
        let message = ClientMessageEvent::new(32, window, kind, [number, 0, 0, 0, 0]);
        let mask = EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT;
        self.conn
            .send_event(false, self.root, mask, message)
            .unwrap();
        self.conn.flush().unwrap();
    }

    /// The root window's property, as bytes.
    fn bytes(&self, name: &str) -> Vec<u8> {
        let property = self.atom(name);
        let reply = self
            .conn
            .get_property(false, self.root, property, AtomEnum::ANY, 0, 1024);
        reply.unwrap().reply().unwrap().value
    }

    /// Wait until window's property is values.
    fn wait_for(&self, window: Window, name: &str, values: &[u32]) {
        wait_for(
            || (self.values(window, name) == values).then_some(()),
            &format!("{name} to be {values:?}"),
        );
    }
}

#[test]
fn x_programs_are_told_the_window_managers_name() {
    let session = Session::start();
    let reader = Reader::connect(&session);
    wait_for(
        || (reader.wm_name().as_deref() == Some("wayluck")).then_some(()),
        "wayluck's own name, with no window manager",
    );
    let wm = Wm::start(&session);
    wm.set_name("quackers");
    wait_for(
        || (reader.wm_name().as_deref() == Some("quackers")).then_some(()),
        "the window manager's name",
    );
    wm.quit();
    wait_for(
        || (reader.wm_name().as_deref() == Some("wayluck")).then_some(()),
        "wayluck's own name, once it's gone",
    );
}

#[test]
fn a_name_given_before_x_starts_is_told() {
    let session = Session::start();
    let wm = Wm::start(&session);
    wm.set_name("early");
    // Xwayland starts with the first X program.
    let reader = Reader::connect(&session);
    wait_for(
        || (reader.wm_name().as_deref() == Some("early")).then_some(()),
        "the name given before",
    );
}

#[test]
fn x_programs_are_told_what_their_window_manager_supports() {
    let session = Session::start();
    let reader = Reader::connect(&session);
    let wanted = [
        "_NET_WORKAREA",
        "_NET_FRAME_EXTENTS",
        "_NET_NUMBER_OF_DESKTOPS",
        "_NET_CURRENT_DESKTOP",
        "_NET_DESKTOP_NAMES",
        "_NET_DESKTOP_VIEWPORT",
        "_NET_WM_DESKTOP",
    ]
    .map(|name| reader.atom(name));
    let supported = wait_for(
        || {
            let supported = reader.values(reader.root, "_NET_SUPPORTED");
            wanted
                .iter()
                .all(|a| supported.contains(a))
                .then_some(supported)
        },
        "everything wayluck tells supported",
    );
    // What smithay's window manager supports is still there.
    assert!(supported.contains(&reader.atom("_NET_ACTIVE_WINDOW")));
}

#[test]
fn the_work_area_is_what_bars_leave_windows() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let reader = Reader::connect(&session);
    let whole = wait_for(|| wm.usable(), "the output's room");
    let area = |(x, y, w, h): (i32, i32, i32, i32)| vec![x as u32, y as u32, w as u32, h as u32];
    wait_for(
        || (reader.values(reader.root, "_NET_WORKAREA") == area(whole)).then_some(()),
        "the whole output",
    );
    let _bar = Layer::open(&session, "bar", Kind::Top, 40, 0xff00ff00);
    let left = (whole.0, whole.1 + 40, whole.2, whole.3 - 40);
    wait_for(
        || (reader.values(reader.root, "_NET_WORKAREA") == area(left)).then_some(()),
        "what the bar leaves",
    );
}

#[test]
fn an_x_windows_frame_extents_are_its_border() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "framed", "Framed", (200, 200));
    let reader = Reader::connect(&session);
    let id = wm.window("framed").id;
    wm.border(id, 5, 0xffff0000);
    wait_for(
        || (reader.values(x.id(), "_NET_FRAME_EXTENTS") == [5; 4]).then_some(()),
        "5 all round",
    );
    wm.border(id, 0, 0);
    wait_for(
        || (reader.values(x.id(), "_NET_FRAME_EXTENTS") == [0; 4]).then_some(()),
        "none",
    );
}

#[test]
fn workspaces_are_listed_as_desktops() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let reader = Reader::connect(&session);
    for name in ["one", "two", "three"] {
        wm.add_workspace(name);
    }
    wm.activate_workspace("two");
    let root = reader.root;
    reader.wait_for(root, "_NET_NUMBER_OF_DESKTOPS", &[3]);
    reader.wait_for(root, "_NET_CURRENT_DESKTOP", &[1]);
    assert_eq!(reader.bytes("_NET_DESKTOP_NAMES"), b"one\0two\0three\0");
    assert_eq!(reader.values(root, "_NET_DESKTOP_VIEWPORT"), [0; 6]);
    wm.activate_workspace("three");
    reader.wait_for(root, "_NET_CURRENT_DESKTOP", &[2]);
    // One work area for each.
    let area = reader.values(root, "_NET_WORKAREA");
    assert_eq!(area.len(), 12);
    assert_eq!(area[..4], area[8..]);
    wm.remove_workspace("one");
    reader.wait_for(root, "_NET_NUMBER_OF_DESKTOPS", &[2]);
    reader.wait_for(root, "_NET_CURRENT_DESKTOP", &[1]);
    assert_eq!(reader.bytes("_NET_DESKTOP_NAMES"), b"two\0three\0");
    // Gone, its workspaces go with it.
    wm.quit();
    reader.wait_for(root, "_NET_NUMBER_OF_DESKTOPS", &[0]);
}

#[test]
fn an_x_windows_desktop_is_its_workspace() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "placed", "Placed", (200, 200));
    let reader = Reader::connect(&session);
    let id = wm.window("placed").id;
    wm.add_workspace("one");
    wm.add_workspace("two");
    wm.window_workspace(id, Some("two"));
    reader.wait_for(x.id(), "_NET_WM_DESKTOP", &[1]);
    // The one before it gone, it's the first.
    wm.remove_workspace("one");
    reader.wait_for(x.id(), "_NET_WM_DESKTOP", &[0]);
    wm.window_workspace(id, None);
    reader.wait_for(x.id(), "_NET_WM_DESKTOP", &[u32::MAX]);
    // Its own gone, nothing's said of it.
    wm.window_workspace(id, Some("two"));
    reader.wait_for(x.id(), "_NET_WM_DESKTOP", &[0]);
    wm.remove_workspace("two");
    reader.wait_for(x.id(), "_NET_WM_DESKTOP", &[]);
}

#[test]
fn a_pager_switching_desktops_asks_the_window_manager() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let reader = Reader::connect(&session);
    wm.add_workspace("one");
    wm.add_workspace("two");
    reader.wait_for(reader.root, "_NET_NUMBER_OF_DESKTOPS", &[2]);
    reader.ask(reader.root, "_NET_CURRENT_DESKTOP", 1);
    // No such desktop: nothing's asked.
    reader.ask(reader.root, "_NET_CURRENT_DESKTOP", 7);
    reader.ask(reader.root, "_NET_CURRENT_DESKTOP", 0);
    wait_for(
        || (wm.workspaces_asked() == ["two", "one"]).then_some(()),
        "two, then one, asked to be shown",
    );
}

#[test]
fn a_pager_moving_an_x_window_asks_the_window_manager() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "moved", "Moved", (200, 200));
    let reader = Reader::connect(&session);
    let id = wm.window("moved").id;
    wm.add_workspace("one");
    wm.add_workspace("two");
    reader.wait_for(reader.root, "_NET_NUMBER_OF_DESKTOPS", &[2]);
    reader.ask(x.id(), "_NET_WM_DESKTOP", 1);
    reader.ask(x.id(), "_NET_WM_DESKTOP", 7);
    reader.ask(x.id(), "_NET_WM_DESKTOP", u32::MAX);
    let asked = [Some("two".to_string()), None];
    wm.wait_for(id, "two, then all, asked", |w| {
        w.workspace_requests == asked
    });
}
