//! What's shown when the window manager changes the layout: all of it at
//! once, in one frame, once the windows in it have drawn for it.

mod harness;

use harness::{
    Session, wait_for,
    wayland::Window,
    wm::{PLACE, Wm},
};
use std::time::Duration;

/// Where the window titled so is shown (x, y, width, height); None if it
/// isn't (not placed, or nothing drawn yet).
fn shown(session: &mut Session, title: &str) -> Option<(i32, i32, i32, i32)> {
    let windows = session.command("windows");
    let window = windows
        .split("; ")
        .find(|w| w.starts_with(&format!("{title}@")))?;
    let (_, rect) = window.split_once('@')?;
    let (x, rest) = rect.split_once(',')?;
    let (y, size) = rest.split_once(',')?;
    let (w, h) = size.split_once('x')?;
    let [x, y, w, h] = [x, y, w, h].map(|n| n.parse::<i32>().unwrap());
    Some((x, y, w, h))
}

#[test]
fn a_new_window_is_shown_with_the_room_made_for_it() {
    const LEFT: (i32, i32, i32, i32) = (0, 0, 300, 300);
    const RIGHT: (i32, i32, i32, i32) = (300, 0, 300, 300);
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let _first = Window::titled(&session, 0xff00ff00, "first");
    let first = wm.window("first").id;
    session.window("first");

    // A new window that's slow to draw, placed with the room made for it.
    wm.stop_placing();
    let _second = Window::slow(&session, 0xff0000ff, "second", Duration::from_millis(100));
    let second = wm.window("second").id;
    wm.place_together(&[(first, LEFT, true), (second, RIGHT, true)], true);

    // The first window moves over only when the second is there too: never
    // a frame with an empty space where it'll be.
    wait_for(
        || (shown(&mut session, "first") == Some(LEFT)).then_some(()),
        "the first window to move over",
    );
    assert_eq!(shown(&mut session, "second"), Some(RIGHT));
    assert_eq!(wm.failed(), 0);
}

#[test]
fn one_committed_too_soon_fails_and_leaves_the_last_one_whole() {
    const LEFT: (i32, i32, i32, i32) = (0, 0, 300, 300);
    const RIGHT: (i32, i32, i32, i32) = (300, 0, 300, 300);
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let _first = Window::titled(&session, 0xff00ff00, "first");
    let first = wm.window("first").id;
    session.window("first");
    wm.stop_placing();
    let _second = Window::slow(&session, 0xff0000ff, "second", Duration::from_millis(100));
    let second = wm.window("second").id;
    wm.place_together(&[(first, LEFT, true), (second, RIGHT, true)], true);

    // While wayluck waits for the second window to draw.
    wm.focus_without_waiting(first);
    wait_for(
        || (wm.failed() == 1).then_some(()),
        "the one committed too soon to fail",
    );
    wait_for(
        || (shown(&mut session, "first") == Some(LEFT)).then_some(()),
        "the first window to move over",
    );
    assert_eq!(shown(&mut session, "second"), Some(RIGHT));
}

#[test]
fn a_new_window_slower_than_a_redraw_is_still_waited_for() {
    const LEFT: (i32, i32, i32, i32) = (0, 0, 300, 300);
    const RIGHT: (i32, i32, i32, i32) = (300, 0, 300, 300);
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let _first = Window::titled(&session, 0xff00ff00, "first");
    let first = wm.window("first").id;
    session.window("first");
    wm.stop_placing();
    // Longer than a redraw is waited for.
    let _second = Window::slow(&session, 0xff0000ff, "second", Duration::from_millis(400));
    let second = wm.window("second").id;
    wm.place_together(&[(first, LEFT, true), (second, RIGHT, true)], true);
    wait_for(
        || (shown(&mut session, "first") == Some(LEFT)).then_some(()),
        "the first window to move over",
    );
    assert_eq!(shown(&mut session, "second"), Some(RIGHT));
}

#[test]
fn a_window_placed_instantly_isnt_waited_for() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    // Redraws well within the time wayluck waits for it.
    let _window = Window::slow(&session, 0xff00ff00, "victim", Duration::from_millis(80));
    let id = wm.window("victim").id;
    session.window("victim");
    // Elsewhere, and smaller: shown there at once, at the size it was drawn
    // (its client hasn't redrawn yet).
    wm.place(id, (500, 300, 200, 150), true, None);
    let (x, y, w, h) = wait_for(
        || shown(&mut session, "victim").filter(|r| r.0 == 500),
        "it to be shown where it's placed",
    );
    assert_eq!((x, y, w, h), (500, 300, PLACE.2, PLACE.3));
}
