//! wayluck: a Wayland compositor that doesn't decide where windows go.

mod atomic;
mod handlers;
mod input;
mod popups;
mod protocol;
mod render;
mod state;
mod windows;
mod winit;
mod wm;

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
pub use state::Wayluck;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => tracing_subscriber::fmt().with_env_filter(filter).init(),
        Err(_) => tracing_subscriber::fmt().init(),
    }

    let mut event_loop: EventLoop<Wayluck> = EventLoop::try_new()?;
    let display: Display<Wayluck> = Display::new()?;
    let mut state = Wayluck::new(&mut event_loop, display);
    tracing::info!("listening on {}", state.socket_name.to_string_lossy());

    winit::init(&mut event_loop, &mut state)?;

    // wayluck -c <command>: run it inside wayluck (the window manager, usually).
    let mut args = std::env::args().skip(1);
    if let (Some("-c"), Some(command)) = (args.next().as_deref(), args.next()) {
        tracing::info!("running {command}");
        std::process::Command::new("sh")
            .args(["-c", &command])
            .env("WAYLAND_DISPLAY", &state.socket_name)
            .spawn()?;
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
