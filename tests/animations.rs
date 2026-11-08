//! Windows drawn moving to where the window manager places them: where
//! they're on their way, and what's on screen meanwhile.

mod harness;

use harness::{
    Session,
    capture::screenshot,
    wait_for,
    wayland::Window,
    wm::{PLACE, Wm},
    x::XWindow,
};
use std::time::{Duration, Instant};

/// A straight curve: progress is time.
const LINEAR: [f64; 4] = [0.0, 0.0, 1.0, 1.0];

/// Somewhere else in the scene than PLACE, as big.
const ELSEWHERE: (i32, i32, i32, i32) = (500, 300, 400, 300);

/// Where the window titled so is drawn while it's animated, in its scene
/// (x, y, width, height); None if it isn't.
fn animating(session: &mut Session, title: &str) -> Option<(i32, i32, i32, i32)> {
    let answer = session.command("animating");
    let window = answer
        .split("; ")
        .find(|w| w.starts_with(&format!("{title}@")))?;
    let (_, rect) = window.split_once('@')?;
    let (x, rest) = rect.split_once(',')?;
    let (y, size) = rest.split_once(',')?;
    let (w, h) = size.split_once('x')?;
    let [x, y, w, h] = [x, y, w, h].map(|n| n.parse::<i32>().unwrap());
    Some((x, y, w, h))
}

/// A window ("victim"), placed at PLACE by a window manager, and its id.
fn placed_window(session: &mut Session) -> (Wm, Window, u32) {
    let wm = Wm::start(session);
    let window = Window::open(session, 0xff00ff00);
    let id = wm.window("victim").id;
    session.window("victim");
    (wm, window, id)
}

#[test]
fn a_window_placed_elsewhere_is_drawn_moving_there() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    wm.set_animation(1000, LINEAR);
    let asked = Instant::now();
    wm.place(id, ELSEWHERE, false, None);
    let (x, y, ..) = wait_for(|| animating(&mut session, "victim"), "it to move");
    // At the start: about where it was.
    assert!(x < 300 && y < 200, "at {x},{y}");
    // Halfway (by the clock, give or take): between the two.
    std::thread::sleep(Duration::from_millis(500).saturating_sub(asked.elapsed()));
    let (x, y, ..) = animating(&mut session, "victim").expect("over too soon");
    assert!(
        (200..=400).contains(&x) && (150..=250).contains(&y),
        "at {x},{y}"
    );
    // After: where it's placed, no longer moving.
    std::thread::sleep(Duration::from_millis(1200).saturating_sub(asked.elapsed()));
    assert_eq!(animating(&mut session, "victim"), None);
    assert_eq!(wm.failed(), 0);
}

#[test]
fn without_an_animation_set_windows_dont_move() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    wm.place(id, ELSEWHERE, false, None);
    wait_for(
        || (session_window(&mut session) == ELSEWHERE.0).then_some(()),
        "it to be placed",
    );
    assert_eq!(animating(&mut session, "victim"), None);
}

/// Where the test's window is on screen (its x).
fn session_window(session: &mut Session) -> i32 {
    session.window("victim").0
}

#[test]
fn an_instant_transaction_isnt_animated() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    wm.set_animation(60_000, LINEAR);
    wm.place(id, ELSEWHERE, true, None);
    wait_for(
        || (session_window(&mut session) == ELSEWHERE.0).then_some(()),
        "it to be placed",
    );
    assert_eq!(animating(&mut session, "victim"), None);
}

#[test]
fn a_window_of_its_own_moves_as_set_for_it() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    wm.set_animation(60_000, LINEAR);
    // Not this one.
    wm.set_window_animation(id, Some((0, LINEAR)));
    wm.place(id, ELSEWHERE, false, None);
    wait_for(
        || (session_window(&mut session) == ELSEWHERE.0).then_some(()),
        "it to be placed",
    );
    assert_eq!(animating(&mut session, "victim"), None);
    // As all of them again.
    wm.set_window_animation(id, None);
    wm.place(id, PLACE, false, None);
    wait_for(|| animating(&mut session, "victim"), "it to move");
}

#[test]
fn placed_again_on_its_way_it_goes_from_where_it_is() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    wm.set_animation(60_000, LINEAR);
    wm.place(id, ELSEWHERE, false, None);
    wait_for(|| animating(&mut session, "victim"), "it to move");
    std::thread::sleep(Duration::from_millis(300));
    let before = animating(&mut session, "victim").unwrap();
    // Back where it was: it turns around, from about where it's drawn.
    wm.place(id, PLACE, false, None);
    wait_for(
        || {
            let now = animating(&mut session, "victim")?;
            (now.0 <= before.0 + 2).then_some(())
        },
        "it to turn around",
    );
    let now = animating(&mut session, "victim").unwrap();
    assert!((now.0 - before.0).abs() <= 5, "{before:?} then {now:?}");
}

