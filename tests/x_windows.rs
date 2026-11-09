//! X windows under a window manager: it hears of them as of any window
//! (names, parent, size limits, fullscreen requests), and what it decides
//! (where, how big, focus, fullscreen, close) reaches their X clients.

mod harness;
use harness::{PATIENCE, Session, wait_for, wayland, wm, wm::Wm, x::XWindow};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection as _,
    properties::{AspectRatio, WmHints, WmSizeHints},
    protocol::{
        shm::ConnectionExt as _,
        xproto::{AccessControl, AtomEnum, ConnectionExt as _, Family, HostMode, PropMode},
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

#[test]
fn xwayland_starts_when_an_x_program_connects() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    assert_eq!(session.command("xwayland"), "not started");
    let _x = XWindow::open(&session.x_display, "xwin", "XApp", (200, 200));
    assert!(session.command("xwayland").starts_with("running"));
}

#[test]
fn x_programs_get_in_only_by_the_door() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    let _x = XWindow::open(&session.x_display, "xwin", "XApp", (200, 200));
    let answer = session.command("xwayland");
    let xwayland = answer.strip_prefix("running ").expect("Xwayland running");
    // Straight to Xwayland's own display, with no cookie: turned away.
    assert!(RustConnection::connect(Some(xwayland)).is_err());
    // By the door: let in.
    assert!(RustConnection::connect(Some(&session.x_display)).is_ok());
}

#[test]
fn a_program_let_in_cant_let_others_in() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    let (conn, _) = RustConnection::connect(Some(&session.x_display)).unwrap();
    let answer = session.command("xwayland");
    let xwayland = answer.strip_prefix("running ").expect("Xwayland running");
    // Asking for anyone to be let in: refused.
    let opened = conn
        .set_access_control(AccessControl::DISABLE)
        .unwrap()
        .check();
    assert!(opened.is_err(), "access control turned off");
    let family = Family::SERVER_INTERPRETED;
    let host = b"localuser\0root".to_vec();
    let added = conn
        .change_hosts(HostMode::INSERT, family, &host)
        .unwrap()
        .check();
    assert!(added.is_err(), "a host added");
    // And still, straight to Xwayland: turned away.
    assert!(RustConnection::connect(Some(xwayland)).is_err());
    // The program's still fine.
    conn.get_input_focus().unwrap().reply().unwrap();
    // The key's nowhere on disk.
    let runtime = session.dir().read_dir().unwrap();
    assert!(
        !runtime
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("xauth"))
    );
}

#[test]
fn file_descriptors_get_through_to_xwayland() {
    let session = Session::start();
    let _wm = Wm::start(&session);
    let (conn, _) = RustConnection::connect(Some(&session.x_display)).unwrap();
    // Shared memory, handed over by its file descriptor (MIT-SHM 1.2).
    let memory = rustix::fs::memfd_create("x-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    rustix::fs::ftruncate(&memory, 4096).unwrap();
    let segment = conn.generate_id().unwrap();
    conn.shm_attach_fd(segment, memory, false).unwrap();
    // A request that needs it: an error if it didn't get there.
    conn.shm_detach(segment)
        .unwrap()
        .check()
        .expect("Xwayland didn't get the memory");
}

/// The test window manager itself: what it does to a Wayland window, the
/// tests below expect for X windows.
#[test]
fn wayland_windows_go_where_the_window_manager_places_them() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let _w = wayland::Window::open(&session, 0xff00ff00);
    wm.window("victim");
    let start = Instant::now();
    while session.window("victim") != wm::PLACE {
        assert!(
            start.elapsed() < PATIENCE,
            "the window isn't where it was placed"
        );
        sleep(Duration::from_millis(20));
    }
}

/// The kinds the window manager hears (wayluck_wm_window_v1's kind).
const NORMAL: u32 = 0;
const DIALOG: u32 = 1;
const UTILITY: u32 = 2;

