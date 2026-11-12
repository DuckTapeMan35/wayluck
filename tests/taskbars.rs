//! Taskbars' view of the windows (zwlr-foreign-toplevel-management), and
//! what they ask of them.

mod harness;
use harness::{
    Session,
    taskbar::{Entry, Taskbar},
    wait_for,
    wayland::Window,
    wm::{PLACE, Wm},
    x::XWindow,
};

/// What the taskbar was told of the window titled so.
fn entry<'a>(windows: &'a [Entry], title: &str) -> Option<&'a Entry> {
    windows.iter().find(|e| e.title == title)
}

#[test]
fn a_taskbar_sees_the_windows_and_which_is_active() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _one = Window::titled(&session, 0xff3366cc, "one");
    let one = wm.window("one").id;
    // Told of windows there before it, and after.
    let taskbar = Taskbar::open(&session);
    let _two = Window::titled(&session, 0xff33cc66, "two");
    wm.window("two");
    let windows = taskbar.wait_for("two shown and active, one not", |w| {
        let one = entry(w, "one").is_some_and(|e| !e.activated);
        one && entry(w, "two").is_some_and(|e| e.activated && e.outputs == 1)
    });
    let two = entry(&windows, "two").unwrap();
    assert!(!two.maximized && !two.fullscreen);

    wm.focus(one);
    taskbar.wait_for("one active, two not", |w| {
        let one = entry(w, "one").is_some_and(|e| e.activated);
        one && entry(w, "two").is_some_and(|e| !e.activated)
    });
}

#[test]
fn a_window_covering_what_its_output_leaves_windows_is_maximized() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let _one = Window::titled(&session, 0xff3366cc, "one");
    let one = wm.window("one").id;
    let taskbar = Taskbar::open(&session);
    taskbar.wait_for("one", |w| entry(w, "one").is_some_and(|e| !e.maximized));
    let usable = wait_for(|| wm.usable(), "the output's room");
    wm.place(one, usable, true, None);
    taskbar.wait_for("one maximized", |w| {
        entry(w, "one").is_some_and(|e| e.maximized)
    });
    wm.place(one, PLACE, true, None);
    taskbar.wait_for("one not", |w| entry(w, "one").is_some_and(|e| !e.maximized));
    // Fullscreen, it's that, not maximized.
    wm.fullscreen(one, true);
    taskbar.wait_for("one fullscreen", |w| {
        entry(w, "one").is_some_and(|e| e.fullscreen && !e.maximized)
    });
}

#[test]
fn what_a_taskbar_asks_the_window_manager_hears_and_the_client() {
    let session = Session::start();
    let wm = Wm::start(&session);
    let one = Window::titled(&session, 0xff3366cc, "one");
    let id = wm.window("one").id;
    let taskbar = Taskbar::open(&session);
    taskbar.wait_for("one", |w| entry(w, "one").is_some());

    // Asked of the window manager, which decides.
    taskbar.activate("one");
    wm.wait_for(id, "an activate request", |w| w.activate_requests == 1);
    taskbar.fullscreen("one");
    wm.wait_for(id, "a fullscreen request", |w| {
        w.fullscreen_requests == [true]
    });
    // Asked of its client, which may close it.
    taskbar.close("one");
    wait_for(
        || (one.seen().close_requests == 1).then_some(()),
        "one asked to close",
    );
}

#[test]
fn x_windows_are_on_taskbars_too() {
    let session = Session::start();
    let _wm = Wm::start(&session);
    let taskbar = Taskbar::open(&session);
    let x = XWindow::open(&session.x_display, "xwin", "XApp", (200, 200));
    taskbar.wait_for("the X window", |w| {
        entry(w, "xwin").is_some_and(|e| e.app_id == "XApp" && e.outputs == 1)
    });
    drop(x);
    taskbar.wait_for("it gone", |w| entry(w, "xwin").is_none());
}
