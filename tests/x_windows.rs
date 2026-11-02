//! X windows under a window manager: it hears of them as of any window
//! (names, parent, size limits, fullscreen requests), and what it decides
//! (where, how big, focus, fullscreen, close) reaches their X clients.

mod harness;
use harness::{PATIENCE, Session, wayland, wm, wm::Wm, x::XWindow};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};
use x11rb::{
    properties::WmSizeHints,
    protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode},
    wrapper::ConnectionExt as _,
};

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

#[test]
fn the_window_manager_closes_x_windows() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _x = XWindow::open(&session.x_display, "xwin", "XWin", (200, 200));
    let heard = wm.window("xwin");
    wm.close(heard.id);
    wm.wait_for(heard.id, "the X window to close", |w| w.closed);
}