#[test]
fn an_appearing_window_starts_where_it_says() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    wm.set_animation(60_000, LINEAR);
    let _window = Window::open(&session, 0xff00ff00);
    let id = wm.window("victim").id;
    // Placed where it is (PLACE), it doesn't move...
    session.window("victim");
    assert_eq!(animating(&mut session, "victim"), None);
    // ...unless it says where it starts.
    wm.place(id, PLACE, false, Some((0, 0, 10, 10)));
    let (x, y, w, h) = wait_for(|| animating(&mut session, "victim"), "it to move");
    assert!(x < 50 && y < 50 && w < 100 && h < 100, "at {x},{y} {w}x{h}");
}

#[test]
fn a_start_needs_its_window_placed_with_it() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    wm.start_from_unplaced(id);
    wait_for(
        || (wm.failed() == 1).then_some(()),
        "the transaction to fail",
    );
}

#[test]
fn a_closed_window_stops_being_animated() {
    let mut session = Session::start();
    let wm = Wm::start(&session);
    let x = XWindow::open(&session.x_display, "xwin", "XWin", (200, 200));
    let id = wm.window("xwin").id;
    session.window("xwin");
    wm.set_animation(60_000, LINEAR);
    wm.place(id, ELSEWHERE, false, None);
    wait_for(|| animating(&mut session, "xwin"), "the window to move");
    drop(x);
    wait_for(
        || session.command("animating").is_empty().then_some(()),
        "the closed window's animation to go",
    );
}

/// The test window's colour, as captured (0xRRGGBB).
const GREEN: u32 = 0x00ff00;

#[test]
fn on_its_way_a_window_is_drawn_where_it_is_stretched() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    // Growing from a small rectangle at the top right, slowly.
    wm.set_animation(60_000, LINEAR);
    wm.place(id, ELSEWHERE, false, Some((1000, 0, 40, 30)));
    wait_for(|| animating(&mut session, "victim"), "it to move");
    let shot = screenshot(&session);
    assert_eq!(shot.at(1010, 10), GREEN, "not drawn where it starts");
    let (x, y, ..) = ELSEWHERE;
    assert_ne!(
        shot.at(x + 20, y + 20),
        GREEN,
        "drawn where it's going already"
    );
    let (x, y, ..) = PLACE;
    assert_ne!(shot.at(x + 20, y + 20), GREEN, "still drawn where it was");
}

#[test]
fn once_there_a_window_is_drawn_where_its_placed() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    wm.set_animation(200, LINEAR);
    wm.place(id, ELSEWHERE, false, None);
    wait_for(|| animating(&mut session, "victim"), "it to move");
    wait_for(
        || animating(&mut session, "victim").is_none().then_some(()),
        "it to get there",
    );
    let shot = screenshot(&session);
    let (x, y, w, h) = ELSEWHERE;
    assert_eq!(
        shot.at(x + 20, y + 20),
        GREEN,
        "not drawn where it's placed"
    );
    assert_eq!(
        shot.at(x + w - 20, y + h - 20),
        GREEN,
        "not drawn its whole size"
    );
    let (x, y, ..) = PLACE;
    assert_ne!(shot.at(x + 20, y + 20), GREEN, "still drawn where it was");
}

/// Frames drawn so far.
fn frames(session: &mut Session) -> u64 {
    session.command("frames").parse().unwrap()
}

#[test]
fn frames_are_drawn_while_a_window_moves_and_then_no_more() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session);
    wm.set_animation(500, LINEAR);
    wm.place(id, ELSEWHERE, false, None);
    wait_for(|| animating(&mut session, "victim"), "it to move");
    let before = frames(&mut session);
    std::thread::sleep(Duration::from_millis(200));
    let moving = frames(&mut session) - before;
    assert!(moving >= 5, "{moving} frames in 200 ms of moving");
    // There, and still: nothing more to draw.
    wait_for(
        || animating(&mut session, "victim").is_none().then_some(()),
        "it to get there",
    );
    std::thread::sleep(Duration::from_millis(100));
    let there = frames(&mut session);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        frames(&mut session),
        there,
        "frames drawn with nothing moving"
    );
}
