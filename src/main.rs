//! wayluck: a Wayland compositor that doesn't decide where windows go.

mod backend;
mod capture;
mod clipboard;
mod input;
mod layers;
mod pick;
mod render;
mod session;
mod state;
mod surfaces;
#[cfg(feature = "test-harness")]
mod testing;
mod windows;
mod wm;
mod xwayland;

use smithay::reexports::{
    calloop::{
        EventLoop,
        timer::{TimeoutAction, Timer},
    },
    wayland_server::Display,
};
pub use state::Wayluck;
use std::time::Duration;

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

    session::raise_open_files_limit();

    let mut event_loop: EventLoop<Wayluck> = EventLoop::try_new()?;
    let display: Display<Wayluck> = Display::new()?;
    let mut state = Wayluck::new(&mut event_loop, display);
    tracing::info!("listening on {}", state.socket_name.to_string_lossy());

    let args = Args::parse()?;
    if args.drm {
        backend::udev::init(&mut event_loop, &mut state)?;
    } else if args.headless() {
        #[cfg(feature = "test-harness")]
        {
            testing::headless::init(&mut event_loop, &mut state)?;
            testing::control::init(&mut event_loop, &state)?;
        }
    } else {
        backend::winit::init(&mut event_loop, &mut state)?;
    }
    // Until a window manager comes: wayluck's own scene.
    state.show_own_scene();
    // X clients: before anything is started, so
    // it's told the display.
    state.start_xwayland(&event_loop.handle());

    // On the hardware, wayluck is the session: services started for it
    // hear where it is before anything else starts.
    if args.drm {
        session::import_environment(&state);
    }
    session::start(&state, args.command.as_deref());

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