#[test]
fn an_x_window_is_the_kind_its_type_says() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _x = XWindow::open_with(&session.x_display, "xtools", "XApp", (100, 100), |c, w| {
        let atom = |name: &str| {
            c.intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let utility = atom("_NET_WM_WINDOW_TYPE_UTILITY");
        c.change_property32(
            PropMode::REPLACE,
            w,
            atom("_NET_WM_WINDOW_TYPE"),
            AtomEnum::ATOM,
            &[utility],
        )
        .unwrap();
    });
    let id = wm.window("xtools").id;
    wm.wait_for(id, "it a utility", |w| w.kind == Some(UTILITY));
}

#[test]
fn x_and_wayland_windows_without_a_type_are_normal_or_dialogs() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let main = XWindow::open(&session.x_display, "xmain", "XApp", (200, 200));
    let main_id = wm.window("xmain").id;
    wm.wait_for(main_id, "it normal", |w| w.kind == Some(NORMAL));
    // Transient for it, with no type of its own: a dialog.
    let _dialog = XWindow::open_with(&session.x_display, "xdialog", "XApp", (100, 100), |c, w| {
        c.change_property32(
            PropMode::REPLACE,
            w,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            &[main.id()],
        )
        .unwrap();
    });
    let dialog = wm.window("xdialog").id;
    wm.wait_for(dialog, "it a dialog", |w| w.kind == Some(DIALOG));
    // A Wayland window with no parent: normal too.
    let _w = wayland::Window::open(&session, 0xff00ff00);
    let id = wm.window("victim").id;
    wm.wait_for(id, "the Wayland window normal", |w| w.kind == Some(NORMAL));
}

#[test]
fn the_window_manager_hears_of_x_windows() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _x = XWindow::open(&session.x_display, "xwin", "XWin", (200, 200));
    let heard = wm.window("xwin");
    wm.wait_for(heard.id, "its app id", |w| w.app_id == "XWin");
}

#[test]
fn x_windows_go_where_the_window_manager_places_them() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "xwin", "XWin", (200, 200));
    wm.window("xwin");
    let start = Instant::now();
    while session.window("xwin") != wm::PLACE {
        assert!(
            start.elapsed() < PATIENCE,
            "the X window isn't where it was placed"
        );
        sleep(Duration::from_millis(20));
    }
    // Its client was told the size too.
    let (_, _, width, height) = wm::PLACE;
    assert_eq!(x.size(), (width as u16, height as u16));
}

#[test]
fn an_x_window_the_window_manager_focuses_gets_the_keyboard() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "xwin", "XWin", (200, 200));
    session.wait_for(|| x.focused().then_some(()), "the X window to have focus");
    let keys = [30, 48];
    session.type_keys(&keys);
    session.wait_for(
        || (x.keys().len() == keys.len()).then_some(()),
        "the X window to get the keys",
    );
}

/// Wine (Proton) takes its window to be in the background unless the root
/// window's _NET_ACTIVE_WINDOW names it; a fullscreen game then minimizes
/// itself. X clients learn nothing of Wayland windows: none is active then.
#[test]
fn the_focused_x_window_is_the_active_one() {
    let mut session = Session::start();
    let wayland = wayland::Window::open(&session, 0xffcc3333);
    let x = XWindow::open(&session.x_display, "xwin", "XWin", (300, 300));
    session.click_window("xwin");
    session.wait_for(|| x.focused().then_some(()), "the X window to have focus");
    session.wait_for(
        || (x.active_window() == x.id()).then_some(()),
        "_NET_ACTIVE_WINDOW to name the X window",
    );
    session.click(10, 10);
    session.wait_for(
        || wayland.seen().focused.then_some(()),
        "the Wayland window to have the keyboard",
    );
    session.wait_for(
        || (x.active_window() == 0).then_some(()),
        "_NET_ACTIVE_WINDOW to name no window",
    );
}

#[test]
fn an_x_dialog_has_its_window_as_parent() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let main = XWindow::open(&session.x_display, "xmain", "XApp", (200, 200));
    let main_id = wm.window("xmain").id;
    let _dialog = XWindow::open_with(&session.x_display, "xdialog", "XApp", (100, 100), |c, w| {
        c.change_property32(
            PropMode::REPLACE,
            w,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            &[main.id()],
        )
        .unwrap();
    });
    let dialog = wm.window("xdialog");
    wm.wait_for(dialog.id, "the dialog's parent", |w| {
        w.parent == Some(main_id)
    });
}

