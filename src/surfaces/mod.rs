//! wl_compositor: every client surface's commits, before whatever role it
//! has (window, bar, popup) gets them.

mod buffers;
mod syncobj;

use crate::{
    Wayluck, layers::shell::dead_layer_commit, state::ClientState, windows::handle_commit,
};
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    reexports::wayland_server::{Client, protocol::wl_surface::WlSurface},
    wayland::compositor::{
        CompositorClientState, CompositorHandler, CompositorState, add_pre_commit_hook,
    },
    xwayland::XWaylandClientData,
};

/// A client's surfaces' state: every client is wayluck's (ClientState), but
/// Xwayland, which smithay connects with its own data.
pub fn compositor_state_of(client: &Client) -> Option<&CompositorClientState> {
    if let Some(data) = client.get_data::<ClientState>() {
        return Some(&data.compositor_state);
    }
    (client.get_data::<XWaylandClientData>()).map(|data| &data.compositor_state)
}

impl CompositorHandler for Wayluck {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        compositor_state_of(client).expect("a client without wayluck's or Xwayland's data")
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Keep the surface's buffer for rendering.
        on_commit_buffer_handler::<Self>(surface);
        handle_commit(self, surface);
        // A new buffer, or a change to what's drawn: a frame shows it. (The
        // damage tracker draws only what really changed.)
        self.queue_redraw();
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        // Before smithay's own hooks, which come with a role
        add_pre_commit_hook::<Self, _>(surface, |_, _, surface| {
            dead_layer_commit(surface);
        });
        // A new GPU buffer waits until it's drawn.
        add_pre_commit_hook::<Self, _>(surface, |state, _, surface| {
            syncobj::wait_until_drawn(state, surface);
        });
    }
}
