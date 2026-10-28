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
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        shm::{ShmHandler, ShmState},
    },
};

impl CompositorHandler for Wayluck {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Keep the surface's buffer for rendering.
        on_commit_buffer_handler::<Self>(surface);
        super::handle_commit(self, surface);
        // A new buffer, or a change to what's drawn: a frame shows it. (The
        // damage tracker draws only what really changed.)
        self.queue_redraw();
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
