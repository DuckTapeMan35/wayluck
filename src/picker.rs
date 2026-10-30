//! What can be shared, for `wayluck pick`, xdg-desktop-portal-wlr's
//! chooser_cmd: it asks over wayluck_picker_unstable_v1, shows the user its
//! picker, and prints the user's choice for the portal. Nothing is shared by
//! asking: the user decides, in the picker.
//! Its thumbnails are captures made small here, so each holds little memory.

use crate::{
    Wayluck,
    image_capture::Source,
    protocol::picker::server::{
        wayluck_pick_v1::{self, WayluckPickV1},
        wayluck_picker_manager_v1::{self, WayluckPickerManagerV1},
    },
    scenes::{Scene, SceneId},
    windows,
};
use smithay::{
    output::Output,
    reexports::{
        calloop::{
            RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::ClientId,
        },
    },
};
use std::time::Duration;

/// How long a picker waits for the window manager's workspaces, at most.
const OFFERS_PATIENCE: Duration = Duration::from_millis(200);

/// Pickers asking, and the workspaces the window manager offers them.
#[derive(Default)]
pub struct Picking {
    /// Every pick not yet destroyed: while there's one, the window manager
    /// has been told a picker is open.
    open: Vec<WayluckPickV1>,
    /// Those waiting for the window manager's offers.
    waiting: Vec<WayluckPickV1>,
    /// Until it's done offering (or out of patience).
    timer: Option<RegistrationToken>,
    offers: Vec<Offer>,
}

/// A workspace the window manager offered: its scene, the monitor it's on,
/// and its name.
struct Offer {
    scene: SceneId,
    output: Output,
    name: String,
}

impl Wayluck {
    pub fn picker_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, WayluckPickerManagerV1, _>(1, ());
    }

    /// Someone asks what can be shared. The window manager, if any, is
    /// asked for its workspaces first.
    fn pick(&mut self, pick: WayluckPickV1) {
        let first = self.picking.open.is_empty();
        self.picking.open.push(pick.clone());
        let Some(manager) = self.wm.manager.clone() else {
            return self.answer(&pick);
        };
        self.picking.waiting.push(pick);
        if first {
            self.picking.offers.clear();
            manager.picker_opened();
        }
        if self.picking.timer.is_none() {
            let timer = Timer::from_duration(OFFERS_PATIENCE);
            let timer = self.loop_handle.insert_source(timer, |_, _, state| {
                state.picking.timer = None;
                state.offers_done();
                TimeoutAction::Drop
            });
            self.picking.timer = timer.ok();
        }
    }

    /// The window manager offers a workspace.
    pub fn offer_workspace(&mut self, scene: SceneId, output: Output, name: String) {
        self.picking.offers.retain(|o| o.scene != scene);
        self.picking.offers.push(Offer {
            scene,
            output,
            name,
        });
    }

    /// The window manager offered all it will (or took too long): pickers
    /// waiting hear what can be shared.
    pub fn offers_done(&mut self) {
        if let Some(timer) = self.picking.timer.take() {
            self.loop_handle.remove(timer);
        }
        for pick in std::mem::take(&mut self.picking.waiting) {
            self.answer(&pick);
        }
    }

    /// What can be shared: the monitors, each followed by the workspaces
    /// offered on it and their windows, or with none offered, the windows
    /// it shows; topmost first.
    fn answer(&self, pick: &WayluckPickV1) {
        for monitor in self.monitors() {
            pick.monitor(monitor.name(), monitor.description());
            let offered: Vec<&Offer> = (self.picking.offers.iter())
                .filter(|o| o.output == *monitor && self.scenes.exists(o.scene))
                .collect();
            if offered.is_empty() {
                if let Some(scene) = self.scenes.shown_on(monitor) {
                    self.send_windows(pick, scene);
                }
                continue;
            }
            for offer in offered {
                pick.workspace(offer.scene.number(), offer.name.clone());
                if let Some(scene) = self.scenes.get(offer.scene) {
                    self.send_windows(pick, scene);
                }
            }
        }
        pick.done();
    }

    /// A scene's windows, topmost first.
    fn send_windows(&self, pick: &WayluckPickV1, scene: &Scene) {
        for (id, _) in scene.windows().rev() {
            let Some(toplevel) = self.windows.get(id).and_then(|w| w.toplevel()) else {
                continue;
            };
            let (title, app_id) = windows::names(toplevel);
            let (title, app_id) = (title.unwrap_or_default(), app_id.unwrap_or_default());
            pick.window(u32::from(id), title, app_id);
        }
    }

    /// A picker is done: if it was the last, the window manager hears.
    fn pick_closed(&mut self, pick: &WayluckPickV1) {
        self.picking.waiting.retain(|p| p != pick);
        let was_open = !self.picking.open.is_empty();
        self.picking.open.retain(|p| p != pick);
        if was_open && self.picking.open.is_empty() {
            self.picking.offers.clear();
            if let Some(manager) = &self.wm.manager {
                manager.picker_closed();
            }
        }
    }
}

impl GlobalDispatch<WayluckPickerManagerV1, ()> for Wayluck {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WayluckPickerManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WayluckPickerManagerV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &WayluckPickerManagerV1,
        request: wayluck_picker_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wayluck_picker_manager_v1::Request;
        match request {
            Request::Pick { id } => {
                let pick = data_init.init(id, ());
                state.pick(pick);
            }
            Request::Thumbnail {
                session,
                source,
                width,
                height,
            } => {
                let session = data_init.init(session, ());
                let source = source.data::<Source>().cloned().unwrap_or(Source::Gone);
                let fit = (width as i32, height as i32).into();
                state.start_session(session, source, false, Some(fit));
            }
            // Captured like any source: as for an output or a window, a
            // workspace gone already gives a source that stops at once.
            Request::WorkspaceSource { source, workspace } => {
                let captured = match state.scenes.numbered(workspace) {
                    Some(scene) => Source::Scene(scene),
                    None => Source::Gone,
                };
                data_init.init(source, captured);
            }
            _ => {}
        }
    }
}

impl Dispatch<WayluckPickV1, ()> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _pick: &WayluckPickV1,
        _request: wayluck_pick_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy.
    }

    fn destroyed(state: &mut Self, _client: ClientId, pick: &WayluckPickV1, _data: &()) {
        state.pick_closed(pick);
    }
}
