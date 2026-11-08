//! Shadows below windows: where they're drawn, and where not.

mod harness;

use harness::{
    Session,
    capture::{Screenshot, screenshot},
    wait_for,
    wayland::Window,
    wm::{PLACE, Wm},
};

const RED: u32 = 0xff0000;
const GREEN: u32 = 0x00ff00;

/// A window of colour argb ("victim"), placed at PLACE by a window
/// manager, and its id.
fn placed_window(session: &mut Session, argb: u32) -> (Wm, Window, u32) {
    let wm = Wm::start(session);
    let window = Window::open(session, argb);
    let id = wm.window("victim").id;
    session.window("victim");
    (wm, window, id)
}

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

#[test]
fn a_shadow_is_drawn_below_a_window_where_its_moved() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session, 0xff00ff00);
    let (x, y, w, h) = PLACE;
    shown(&session, "the window", |s| s.at(x + 10, y + 10) == GREEN);
    wm.shadow(id, 0, 0, (20, 20), 0xffff0000);
    let shot = shown(&session, "the shadow", |s| {
        s.at(x + w + 10, y + h + 10) == RED
    });
    // Moved right and down: not to the left of where it's moved, nor above.
    assert_ne!(shot.at(x + 10, y + h + 10), RED);
    assert_ne!(shot.at(x + w + 10, y + 10), RED);
    assert_eq!(shot.at(x + 30, y + h + 10), RED);
    // The window over it.
    assert_eq!(shot.at(x + w - 10, y + h - 10), GREEN);
}

#[test]
fn a_blurred_shadow_fades_out_as_far_as_its_blur() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session, 0xff00ff00);
    let (x, y, w, h) = PLACE;
    let empty = shown(&session, "the window", |s| s.at(x + 10, y + 10) == GREEN);
    let background = empty.at(x + w + 15, y + h / 2);
    wm.shadow(id, 30, 0, (0, 0), 0xffff0000);
    let shot = shown(&session, "the shadow", |s| {
        s.at(x + w + 2, y + h / 2) != background
    });
    let near = red(shot.at(x + w + 2, y + h / 2));
    let farther = red(shot.at(x + w + 15, y + h / 2));
    assert!(near > farther, "{near} by the window, {farther} farther");
    assert!(near < 0xff, "not blurred: {near}");
    assert_eq!(shot.at(x + w + 40, y + h / 2), background, "past its blur");
}

#[test]
fn a_shadow_with_no_alpha_isnt_drawn() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session, 0xff00ff00);
    let (x, y, w, h) = PLACE;
    let empty = shown(&session, "the window", |s| s.at(x + 10, y + 10) == GREEN);
    wm.shadow(id, 0, 10, (0, 0), 0xffff0000);
    shown(&session, "the shadow", |s| {
        s.at(x + w + 5, y + h / 2) == RED
    });
    wm.shadow(id, 0, 10, (0, 0), 0x00ff0000);
    shown(&session, "it gone", |s| {
        s.at(x + w + 5, y + h / 2) == empty.at(x + w + 5, y + h / 2)
    });
}

#[test]
fn a_shadow_isnt_drawn_under_a_translucent_window() {
    let mut session = Session::start();
    let (wm, _window, id) = placed_window(&mut session, 0x8000ff00);
    let (x, y, w, h) = PLACE;
    let empty = shown(&session, "the window", |s| {
        s.at(x + 10, y + 10) != s.at(5, 5)
    });
    wm.shadow(id, 0, 10, (0, 0), 0xffff0000);
    let shot = shown(&session, "the shadow", |s| {
        s.at(x + w + 5, y + h / 2) == RED
    });
    assert_eq!(
        shot.at(x + w / 2, y + h / 2),
        empty.at(x + w / 2, y + h / 2)
    );
}
