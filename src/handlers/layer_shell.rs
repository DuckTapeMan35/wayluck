//! wlr-layer-shell: bars, wallpapers, notifications, launchers. They aren't
//! windows, and the window manager doesn't place them: each says which
//! edges of its output it sticks to and how much room it keeps for itself
//! (its exclusive zone). wayluck lays them out (smithay's LayerMap), and
//! what they leave is where the window manager may put windows: its
//! output's usable area.

use crate::Wayluck;
use smithay::{
    delegate_layer_shell,
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    wayland::{
        compositor::with_states,
        shell::{
            wlr_layer::{
                Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::PopupSurface,
        },
    },
};

impl WlrLayerShellHandler for Wayluck {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    /// On the output its client named, or else the first.
    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());
        let Some(output) = output else {
            tracing::warn!("{namespace:?}: no output to put it on");
            surface.send_close();
            return;
        };
        tracing::info!("layer surface {namespace:?} on {}", output.name());
        let mapped =
            layer_map_for_output(&output).map_layer(&LayerSurface::new(surface, namespace));
        if let Err(e) = mapped {
            tracing::warn!("a layer surface: {e}");
        }
    }

    /// A bar's menu: placed by its positioner, relative to the bar (popups
    /// of windows are kept on the output too; these aren't, yet).
    fn new_popup(&mut self, _parent: WlrLayerSurface, _popup: PopupSurface) {}

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let found = self.space.outputs().find_map(|output| {
            let map = layer_map_for_output(output);
            let layer = map
                .layers()
                .find(|l| l.layer_surface() == &surface)
                .cloned();
            layer.map(|layer| (output.clone(), layer))
        });
        if let Some((output, layer)) = found {
            layer_map_for_output(&output).unmap_layer(&layer);
            self.layer_gone(&layer);
            self.layers_changed(&output);
        }
    }
}
delegate_layer_shell!(Wayluck);

/// After a commit: if it's a layer surface's, its output's layers are laid
/// out again (it may have changed its size or zone), and its first commit is
/// answered with its first configure, and it may take or give back the keyboard.
/// false: it isn't one.
pub fn layer_committed(state: &mut Wayluck, surface: &WlSurface) -> bool {
    let output = state.space.outputs().find(|o| {
        layer_map_for_output(o)
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some()
    });
    let Some(output) = output.cloned() else {
        return false;
    };
    let configured = with_states(surface, |s| {
        s.data_map
            .get::<LayerSurfaceData>()
            .is_some_and(|d| d.lock().unwrap().initial_configure_sent)
    });
    let layer = {
        let mut map = layer_map_for_output(&output);
        map.arrange();
        map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .cloned()
    };
    if let Some(layer) = layer {
        if !configured {
            layer.layer_surface().send_configure();
        }
        state.layer_wants_keyboard(&layer);
    }
    state.layers_changed(&output);
    true
}
