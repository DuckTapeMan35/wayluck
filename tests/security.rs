//! Wayland's guarantees, with X clients about: what a client can't see or
//! take from another. Each test gives a victim something to keep (like keys
//! typed into it), lets an attacker try every way it has, and checks
//! the victim did get it and the attacker didn't.
//!
//! cargo test --features test-harness

mod harness;
use harness::{
    Session, wayland,
    x::{KeySpy, XWindow},
};

/// evdev codes: s, e, c, r, e, t.
const SECRET: [u32; 6] = [31, 18, 46, 19, 18, 20];

#[test]
fn keys_typed_into_a_wayland_window_reach_no_x_client() {
    let mut session = Session::start();
    let victim = wayland::Window::open(&session, 0xffcc3333);
    // With no window manager, a click on it gives it the keyboard.
    session.click(50, 50);
    session.wait_for(
        || victim.seen().focused.then_some(()),
        "the victim to have the keyboard",
    );

    let spy = KeySpy::start(&session.x_display);
    session.type_keys(&SECRET);
    // The victim got them: they were typed, and typed into it.
    session.wait_for(
        || (victim.seen().keys.len() == SECRET.len()).then_some(()),
        "the victim to get the keys",
    );
    assert_eq!(victim.seen().keys, SECRET);

    let seen = spy.stop();
    assert!(
        seen.is_empty(),
        "an X client saw keys typed into a Wayland window: {seen:?}\nwayluck's log:\n{}",
        session.log()
    );
}

#[test]
fn an_x_window_clicked_gets_the_keyboard() {
    let mut session = Session::start();
    let window = XWindow::open(&session.x_display, "xvictim", "XVictim", (300, 300));
    session.click_window("xvictim");
    session.wait_for(
        || window.focused().then_some(()),
        "the X window to have focus",
    );
    session.type_keys(&SECRET);
    session.wait_for(
        || (window.keys().len() == SECRET.len()).then_some(()),
        "the X window to get the keys",
    );
    assert_eq!(window.keys(), SECRET);
}

#[test]
fn keys_typed_into_an_x_window_reach_no_wayland_window() {
    let mut session = Session::start();
    let wayland = wayland::Window::open(&session, 0xffcc3333);
    let x = XWindow::open(&session.x_display, "xvictim", "XVictim", (300, 300));
    session.click_window("xvictim");
    session.wait_for(|| x.focused().then_some(()), "the X window to have focus");
    session.type_keys(&SECRET);
    session.wait_for(
        || (x.keys().len() == SECRET.len()).then_some(()),
        "the X window to get the keys",
    );
    let seen = wayland.seen();
    assert!(
        !seen.focused && seen.keys.is_empty(),
        "a Wayland window saw keys typed into an X window: {:?}",
        seen.keys
    );
}

#[test]
fn once_a_wayland_window_has_the_keyboard_x_clients_see_no_keys() {
    let mut session = Session::start();
    // The Wayland window first (at the top left), the X window over it,
    // a step down and right: the Wayland window's top left corner shows.
    let victim = wayland::Window::open(&session, 0xffcc3333);
    let x = XWindow::open(&session.x_display, "xwindow", "XWindow", (300, 300));
    session.click_window("xwindow");
    session.wait_for(|| x.focused().then_some(()), "the X window to have focus");
    session.click(10, 10);
    session.wait_for(
        || victim.seen().focused.then_some(()),
        "the Wayland window to have the keyboard back",
    );
    // And X focus is taken away: no X window is left thinking it has it.
    session.wait_for(
        || (!x.focused()).then_some(()),
        "the X window to lose focus",
    );

    let spy = KeySpy::start(&session.x_display);
    session.type_keys(&SECRET);
    session.wait_for(
        || (victim.seen().keys.len() == SECRET.len()).then_some(()),
        "the Wayland window to get the keys",
    );
    let seen = spy.stop();
    assert!(
        seen.is_empty() && x.keys().is_empty(),
        "X clients saw keys typed into a Wayland window: spy {seen:?}, window {:?}",
        x.keys()
    );
}

#[test]
fn x_windows_are_listed_with_their_title_and_class() {
    let mut session = Session::start();
    let _x = XWindow::open(&session.x_display, "xlisted", "XListed", (200, 200));
    session.window("xlisted");
    let listed = wayland::list_windows(&session);
    assert!(
        listed.contains(&("xlisted".into(), "XListed".into())),
        "not listed: {listed:?}"
    );
}
