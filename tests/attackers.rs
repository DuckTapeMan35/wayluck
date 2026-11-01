//! The attackers themselves work: on a plain X server (Xvfb), each sees
//! what it watches for. So a security test that passes means wayluck kept
//! it out, not that the attacker was broken. Needs Xvfb and xdotool.

mod harness;
use harness::x::KeySpy;
use std::{process::Command, thread::sleep, time::Duration};

#[test]
fn key_spy_sees_keys_on_a_plain_x_server() {
    let mut xvfb = Command::new("Xvfb")
        .args([":93", "-screen", "0", "640x480x24"])
        .spawn()
        .expect("needs Xvfb");
    sleep(Duration::from_secs(1));
    let spy = KeySpy::start(":93");
    Command::new("xdotool")
        .env("DISPLAY", ":93")
        .args(["key", "s", "e"])
        .status()
        .expect("needs xdotool");
    sleep(Duration::from_millis(300));
    let seen = spy.stop();
    let _ = xvfb.kill();
    let _ = xvfb.wait();
    // Every way it watches works: raw events, its window's, the keymap.
    for how in ["raw key press", "key press", "keymap"] {
        assert!(
            seen.iter().any(|s| s.starts_with(how)),
            "no {how}: {seen:?}"
        );
    }
}
