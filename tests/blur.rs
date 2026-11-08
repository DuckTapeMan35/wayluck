//! Blur behind windows: what's below one, blurred, seen through it.

mod harness;

use harness::{
    Session,
    capture::{Screenshot, screenshot},
    wait_for,
    wayland::Window,
    wm::Wm,
};

const RED: u32 = 0xff0000;
const BLUE: u32 = 0x0000ff;
const ON: u32 = 2;
const OFF: u32 = 1;

/// The first screenshot in which f holds.
fn shown(session: &Session, what: &str, f: impl Fn(&Screenshot) -> bool) -> Screenshot {
    wait_for(
        || {
            let shot = screenshot(session);
            f(&shot).then_some(shot)
        },
        what,
    )
}

fn red(c: u32) -> u32 {
    c >> 16
}

fn blue(c: u32) -> u32 {
    c & 0xff
}

/// A red window and a blue one, placed at red and blue, and a clear one
/// ("glass") over them at glass, on top: their ids, and the windows.
fn scene(
    session: &mut Session,
    red: (i32, i32, i32, i32),
    blue: (i32, i32, i32, i32),
    glass: (i32, i32, i32, i32),
) -> (Wm, [u32; 3], Vec<Window>) {
    let wm = Wm::start(session);
    let mut windows = Vec::new();
    let mut ids = [0; 3];
    for (i, (title, argb)) in [("red", 0xffff0000), ("blue", 0xff0000ff), ("glass", 0)]
        .into_iter()
        .enumerate()
    {
        windows.push(Window::titled(session, argb, title));
        ids[i] = wm.window(title).id;
    }
    for (id, rect) in ids.into_iter().zip([red, blue, glass]) {
        wm.place(id, rect, true, None);
    }
    (wm, ids, windows)
}

/// Red left of blue, meeting at x 300; glass over where they meet.
fn side_by_side(session: &mut Session) -> (Wm, [u32; 3], Vec<Window>) {
    let s = scene(
        session,
        (100, 100, 200, 200),
        (300, 100, 200, 200),
        (200, 150, 200, 100),
    );
    shown(session, "them placed", |s| {
        s.at(295, 200) == RED && s.at(305, 200) == BLUE
    });
    s
}

#[test]
fn behind_a_blurred_window_what_meets_below_is_mixed() {
    let mut session = Session::start();
    let (wm, [_, _, glass], _windows) = side_by_side(&mut session);
    wm.blur(glass, ON);
    let shot = shown(&session, "it blurred", |s| s.at(295, 200) != RED);
    // Each side still more its own colour.
    let (left, right) = (shot.at(290, 200), shot.at(310, 200));
    assert!(red(left) > blue(left) && blue(left) > 0, "left: {left:06x}");
    assert!(
        blue(right) > red(right) && red(right) > 0,
        "right: {right:06x}"
    );
    // Far from where they meet, as it was.
    let far = shot.at(205, 200);
    assert!(red(far) > 0xf0 && blue(far) < 0x10, "far left: {far:06x}");
    // Not around the window.
    assert_eq!(shot.at(295, 140), RED);
    assert_eq!(shot.at(305, 260), BLUE);
}

#[test]
fn blur_is_the_right_way_up() {
    let mut session = Session::start();
    // Red above blue, meeting at y 200.
    let (wm, [_, _, glass], _windows) = scene(
        &mut session,
        (100, 100, 200, 100),
        (100, 200, 200, 100),
        (150, 150, 100, 100),
    );
    shown(&session, "them placed", |s| {
        s.at(200, 195) == RED && s.at(200, 205) == BLUE
    });
    wm.blur(glass, ON);
    let shot = shown(&session, "it blurred", |s| s.at(200, 195) != RED);
    let (above, below) = (shot.at(200, 190), shot.at(200, 210));
    assert!(red(above) > blue(above), "above: {above:06x}");
    assert!(blue(below) > red(below), "below: {below:06x}");
}

#[test]
fn a_window_not_blurred_shows_what_is_below_as_it_is() {
    let mut session = Session::start();
    let (wm, [_, _, glass], _windows) = side_by_side(&mut session);
    wm.blur(glass, ON);
    shown(&session, "it blurred", |s| s.at(295, 200) != RED);
    wm.blur(glass, OFF);
    shown(&session, "it clear again", |s| {
        s.at(295, 200) == RED && s.at(305, 200) == BLUE
    });
}

#[test]
fn when_whats_below_changes_it_is_blurred_again() {
    let mut session = Session::start();
    let (wm, [_, blue_id, glass], _windows) = side_by_side(&mut session);
    wm.blur(glass, ON);
    let before = shown(&session, "it blurred", |s| s.at(295, 200) != RED);
    // Blue goes: red, and the background, are what's below.
    wm.place(blue_id, (900, 500, 200, 200), true, None);
    shown(&session, "it blurred again", |s| {
        blue(s.at(310, 200)) < blue(before.at(310, 200)) / 2
    });
}

#[test]
fn stronger_blur_reaches_farther() {
    let mut session = Session::start();
    let (wm, [_, _, glass], _windows) = side_by_side(&mut session);
    wm.blur(glass, ON);
    let weak = shown(&session, "it blurred", |s| s.at(295, 200) != RED);
    wm.set_blur(5, 3.0);
    // Something that isn't seen until the next frame: placed again.
    wm.blur(glass, ON);
    let strong = shown(&session, "it blurred more", |s| {
        blue(s.at(270, 200)) > blue(weak.at(270, 200)) + 8
    });
    assert!(blue(strong.at(270, 200)) > 0);
}

#[test]
fn a_window_is_blurred_behind_where_its_client_asks() {
    let mut session = Session::start();
    let (_wm, _, windows) = side_by_side(&mut session);
    let glass = &windows[2];
    // The left of it, to x 290: where red and blue meet (300) isn't in it.
    glass.ask_blur(Some((0, 0, 90, 100)));
    let shot = shown(&session, "it blurred", |s| s.at(285, 200) != RED);
    assert!(blue(shot.at(285, 200)) > 0);
    assert_eq!(shot.at(295, 200), RED, "blurred where it didn't ask");
    assert_eq!(shot.at(305, 200), BLUE, "blurred where it didn't ask");
    glass.ask_blur(None);
    shown(&session, "it clear again", |s| s.at(285, 200) == RED);
}

#[test]
fn the_window_manager_can_say_no_to_a_client_asking() {
    let mut session = Session::start();
    let (wm, [_, _, glass_id], windows) = side_by_side(&mut session);
    let glass = &windows[2];
    glass.ask_blur(Some((0, 0, 200, 100)));
    shown(&session, "it blurred", |s| s.at(295, 200) != RED);
    wm.blur(glass_id, OFF);
    shown(&session, "it clear", |s| s.at(295, 200) == RED);
}