#[test]
fn an_x_window_comes_with_how_its_client_would_like_it_kept() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _x = XWindow::open_with(&session.x_display, "xkept", "XApp", (300, 200), |c, w| {
        let atom = |name: &str| {
            c.intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let above = atom("_NET_WM_STATE_ABOVE");
        c.change_property32(
            PropMode::REPLACE,
            w,
            atom("_NET_WM_STATE"),
            AtomEnum::ATOM,
            &[above],
        )
        .unwrap();
        let hints = WmSizeHints {
            aspect: Some((AspectRatio::new(3, 2), AspectRatio::new(3, 2))),
            ..WmSizeHints::default()
        };
        hints.set_normal_hints(c, w).unwrap();
    });
    let id = wm.window("xkept").id;
    // Above (1) and its aspect kept (2), already when it's all there is.
    let heard = wm.wait_for(id, "its first done", |w| w.dones > 0);
    let first = heard.at_first_done.unwrap();
    assert_eq!(first.hints, Some(1 | 2));
    assert_eq!(first.kind, Some(NORMAL));
    assert_eq!(first.app_id, "XApp");
}

#[test]
fn a_wayland_window_comes_with_what_its_client_set_up() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _w = wayland::Window::open(&session, 0xff00ff00);
    let id = wm.window("victim").id;
    let heard = wm.wait_for(id, "its first done", |w| w.dones > 0);
    let first = heard.at_first_done.unwrap();
    assert_eq!(first.title, "victim");
    assert_eq!(first.kind, Some(NORMAL));
    assert_eq!(first.hints, Some(0));
}

#[test]
fn changes_to_a_window_end_in_a_done() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "before", "XApp", (200, 200));
    let id = wm.window("before").id;
    let dones = wm.wait_for(id, "its first done", |w| w.dones > 0).dones;
    x.retitle("after");
    wm.wait_for(id, "the new title, and a done after it", |w| {
        w.title == "after" && w.dones > dones
    });
}

#[test]
fn an_urgent_x_window_wants_attention() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _x = XWindow::open_with(&session.x_display, "xurgent", "XApp", (200, 200), |c, w| {
        let hints = WmHints {
            urgent: true,
            ..WmHints::default()
        };
        hints.set(c, w).unwrap();
    });
    let id = wm.window("xurgent").id;
    let heard = wm.wait_for(id, "its first done", |w| w.dones > 0);
    assert_eq!(heard.at_first_done.unwrap().attention, Some(true));
}

#[test]
fn a_pager_asking_for_an_x_window_asks_the_window_manager() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "xwin", "XApp", (200, 200));
    let id = wm.window("xwin").id;
    x.ask_active();
    wm.wait_for(id, "it asked to be brought forward", |w| {
        w.activate_requests == 1
    });
}

#[test]
fn a_wayland_window_asking_with_no_right_to_only_wants_attention() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let first = wayland::Window::titled(&session, 0xff00ff00, "first");
    let first_id = wm.window("first").id;
    // The next one has the keyboard: the first's last input is old news.
    let _second = wayland::Window::titled(&session, 0xff0000ff, "second");
    wm.window("second");
    wait_for(
        || (!first.seen().focused).then_some(()),
        "the first to lose the keyboard",
    );
    first.ask_activation(true);
    let heard = wm.wait_for(first_id, "it wants attention", |w| {
        w.attention == Some(true)
    });
    assert_eq!(heard.activate_requests, 0);
    // Focused, it's had it.
    wm.focus(first_id);
    wm.wait_for(first_id, "it no longer wants attention", |w| {
        w.attention == Some(false)
    });
}

