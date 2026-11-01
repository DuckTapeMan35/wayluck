//! wayluck: a Wayland compositor that doesn't decide where windows go.

mod atomic;
mod cursor;
mod focus;
mod handlers;
#[cfg(feature = "test-harness")]
mod headless;
mod image_capture;
mod input;
mod layers;
mod pick;
mod picker;
mod popups;
mod protocol;
mod render;
mod rounded;
mod scenes;
mod screencopy;
mod state;
#[cfg(feature = "test-harness")]
mod test_control;
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
    // `wayluck pick` and `wayluck picker` aren't the compositor: clients of
    // the running one, asking the user what to share.
    match std::env::args().nth(1).as_deref() {
        Some("pick") => std::process::exit(pick::run_pick()),
        Some("picker") => std::process::exit(pick::run_picker()),
        _ => {}
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
    tracing::info!("listening on {}", state.socket_name.to_string_lossy());

    let args = Args::parse()?;
    if args.drm {
        udev::init(&mut event_loop, &mut state)?;
    } else if args.headless() {
        #[cfg(feature = "test-harness")]
        {
            headless::init(&mut event_loop, &mut state)?;
            test_control::init(&mut event_loop, &state)?;
        }
    } else {
        winit::init(&mut event_loop, &mut state)?;
    }
    // Until a window manager comes: wayluck's own scene (scenes.rs).
    state.show_own_scene();
    // X clients (handlers/xwayland.rs): before anything is started, so
    // it's told the display.
    state.start_xwayland(&event_loop.handle());

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
fn import_environment(state: &Wayluck) {
    let env = session_env(state);
    let imported = Command::new("dbus-update-activation-environment")
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

/// What wayluck was asked on its command line.
#[derive(Default)]
struct Args {
    /// -c <command>: run it inside wayluck (instead of the init script).
    command: Option<String>,
    /// --drm: run on the hardware, from a TTY, instead of as a window.
    drm: bool,
    /// --headless (tests only): an output drawn offscreen, no window.
    #[cfg(feature = "test-harness")]
    headless: bool,
    /// --exit-after <seconds>: stop on its own.
    exit_after: Option<Duration>,
}

impl Args {
    /// --headless, which only test builds have.
    fn headless(&self) -> bool {
        #[cfg(feature = "test-harness")]
        return self.headless;
        #[cfg(not(feature = "test-harness"))]
        false
    }

    fn parse() -> Result<Self, String> {
        let mut parsed = Args::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-c" => parsed.command = Some(args.next().ok_or("-c needs a command")?),
                "--drm" => parsed.drm = true,
                #[cfg(feature = "test-harness")]
                "--headless" => parsed.headless = true,
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
