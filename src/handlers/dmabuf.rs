//! linux-dmabuf: clients drawing with the GPU hand over their buffers as
//! GPU memory (dmabufs) instead of copying them through shared memory. The
//! backend's renderer says if it can use one (Wayluck::import_dmabuf).

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_dmabuf,
    wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
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
delegate_dmabuf!(Wayluck);
