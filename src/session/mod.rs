//! wayluck's session: what programs started in it are told, the command
//! (or init script) that starts it, and the services that hear of it.

mod programs;

use crate::Wayluck;
pub use programs::raise_open_files_limit;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic};
use std::{ffi::OsStr, os::unix::net::UnixStream, path::PathBuf};

/// Run the command inside wayluck (the window manager, usually): -c's, or
/// else the init script, if there is one.
pub fn start(state: &Wayluck, command: Option<&str>) {
    let start = match command {
        Some(command) => {
            let mut sh = programs::command("sh");
            sh.args(["-c", command]);
            Some(sh)
        }
        None => init_script().map(programs::command),
    };
    if let Some(mut start) = start {
        tracing::info!("running {:?}", start);
        if let Err(e) = start.envs(session_env(state)).spawn() {
            tracing::error!("couldn't run {:?}: {e}", start.get_program());
        }
    }
}

/// What programs in wayluck's session are told about it: DISPLAY too, if
/// Xwayland started.
fn session_env(state: &Wayluck) -> Vec<(&'static str, &OsStr)> {
    let mut env = vec![
        ("WAYLAND_DISPLAY", state.socket_name.as_os_str()),
        ("XDG_CURRENT_DESKTOP", OsStr::new("wayluck")),
        ("XDG_SESSION_TYPE", OsStr::new("wayland")),
    ];
    if let Some(display) = &state.x_display {
        env.push(("DISPLAY", display.as_os_str()));
    }
    env
}

/// Tell D-Bus and systemd's user manager about the session too, so that
/// services they start for it (portals, notification daemons) find wayluck.
/// Only on the hardware: nested, it would point the outer session's
/// services at wayluck.
pub fn import_environment(state: &Wayluck) {
    let env = session_env(state);
    let imported = programs::command("dbus-update-activation-environment")
        .arg("--systemd")
        .args(env.iter().map(|(name, _)| name))
        .envs(env.iter().copied())
        .status();
    match imported {
        Ok(status) if status.success() => tracing::info!("session environment imported"),
        Ok(status) => tracing::warn!("dbus-update-activation-environment: {status}"),
        Err(e) => tracing::warn!("couldn't run dbus-update-activation-environment: {e}"),
    }
}

/// $XDG_CONFIG_HOME/wayluck/init (~/.config/wayluck/init), if it exists:
/// what runs when wayluck isn't given -c. Starting the window manager, say.
fn init_script() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let init = config.join("wayluck").join("init");
    init.is_file().then_some(init)
}

/// SIGTERM (logging out), SIGINT (^C), SIGHUP (its terminal gone): the
/// loop stops, so main returns and what wayluck made goes with it (its
/// socket, X's sockets and lock files) instead of being left behind. A
/// handler writes to a socket the loop watches; the signals aren't blocked,
/// so the programs wayluck starts get them as usual.
pub fn stop_on_signals(event_loop: &EventLoop<'static, Wayluck>) -> std::io::Result<()> {
    let (heard, told) = UnixStream::pair()?;
    for signal in [SIGTERM, SIGINT, SIGHUP] {
        signal_hook::low_level::pipe::register(signal, told.try_clone()?)?;
    }
    let source = Generic::new(heard, Interest::READ, Mode::Level);
    let inserted = event_loop.handle().insert_source(source, |_, _, state| {
        tracing::info!("stopping: asked to by a signal");
        state.loop_signal.stop();
        Ok(PostAction::Remove)
    });
    inserted.map_err(|e| e.error)?;
    Ok(())
}
