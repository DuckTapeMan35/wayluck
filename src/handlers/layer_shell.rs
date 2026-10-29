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
                Anchor, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceCachedState,
                LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState,
            },
            xdg::PopupSurface,
        },
    },
};
use std::cell::Cell;

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
        layer_reborn(surface.wl_surface());
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
        layer_died(surface.wl_surface());
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

// Bullshit is afoot here

/// On a wl_surface whose layer surface its client destroyed. smithay resets
/// such a surface's layer state to 0×0 and unanchored, but keeps checking
/// it on commit (smithay#1979). That combination is a protocol error, so
/// the next commit cuts the client off, though the protocol allows it
/// (quickshell and gtk4-layer-shell hide a panel that way).
#[derive(Default)]
struct LayerGone(Cell<bool>);

/// Its layer surface is gone: mark it.
fn layer_died(surface: &WlSurface) {
    with_states(surface, |s| {
        s.data_map.insert_if_missing(LayerGone::default);
        s.data_map.get::<LayerGone>().unwrap().0.set(true);
    });
}

/// It's a layer surface again: no longer marked, and unanchored, as smithay
/// would have it.
fn layer_reborn(surface: &WlSurface) {
    with_states(surface, |s| {
        if s.data_map
            .get::<LayerGone>()
            .is_some_and(|g| g.0.replace(false))
        {
            s.cached_state
                .get::<LayerSurfaceCachedState>()
                .pending()
                .anchor = Anchor::empty();
        }
    });
}

/// Before each commit, ahead of smithay's check: a marked surface's dead
/// layer state is anchored to every edge, so the check passes (nothing
/// draws it any more).
pub fn dead_layer_commit(surface: &WlSurface) {
    with_states(surface, |s| {
        if s.data_map.get::<LayerGone>().is_some_and(|g| g.0.get()) {
            s.cached_state
                .get::<LayerSurfaceCachedState>()
                .pending()
                .anchor = Anchor::all();
        }
    });
}
