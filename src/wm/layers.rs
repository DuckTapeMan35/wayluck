//! Layer surfaces (bars, wallpapers, notifications), as the window manager
//! sees them: told of once they're on an output, with what their clients
//! call them and which layer they're on. wayluck places them; the window
//! manager only gives them a look.

use super::protocol::wayluck_wm_layer_v1::{self, WayluckWmLayerV1};
use crate::Wayluck;
use smithay::{
    desktop::LayerSurface,
    output::Output,
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, Resource, backend::ClientId,
        protocol::wl_surface::WlSurface,
    },
    wayland::{compositor::with_states, shell::wlr_layer::Layer},
};
use std::cell::Cell;

/// On a layer surface's surface: the layer its window manager last heard.
#[derive(Default)]
struct SentLayer(Cell<Option<Layer>>);

impl Wayluck {
    /// Tell the window manager (if any) about a layer surface on output,
    /// once.
    pub fn wm_layer_added(&mut self, layer: &LayerSurface, output: &Output) {
        let surface = layer.wl_surface().clone();
        if self.wm.layers.iter().any(|(s, _)| *s == surface) {
            return;
        }
        let Some((manager, client)) = self.wm.manager_and_client() else {
            return;
        };
        let Ok(resource) = client.create_resource::<WayluckWmLayerV1, WlSurface, Wayluck>(
            &self.display_handle,
            manager.version(),
            surface.clone(),
        ) else {
            return;
        };
        manager.layer(&resource);
        resource.namespace(layer.namespace().into());
        with_states(&surface, |s| {
            s.data_map.insert_if_missing(SentLayer::default)
        });
        self.wm_layer_moved(layer, &resource);
        let output = (self.wm.outputs.iter()).find(|(o, _)| o == output);
        resource.output(output.map(|(_, r)| r));
        resource.done();
        self.wm.layers.push((surface, resource));
    }

    /// A layer surface's client may have moved it to another layer: the
    /// window manager hears, if so.
    pub fn wm_layer_changed(&self, layer: &LayerSurface) {
        let found = (self.wm.layers.iter()).find(|(s, _)| s == layer.wl_surface());
        if let Some((_, resource)) = found
            && self.wm_layer_moved(layer, resource)
        {
            resource.done();
        }
    }

    /// The layer it's on, if it isn't the one last told: whether it told.
    fn wm_layer_moved(&self, layer: &LayerSurface, resource: &WayluckWmLayerV1) -> bool {
        let now = layer.layer();
        let changed = with_states(layer.wl_surface(), |s| {
            let sent = s.data_map.get::<SentLayer>()?;
            Some(sent.0.replace(Some(now)) != Some(now))
        });
        if changed != Some(true) {
            return false;
        }
        use wayluck_wm_layer_v1::Layer as L;
        resource.layer(match now {
            Layer::Background => L::Background,
            Layer::Bottom => L::Bottom,
            Layer::Top => L::Top,
            Layer::Overlay => L::Overlay,
        });
        true
    }

    /// A layer surface is gone: the window manager hears so, and should
    /// destroy it.
    pub fn wm_layer_closed(&mut self, surface: &WlSurface) {
        let gone = (self.wm.layers.iter()).position(|(s, _)| s == surface);
        if let Some(at) = gone {
            let (_, resource) = self.wm.layers.remove(at);
            resource.closed();
        }
    }
}

impl Dispatch<WayluckWmLayerV1, WlSurface> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WayluckWmLayerV1,
        _request: wayluck_wm_layer_v1::Request,
        _data: &WlSurface,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy, handled below.
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &WayluckWmLayerV1,
        _data: &WlSurface,
    ) {
        state.wm.layers.retain(|(_, r)| r != resource);
    }
}