#[test]
fn a_wayland_window_asking_with_input_asks_the_window_manager() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let w = wayland::Window::open(&session, 0xff00ff00);
    let id = wm.window("victim").id;
    wait_for(|| w.seen().focused.then_some(()), "it to have the keyboard");
    w.ask_activation(true);
    wm.wait_for(id, "it asked to be brought forward", |w| {
        w.activate_requests == 1
    });
    // A token from nothing isn't enough.
    w.ask_activation(false);
    wm.wait_for(id, "it wants attention", |w| w.attention == Some(true));
}

#[test]
fn an_x_dock_is_no_window_and_stays_where_it_puts_itself() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let _dock = XWindow::open_with(&session.x_display, "xbar", "XBar", (400, 30), |c, w| {
        let atom = |name: &str| {
            c.intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let dock = atom("_NET_WM_WINDOW_TYPE_DOCK");
        c.change_property32(
            PropMode::REPLACE,
            w,
            atom("_NET_WM_WINDOW_TYPE"),
            AtomEnum::ATOM,
            &[dock],
        )
        .unwrap();
    });
    wait_for(
        || (session.command("x-docks") == "xbar@0,0,400x30").then_some(()),
        "the dock, where it put itself",
    );
    // A window after it: the window manager hears of that one only.
    let _x = XWindow::open(&session.x_display, "xwin", "XApp", (200, 200));
    wm.window("xwin");
    assert!(!wm.titles().contains(&"xbar".to_string()));
}

#[test]
fn an_x_docks_strut_is_kept_from_windows() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let whole = wait_for(|| wm.usable(), "the output's room");
    let dock = XWindow::open_with(&session.x_display, "xbar", "XBar", (400, 30), |c, w| {
        let atom = |name: &str| {
            c.intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let dock = atom("_NET_WM_WINDOW_TYPE_DOCK");
        c.change_property32(
            PropMode::REPLACE,
            w,
            atom("_NET_WM_WINDOW_TYPE"),
            AtomEnum::ATOM,
            &[dock],
        )
        .unwrap();
        // 30 along the top, from x 0 to 399.
        let strut = [0, 0, 30, 0, 0, 0, 0, 0, 0, 399, 0, 0];
        c.change_property32(
            PropMode::REPLACE,
            w,
            atom("_NET_WM_STRUT_PARTIAL"),
            AtomEnum::CARDINAL,
            &strut,
        )
        .unwrap();
    });
    let kept = (whole.0, whole.1 + 30, whole.2, whole.3 - 30);
    wait_for(
        || (wm.usable() == Some(kept)).then_some(()),
        "the top 30 kept",
    );
    // Gone, it keeps nothing.
    drop(dock);
    wait_for(
        || (wm.usable() == Some(whole)).then_some(()),
        "all of it again",
    );
}

#[test]
fn the_window_manager_hears_x_size_limits() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _x = XWindow::open_with(
        &session.x_display,
        "xfixed",
        "XFixed",
        (300, 200),
        |c, w| {
            let hints = WmSizeHints {
                min_size: Some((300, 200)),
                max_size: Some((300, 200)),
                ..WmSizeHints::default()
            };
            hints.set_normal_hints(c, w).unwrap();
        },
    );
    let heard = wm.window("xfixed");
    wm.wait_for(heard.id, "its size limits", |w| {
        w.size_hints == Some([300, 200, 300, 200])
    });
}

#[test]
fn x_fullscreen_is_the_window_managers_to_give() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "xwin", "XWin", (200, 200));
    let heard = wm.window("xwin");
    x.ask_fullscreen(true);
    wm.wait_for(heard.id, "the fullscreen request", |w| {
        w.fullscreen_requests == [true]
    });
    // Asking isn't getting.
    assert!(!x.is_fullscreen(), "fullscreen without the window manager");
    wm.fullscreen(heard.id, true);
    harness::wait_for(
        || x.is_fullscreen().then_some(()),
        "the X window to be fullscreen",
    );
    assert_eq!(wm.failed(), 0);
}

