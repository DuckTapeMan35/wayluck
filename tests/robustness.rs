//! wayluck under load: what a session with many windows needs not to fail.

mod harness;

use harness::{Session, wayland};
use std::path::Path;
use x11rb::rust_connection::RustConnection;

/// Every window costs wayluck open files (its client's connection, its
/// buffers, their sync points). With the limit a session starts with,
/// wayluck ran out at around a hundred windows and exited; it raises its
/// own limit as far as the system lets it.
#[test]
fn many_windows_dont_run_wayluck_out_of_open_files() {
    // A limit of 256: each of these windows takes at least two.
    let session = Session::start_with_open_files(256);
    let windows: Vec<_> = (0..300)
        .map(|i| wayland::Window::open(&session, 0xff000000 | (i * 997)))
        .collect();
    assert!(windows.iter().all(|w| w.seen().mapped));
}

/// What wayluck starts (the window manager, and through it every program)
/// gets the limit wayluck itself started with, not the one it raised: some
/// programs misbehave with file descriptors above 1024.
#[test]
fn programs_wayluck_starts_get_the_usual_open_files_limit() {
    let session = Session::start_with(Some(256), Some("ulimit -Sn > \"$XDG_RUNTIME_DIR/limit\""));
    let file = session.dir().join("limit");
    let limit = harness::wait_for(
        || {
            std::fs::read_to_string(&file)
                .ok()
                .filter(|s| s.ends_with('\n'))
        },
        "the program to say its limit",
    );
    assert_eq!(limit.trim(), "256");
}

/// Logging out (SIGTERM): wayluck stops, and what it made goes with it, so
/// the next session doesn't find its sockets and lock files.
#[test]
fn asked_to_stop_wayluck_leaves_nothing_behind() {
    let mut session = Session::start();
    // X started (by an X program connecting), as it'd be in a session.
    let (x, _) = RustConnection::connect(Some(&session.x_display)).unwrap();
    let answer = session.command("xwayland");
    let xwayland = answer.strip_prefix("running :").expect("Xwayland running");
    let door = session.x_display.trim_start_matches(':').to_string();
    drop(x);
    let socket = session.dir().join(&session.wayland_display);
    assert!(socket.exists());

    let status = session.terminate();
    assert!(status.success(), "{status}");
    assert!(!socket.exists(), "its socket");
    for display in [door.as_str(), xwayland] {
        assert!(
            !Path::new(&format!("/tmp/.X{display}-lock")).exists(),
            ":{display}'s lock file"
        );
        let x_socket = format!("/tmp/.X11-unix/X{display}");
        assert!(!Path::new(&x_socket).exists(), ":{display}'s socket");
    }
}

/// What wayluck starts gets the signals as usual (none blocked): stopping
/// on them didn't take them from its programs.
#[test]
fn programs_wayluck_starts_can_be_signalled() {
    let session = Session::start_with(
        None,
        Some("grep SigBlk /proc/self/status > \"$XDG_RUNTIME_DIR/blocked\""),
    );
    let file = session.dir().join("blocked");
    let blocked = harness::wait_for(
        || {
            std::fs::read_to_string(&file)
                .ok()
                .filter(|s| s.ends_with('\n'))
        },
        "the program to say what it blocks",
    );
    let mask =
        u64::from_str_radix(blocked.trim().trim_start_matches("SigBlk:").trim(), 16).unwrap();
    assert_eq!(mask, 0, "{blocked}");
}
