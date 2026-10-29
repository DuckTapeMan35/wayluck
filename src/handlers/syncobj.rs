//! When a GPU client's buffer may be shown: once the GPU has finished
//! drawing it. A client says when that is with linux-drm-syncobj (explicit
//! sync: a point on a timeline, signalled when the drawing is done), or the
//! buffer itself says so (implicit sync: its dmabuf becomes readable).
//! NVIDIA's driver needs explicit sync: without it, a buffer can be shown
//! half drawn (Electron apps flicker). The release point, signalled when
//! wayluck is done reading a buffer, smithay handles.

use std::io;

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_drm_syncobj,
    reexports::{
        calloop::{EventSource, Interest},
        wayland_server::{Resource, protocol::wl_surface::WlSurface},
    },
    wayland::{
        compositor::{
            Blocker, BufferAssignment, CompositorHandler, SurfaceAttributes, add_blocker,
            with_states,
        },
        dmabuf::get_dmabuf,
        drm_syncobj::{DrmSyncPoint, DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState},
    },
};

use crate::Wayluck;

impl DrmSyncobjHandler for Wayluck {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.syncobj_state.as_mut()
    }
}
delegate_drm_syncobj!(Wayluck);

/// Before a commit (a pre-commit hook): one with a new GPU buffer waits
/// until the buffer is drawn (its acquire point, or else its dmabuf). Only
/// this surface's commits wait; everything else goes on.
pub fn wait_until_drawn(state: &mut Wayluck, surface: &WlSurface) {
    let (acquire, dmabuf) = with_states(surface, |s| {
        let acquire = (s.cached_state.get::<DrmSyncobjCachedState>().pending())
            .acquire_point
            .clone();
        let dmabuf = match &s.cached_state.get::<SurfaceAttributes>().pending().buffer {
            Some(BufferAssignment::NewBuffer(buffer)) => get_dmabuf(buffer).cloned().ok(),
            _ => None,
        };
        (acquire, dmabuf)
    });
    let Some(dmabuf) = dmabuf else {
        return; // shared memory: drawn by the CPU, done when committed
    };
    match acquire {
        Some(acquire) => explicit(state, surface, &acquire),
        None => implicit(state, surface, &dmabuf),
    }
}

fn explicit(state: &mut Wayluck, surface: &WlSurface, acquire: &DrmSyncPoint) {
    match acquire.generate_blocker() {
        Ok((blocker, source)) => wait(state, surface, blocker, source),
        Err(e) => tracing::warn!("can't wait for a buffer's acquire point: {e}"),
    }
}

fn implicit(state: &mut Wayluck, surface: &WlSurface, dmabuf: &Dmabuf) {
    // Err: it's drawn already.
    if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
        wait(state, surface, blocker, source);
    }
}

/// Hold surface's commit back (blocker) until source fires, then let its
/// client's held commits through.
fn wait<S>(
    state: &mut Wayluck,
    surface: &WlSurface,
    blocker: impl Blocker + Send + 'static,
    source: S,
) where
    S: EventSource<Event = (), Ret = io::Result<()>> + 'static,
{
    let Some(client) = surface.client() else {
        return;
    };
    let ready = state.loop_handle.insert_source(source, move |_, _, state| {
        let dh = state.display_handle.clone();
        state
            .client_compositor_state(&client)
            .blocker_cleared(state, &dh);
        Ok(())
    });
    // Couldn't watch it: shown without waiting, as before.
    if ready.is_ok() {
        add_blocker(surface, blocker);
    }
}