/// A game (or Wine for it) sets _NET_WM_STATE before it maps its window,
/// as EWMH allows: the window manager hears it wants fullscreen with the
/// window, so it can place it fullscreen at once instead of tiling it first.
#[test]
fn an_x_window_that_starts_fullscreen_asks_with_the_window() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _x = XWindow::open_with(&session.x_display, "xgame", "XGame", (200, 200), |c, w| {
        let atom = |name: &str| {
            c.intern_atom(false, name.as_bytes())
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let (state, fullscreen) = (atom("_NET_WM_STATE"), atom("_NET_WM_STATE_FULLSCREEN"));
        c.change_property32(PropMode::REPLACE, w, state, AtomEnum::ATOM, &[fullscreen])
            .unwrap();
    });
    let heard = wm.window("xgame");
    wm.wait_for(heard.id, "the fullscreen request", |w| {
        w.fullscreen_requests == [true]
    });
}

/// A menu (an override-redirect window) is where its program puts it, in
/// X's coordinates, which are wayluck's global ones.
#[test]
fn an_x_menu_shows_where_its_program_puts_it() {
    let mut session = Session::start();
    let x = XWindow::open(&session.x_display, "xmain", "XApp", (300, 300));
    session.click_window("xmain");
    session.wait_for(|| x.focused().then_some(()), "the X window to have focus");
    let _menu = XWindow::open_menu(&session.x_display, "xmenu", (60, 70), (100, 80));
    assert_eq!(session.x_menu("xmenu", true), (60, 70, 100, 80));
}

/// Menus are X's own: the window manager isn't told of them, and clients
/// listing windows don't see them.
#[test]
fn x_menus_are_no_windows_of_the_window_manager() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let _menu = XWindow::open_menu(&session.x_display, "xmenu", (60, 70), (100, 80));
    session.x_menu("xmenu", false);
    // A window opened after it: once the window manager hears of that, it
    // would have heard of the menu, in order.
    let _later = XWindow::open(&session.x_display, "xlater", "XApp", (200, 200));
    wm.window("xlater");
    assert!(!wm.titles().contains(&"xmenu".into()), "{:?}", wm.titles());
    let listed = wayland::list_windows(&session);
    assert!(
        !listed.iter().any(|(title, _)| title == "xmenu"),
        "{listed:?}"
    );
}

/// Text copied in a Wayland window pastes into an X window.
#[test]
fn wayland_copies_paste_into_x() {
    let mut session = Session::start();
    let wayland = wayland::Window::open(&session, 0xffcc3333);
    session.click(10, 10);
    session.wait_for(
        || wayland.seen().focused.then_some(()),
        "the Wayland window to have the keyboard",
    );
    wayland.copy("from wayland");
    let x = XWindow::open(&session.x_display, "xmain", "XApp", (300, 300));
    session.click_window("xmain");
    session.wait_for(|| x.focused().then_some(()), "the X window to have focus");
    session.wait_for(
        || (x.paste().as_deref() == Some("from wayland")).then_some(()),
        "the X window to paste what was copied in the Wayland one",
    );
}

/// Text copied in an X window pastes into a Wayland window.
#[test]
fn x_copies_paste_into_wayland() {
    let mut session = Session::start();
    let wayland = wayland::Window::open(&session, 0xffcc3333);
    let x = XWindow::open(&session.x_display, "xmain", "XApp", (300, 300));
    session.click_window("xmain");
    session.wait_for(|| x.focused().then_some(()), "the X window to have focus");
    x.copy("from x");
    // wayluck takes a moment (it asks the X program what it copied can be
    // pasted as): a person doesn't click away that fast.
    session.clipboard_from_x();
    session.click(10, 10);
    session.wait_for(
        || wayland.seen().focused.then_some(()),
        "the Wayland window to have the keyboard",
    );
    session.wait_for(
        || (wayland.paste().as_deref() == Some("from x")).then_some(()),
        "the Wayland window to paste what was copied in the X one",
    );
}

#[test]
fn the_window_manager_closes_x_windows() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _x = XWindow::open(&session.x_display, "xwin", "XWin", (200, 200));
    let heard = wm.window("xwin");
    wm.close(heard.id);
    wm.wait_for(heard.id, "the X window to close", |w| w.closed);
}
