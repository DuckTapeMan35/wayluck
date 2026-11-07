//! Outputs, as the window manager sees them: monitors, and virtual outputs
//! it made or that share a workspace. And wl_output, as clients see them.

use super::protocol::{
    wayluck_wm_output_v1::{self, WayluckWmOutputV1},
    wayluck_wm_scene_v1::WayluckWmSceneV1,
};
use crate::Wayluck;
use smithay::{
    desktop::{Space, Window, layer_map_for_output},
    output::Output,
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, Resource, backend::ClientId,
    },
    wayland::output::OutputHandler,
};

// wl_output: wayluck's outputs, as clients see them.
impl OutputHandler for Wayluck {}

impl Wayluck {
    /// A virtual output made to share a workspace: the window
    /// manager gets it, and hears which scene it shows. false: there's no
    /// window manager to.
    pub fn wm_share_workspace(&mut self, output: &Output, scene: &WayluckWmSceneV1) -> bool {
        let Some((manager, client)) = self.wm.manager_and_client() else {
            return false;
        };
        let Ok(resource) = client.create_resource::<WayluckWmOutputV1, Output, Wayluck>(
            &self.display_handle,
            manager.version(),
            output.clone(),
        ) else {
            return false;
        };
        manager.share_workspace(&resource, scene);
        send_output_state(&resource, output, &self.space);
        self.wm.outputs.push((output.clone(), resource));
        true
    }

    /// Tell the window manager (if any) about an output.
    pub fn wm_output_added(&mut self, output: &Output) {
        let Some((manager, client)) = self.wm.manager_and_client() else {
            return;
        };
        let Ok(resource) = client.create_resource::<WayluckWmOutputV1, Output, Wayluck>(
            &self.display_handle,
            manager.version(),
            output.clone(),
        ) else {
            return;
        };
        manager.output(&resource);
        send_output_state(&resource, output, &self.space);
        self.wm.outputs.push((output.clone(), resource));
    }

    /// An output moved or changed size: the window manager hears the new geometry.
    pub fn wm_output_changed(&mut self, output: &Output) {
        for (o, resource) in &self.wm.outputs {
            if o == output {
                send_output_state(resource, output, &self.space);
            }
        }
    }

    /// Something started capturing output, or everything stopped: the
    /// window manager hears.
    pub fn wm_capture(&self, output: &Output, started: bool) {
        for (o, resource) in &self.wm.outputs {
            if o == output {
                if started {
                    resource.capture_started();
                } else {
                    resource.capture_stopped();
                }
            }
        }
    }
}

/// An output's name, geometry and usable area, then done.
pub(super) fn send_output_state(
    resource: &WayluckWmOutputV1,
    output: &Output,
    space: &Space<Window>,
) {
    resource.name(output.name());
    if let Some(geo) = space.output_geometry(output) {
        resource.geometry(geo.loc.x, geo.loc.y, geo.size.w, geo.size.h);
        // What bars leave windows, in global coordinates.
        let mut usable = layer_map_for_output(output).non_exclusive_zone();
        usable.loc += geo.loc;
        resource.usable(usable.loc.x, usable.loc.y, usable.size.w, usable.size.h);
    }
    resource.done();
}

impl Dispatch<WayluckWmOutputV1, Output> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WayluckWmOutputV1,
        _request: wayluck_wm_output_v1::Request,
        _data: &Output,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy, handled below.
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &WayluckWmOutputV1,
        output: &Output,
    ) {
        state.wm.outputs.retain(|(_, r)| r != resource);
        // A virtual output goes with it (a monitor stays).
        state.remove_virtual_output(output);
    }
}
