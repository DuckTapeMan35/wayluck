//! wayluck: a Wayland compositor that doesn't decide where windows go.

mod handlers;
mod render;
mod state;
mod windows;
mod winit;

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

    event_loop.run(None, &mut state, |state| {
        // Replies to clients are queued while handling; send them once per
        // turn of the loop.
        if let Err(e) = state.display_handle.flush_clients() {
            tracing::warn!("couldn't flush clients: {e}");
        }
    })?;
    Ok(())
}
