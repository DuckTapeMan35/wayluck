//! wayluck under load: what a session with many windows needs not to fail.

mod harness;

use harness::{Session, wayland};

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
