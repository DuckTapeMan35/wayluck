//! Monitors coming (and going): where they go, and who hears.

mod harness;

use harness::{
    Session,
    layer::{Kind, Layer},
    wait_for,
    wayland::Window,
    wm::Wm,
};

#[test]
fn a_monitor_plugged_in_goes_right_of_the_others() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let first = wm.output("HEADLESS-1").geometry.unwrap();
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    // The window manager hears of it, where it is: its top lined up with
    // the first's, its left at the first's right.
    let second = wm.output("HEADLESS-2").geometry.unwrap();
    assert_eq!(second, (first.0 + first.2, first.1, 800, 600));
}

#[test]
fn the_pointer_goes_across_monitors_and_no_further() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    session.command("move 1270 100");
    // Onto the second, right of the first (1280 wide).
    assert_eq!(session.command("nudge 20 0"), "1290,100");
    // Below the second (600 high), there's nothing: it stays on it.
    assert_eq!(session.command("nudge 0 700"), "1290,599");
}

#[test]
fn virtual_outputs_stay_right_of_the_monitors() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    wm.virtual_output(640, 480);
    assert_eq!(wm.output("VIRTUAL-1").geometry, Some((1280, 0, 640, 480)));
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    // Moved over to make room: the pointer, kept on the monitors, never
    // gets there.
    wait_for(
        || (wm.output("VIRTUAL-1").geometry == Some((2080, 0, 640, 480))).then_some(()),
        "the virtual output right of the new monitor",
    );
}

#[test]
fn an_unplugged_monitor_is_gone_and_the_ones_right_of_it_close_up() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    wm.output("HEADLESS-2");
    assert_eq!(session.command("remove-monitor HEADLESS-1"), "ok");
    wait_for(
        || wm.output("HEADLESS-1").removed.then_some(()),
        "the window manager to hear it's gone",
    );
    wait_for(
        || (wm.output("HEADLESS-2").geometry == Some((0, 0, 800, 600))).then_some(()),
        "the second where the first was",
    );
}

#[test]
fn an_unplugged_monitors_bars_close() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    // On the first monitor, as a bar that names none is.
    let bar = Layer::open(&session, "bar", Kind::Top, 30, 0xff3366cc);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    assert_eq!(session.command("remove-monitor HEADLESS-1"), "ok");
    wait_for(|| bar.closed().then_some(()), "the bar to be closed");
}

#[test]
fn the_pointer_on_an_unplugged_monitor_comes_back_onto_one() {
    let mut session = Session::start();
    let _wm = Wm::start(&session);
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    session.command("move 1300 100");
    assert_eq!(session.command("remove-monitor HEADLESS-2"), "ok");
    // At the edge of the first, as near as it gets: from there, 10 left is
    // still on it.
    assert_eq!(session.command("nudge -10 0"), "1269,100");
}

#[test]
fn with_no_window_manager_windows_go_to_the_first_monitor_left() {
    let mut session = Session::start();
    let _window = Window::open(&session, 0xff00ff00);
    session.window("victim");
    assert_eq!(session.command("add-monitor HEADLESS-2 800 600"), "ok");
    assert_eq!(session.command("remove-monitor HEADLESS-1"), "ok");
    // Still on screen: on the one that's left, now where the first was.
    let (x, y, _, _) = session.window("victim");
    assert!(
        x < 800 && y < 600,
        "it isn't on the monitor left: at {x},{y}"
    );
}
