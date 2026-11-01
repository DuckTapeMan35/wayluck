//! wl_compositor (surfaces) and wl_shm (buffers in shared memory).

use crate::{Wayluck, state::ClientState};
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_shm,
    reexports::wayland_server::{
        Client,
        protocol::{wl_buffer, wl_surface::WlSurface},
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, add_pre_commit_hook,
        },
        shm::{ShmHandler, ShmState},
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
        super::handle_commit(self, surface);
        // A new buffer, or a change to what's drawn: a frame shows it. (The
        // damage tracker draws only what really changed.)
        self.queue_redraw();
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        // Before smithay's own hooks, which come with a role: see
        // layer_shell.rs.
        add_pre_commit_hook::<Self, _>(surface, |_, _, surface| {
            super::layer_shell::dead_layer_commit(surface);
        });
        // A new GPU buffer waits until it's drawn (syncobj.rs).
        add_pre_commit_hook::<Self, _>(surface, |state, _, surface| {
            super::syncobj::wait_until_drawn(state, surface);
        });
    }
}

impl BufferHandler for Wayluck {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Wayluck {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_compositor!(Wayluck);
delegate_shm!(Wayluck);
