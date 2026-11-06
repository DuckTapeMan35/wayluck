//! Clients' buffers: in shared memory (wl_shm), or, for clients drawing
//! with the GPU, as GPU memory (linux-dmabuf) instead of copying them
//! through shared memory. The backend's renderer says if it can use a
//! dmabuf (Wayluck::import_dmabuf).

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    wayland::{
        buffer::BufferHandler,
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        shm::{ShmHandler, ShmState},
    },
};

use crate::Wayluck;

impl DmabufHandler for Wayluck {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let imported = self
            .import_dmabuf
            .as_ref()
            .is_some_and(|import| import(&dmabuf));
        if imported {
            // Can only fail for a client that's gone.
            let _ = notifier.successful::<Wayluck>();
        } else {
            notifier.failed();
        }
    }
}

impl BufferHandler for Wayluck {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for Wayluck {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
