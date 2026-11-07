//! `wayluck pick`, xdg-desktop-portal-wlr's chooser_cmd: not the
//! compositor, a client of the running one. It connects to wayluck's pick
//! socket, which asks the user, and prints the line wayluck answers with,
//! the way the portal wants it ("Monitor: NAME", "Window: ID"; a workspace
//! is shared through a virtual output, so "Monitor: VIRTUAL-N"). Closed
//! without a line: cancelled. All it ever learns is what the user chose.

use std::{io::Read, os::unix::net::UnixStream, path::Path};

/// Run `wayluck pick`. The exit code: 0 with the choice printed, or 1 if
/// the user cancelled or on an error (said on stderr): the portal shares
/// nothing then.
pub fn run_pick() -> i32 {
    match ask() {
        Ok(Some(answer)) => {
            println!("{answer}");
            0
        }
        Ok(None) => 1,
        Err(e) => {
            eprintln!("wayluck pick: {e}");
            1
        }
    }
}

/// What the user chose (None: nothing).
fn ask() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR isn't set")?;
    let display = std::env::var_os("WAYLAND_DISPLAY")
        .ok_or("WAYLAND_DISPLAY isn't set: not in a wayluck session")?;
    let socket = super::pick_socket(Path::new(&dir), &display);
    let mut asking = UnixStream::connect(&socket)
        .map_err(|e| format!("can't ask wayluck ({}): {e}", socket.display()))?;
    let mut answer = String::new();
    asking.read_to_string(&mut answer)?;
    let answer = answer.trim();
    Ok((!answer.is_empty()).then(|| answer.to_string()))
}
