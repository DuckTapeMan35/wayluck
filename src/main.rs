//! wayluck: a Wayland compositor that doesn't decide where windows go.

mod atomic;
mod cursor;
mod handlers;
mod image_capture;
mod input;
mod layers;
mod picker;
mod popups;
mod protocol;
mod render;
mod scenes;
mod screencopy;
mod state;
mod toplevel_list;
mod udev;
mod virtual_output;
mod windows;
mod winit;
mod wm;

use smithay::reexports::{
    calloop::{
        EventLoop,
        timer::{TimeoutAction, Timer},
    },
    wayland_server::Display,
};
pub use state::Wayluck;
use std::{ffi::OsStr, path::PathBuf, process::Command, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `wayluck pick`: not a compositor, a client asking the running one what
    // to share (picker.rs).
    if std::env::args().nth(1).as_deref() == Some("pick") {
        std::process::exit(picker::run_pick());
    }

    // Logs to stderr: stdout is the programs' it runs.
    let logs = tracing_subscriber::fmt().with_writer(std::io::stderr);
    match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => logs.with_env_filter(filter).init(),
        Err(_) => logs.init(),
    }

    let mut event_loop: EventLoop<Wayluck> = EventLoop::try_new()?;
    let display: Display<Wayluck> = Display::new()?;
    let mut state = Wayluck::new(&mut event_loop, display);
    // Until a window manager comes: wayluck's own scene.
    state.show_own_scene();
    tracing::info!("listening on {}", state.socket_name.to_string_lossy());

    let args = Args::parse()?;
    if args.drm {
        udev::init(&mut event_loop, &mut state)?;
    } else {
        winit::init(&mut event_loop, &mut state)?;
    }

    // On the hardware, wayluck is the session: services started for it
    // hear where it is before anything else starts.
    if args.drm {
        import_environment(&state);
    }

    // Run the command inside wayluck (the window manager, usually): -c's,
    // or else the init script, if there is one.
    let start = match &args.command {
        Some(command) => {
            let mut sh = Command::new("sh");
            sh.args(["-c", command]);
            Some(sh)
        }
        None => init_script().map(Command::new),
    };
    if let Some(mut start) = start {
        tracing::info!("running {:?}", start);
        if let Err(e) = start.envs(session_env(&state)).spawn() {
            tracing::error!("couldn't run {:?}: {e}", start.get_program());
        }
    }

    // Stop on its own, after a while (for testing).
    if let Some(after) = args.exit_after {
        event_loop
            .handle()
            .insert_source(Timer::from_duration(after), |_, _, state| {
                tracing::info!("exiting, as asked (--exit-after)");
                state.loop_signal.stop();
                TimeoutAction::Drop
            })?;
    }

    event_loop.run(None, &mut state, |state| {
        // Replies to clients are queued while handling; send them once per
        // turn of the loop.
        if let Err(e) = state.display_handle.flush_clients() {
            tracing::warn!("couldn't flush clients: {e}");
        }
    })?;
    Ok(())
}

/// What programs in wayluck's session are told about it.
fn session_env(state: &Wayluck) -> [(&'static str, &OsStr); 3] {
    [
        ("WAYLAND_DISPLAY", &state.socket_name),
        ("XDG_CURRENT_DESKTOP", OsStr::new("wayluck")),
        ("XDG_SESSION_TYPE", OsStr::new("wayland")),
    ]
}

/// Tell D-Bus and systemd's user manager about the session too, so that
/// services they start for it (portals, notification daemons) find wayluck.
/// Only on the hardware: nested, it would point the outer session's
/// services at wayluck.
fn import_environment(state: &Wayluck) {
    let env = session_env(state);
    let imported = Command::new("dbus-update-activation-environment")
        .arg("--systemd")
        .args(env.map(|(name, _)| name))
        .envs(env)
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

/// What wayluck was asked on its command line.
#[derive(Default)]
struct Args {
    /// -c <command>: run it inside wayluck (instead of the init script).
    command: Option<String>,
    /// --drm: run on the hardware, from a TTY, instead of as a window.
    drm: bool,
    /// --exit-after <seconds>: stop on its own.
    exit_after: Option<Duration>,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut parsed = Args::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-c" => parsed.command = Some(args.next().ok_or("-c needs a command")?),
                "--drm" => parsed.drm = true,
                "--exit-after" => {
                    let secs = args.next().ok_or("--exit-after needs seconds")?;
                    let secs: u64 = secs.parse().map_err(|_| format!("not seconds: {secs}"))?;
                    parsed.exit_after = Some(Duration::from_secs(secs));
                }
                _ => return Err(format!("unknown argument: {arg}")),
            }
        }
        Ok(parsed)
    }
}
