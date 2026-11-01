//! Wayland's guarantees, with X clients about: what a client can't see or
//! take from another. Each test gives a victim something to keep (like keys
//! typed into it), lets an attacker try every way it has, and checks
//! the victim did get it and the attacker didn't.
//!
//! cargo test --features test-harness

mod harness;
use harness::{Session, wayland, x::KeySpy};

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
