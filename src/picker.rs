//! Asking the user what to share (wayluck_picker_unstable_v1). Anyone may
//! ask (`wayluck pick`, xdg-desktop-portal-wlr's chooser_cmd) and hears
//! only the answer. To ask the user, wayluck starts its picker (`wayluck
//! picker`) on a connection of its own making: only that client
//! sees the host global, which lists what can be shared and captures it
//! small, workspaces off screen too. The window manager
//! is told a picker is open, and offers its workspaces first; a workspace
//! chosen is shared through a virtual output the window
//! manager keeps it on until capture stops.

use crate::{
    Wayluck,
    image_capture::Source,
    protocol::picker::server::{
        wayluck_pick_v1::{self, WayluckPickV1},
        wayluck_picker_host_v1::{self, WayluckPickerHostV1},
        wayluck_picker_manager_v1::{self, WayluckPickerManagerV1},
    },
    protocol::wayluck_wm_scene_v1::WayluckWmSceneV1,
    scenes::{Scene, SceneId},
    state::ClientState,
    windows,
};
use smithay::{
    output::Output,
    reexports::{
        calloop::{
            RegistrationToken,
            channel::{Event, Sender, channel},
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::ClientId,
        },
    },
};
use std::{
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

/// How long a picker waits for the window manager's workspaces, at most.
const OFFERS_PATIENCE: Duration = Duration::from_millis(200);

/// How long a virtual output made for a shared workspace waits to be
/// captured: if nothing has by then, it's as if capture stopped.
const SHARE_PATIENCE: Duration = Duration::from_secs(10);

/// The question being asked, if one is, and the workspaces the window
/// manager offers for it.
#[derive(Default)]
pub struct Picking {
    /// Who asked, until they hear the answer (or go).
    pick: Option<WayluckPickV1>,
    /// The picker started for it, once it has bound the host.
    host: Option<WayluckPickerHostV1>,
    /// The window manager has been told a picker is open.
    open: bool,
    /// Waiting for the window manager to offer (or run out of patience).
    timer: Option<RegistrationToken>,
    offers: Vec<Offer>,
    /// Told when a picker started exits (made with the first).
    exited: Option<Sender<()>>,
}

/// A workspace the window manager offered: its scene (and the window
/// manager's object for it), the monitor it's on, and its name.
struct Offer {
    scene: SceneId,
    wm_scene: WayluckWmSceneV1,
    output: Output,
    name: String,
}

impl Wayluck {
    pub fn picker_global(dh: &DisplayHandle) {
        dh.create_global::<Wayluck, WayluckPickerManagerV1, _>(1, ());
        dh.create_global::<Wayluck, WayluckPickerHostV1, _>(1, ());
    }

    /// Someone asks the user what to share: the picker is started, and the
    /// window manager, if any, asked for its workspaces meanwhile.
    fn pick(&mut self, pick: WayluckPickV1) {
        if self.picking.pick.is_some() || self.picking.open {
            return pick.cancelled(); // one at a time
        }
        if let Err(e) = self.start_picker() {
            tracing::warn!("can't start the picker: {e}");
            return pick.cancelled();
        }
        self.picking.pick = Some(pick);
        self.picking.open = true;
        self.picking.offers.clear();
        let Some(manager) = self.wm.manager.clone() else {
            return;
        };
        manager.picker_opened();
        let timer = Timer::from_duration(OFFERS_PATIENCE);
        let timer = self.loop_handle.insert_source(timer, |_, _, state| {
            state.picking.timer = None;
            state.offers_done();
            TimeoutAction::Drop
        });
        self.picking.timer = timer.ok();
    }

    /// `wayluck picker`, as a client on a connection made here: its end is
    /// its stdin. Only it sees the host global (ClientState::picker).
    fn start_picker(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (ours, theirs) = UnixStream::pair()?;
        let mut child = Command::new(std::env::current_exe()?)
            .arg("picker")
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            // Its window is an ordinary client's.
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .spawn()?;
        // Reaped when it exits, and wayluck told (in case it went before
        // binding the host: then nothing else says so).
        let exited = self.picker_exits()?;
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = exited.send(());
        });
        let data = ClientState {
            picker: true,
            ..ClientState::default()
        };
        self.display_handle.insert_client(ours, Arc::new(data))?;
        Ok(())
    }

    /// Where a thread waiting on a picker says it exited.
    fn picker_exits(&mut self) -> Result<Sender<()>, Box<dyn std::error::Error>> {
        if let Some(exited) = &self.picking.exited {
            return Ok(exited.clone());
        }
        let (exited, heard) = channel();
        self.loop_handle.insert_source(heard, |event, _, state| {
            // Bound, it's gone already (the host went with its client).
            if let Event::Msg(()) = event
                && state.picking.host.is_none()
                && state.picking.open
            {
                state.picker_gone();
            }
        })?;
        self.picking.exited = Some(exited.clone());
        Ok(exited)
    }

    /// The window manager offers a workspace.
    pub fn offer_workspace(&mut self, wm_scene: WayluckWmSceneV1, output: Output, name: String) {
        let Some(&scene) = wm_scene.data::<SceneId>() else {
            return;
        };
        self.picking.offers.retain(|o| o.scene != scene);
        self.picking.offers.push(Offer {
            scene,
            wm_scene,
            output,
            name,
        });
    }

    /// The user chose a workspace: a virtual output shows it, like the
    /// monitor it's on, and whoever asked hears its name. The window manager
    /// keeps it laid out, and lets it go when capture stops.
    fn share_workspace(&mut self, number: u32) {
        let offer = (self.picking.offers.iter()).find(|o| o.scene.number() == number);
        let Some(offer) = offer else {
            return self.answer(None);
        };
        let (scene, wm_scene, template) =
            (offer.scene, offer.wm_scene.clone(), offer.output.clone());
        let Some(mode) = template.current_mode() else {
            return self.answer(None);
        };
        let output = self.add_virtual_output(mode.size, Some(template), 0);
        self.scenes.show(&output, Some(scene));
        self.sync_space();
        if !self.wm_share_workspace(&output, &wm_scene) {
            self.remove_virtual_output(&output);
            return self.answer(None);
        }
        // Nothing captures it after all (the app gave up): it goes.
        let unclaimed = output.clone();
        let timer = Timer::from_duration(SHARE_PATIENCE);
        let timer = self.loop_handle.insert_source(timer, move |_, _, state| {
            let captured = state.captures.iter().any(|(o, _)| *o == unclaimed);
            if state.is_virtual(&unclaimed) && !captured {
                state.wm_capture(&unclaimed, false);
            }
            TimeoutAction::Drop
        });
        if let Err(e) = timer {
            tracing::warn!("can't watch a shared workspace's output: {e}");
        }
        self.answer(Some(format!("Monitor: {}", output.name())));
    }

    /// The window manager offered all it will (or took too long): the
    /// picker, if it's there yet, hears what can be shared.
    pub fn offers_done(&mut self) {
        if let Some(timer) = self.picking.timer.take() {
            self.loop_handle.remove(timer);
        }
        if let Some(host) = self.picking.host.clone() {
            self.list(&host);
        }
    }

    /// What can be shared: the monitors, each followed by the workspaces
    /// offered on it and their windows, or with none offered, the windows
    /// it shows; topmost first.
    fn list(&self, host: &WayluckPickerHostV1) {
        for monitor in self.monitors() {
            host.monitor(monitor.name(), monitor.description());
            let offered: Vec<&Offer> = (self.picking.offers.iter())
                .filter(|o| o.output == *monitor && self.scenes.exists(o.scene))
                .collect();
            if offered.is_empty() {
                if let Some(scene) = self.scenes.shown_on(monitor) {
                    self.send_windows(host, scene);
                }
                continue;
            }
            for offer in offered {
                host.workspace(offer.scene.number(), offer.name.clone());
                if let Some(scene) = self.scenes.get(offer.scene) {
                    self.send_windows(host, scene);
                }
            }
        }
        host.done();
    }

    /// A scene's windows, topmost first.
    fn send_windows(&self, host: &WayluckPickerHostV1, scene: &Scene) {
        for (id, _) in scene.windows().rev() {
            let Some(toplevel) = self.windows.get(id).and_then(|w| w.toplevel()) else {
                continue;
            };
            let (title, app_id) = windows::names(toplevel);
            let (title, app_id) = (title.unwrap_or_default(), app_id.unwrap_or_default());
            host.window(u32::from(id), title, app_id);
        }
    }

    /// The picker answers: whoever asked hears (Some: chosen, None:
    /// cancelled), if they're still there.
    fn answer(&mut self, answer: Option<String>) {
        let Some(pick) = self.picking.pick.take() else {
            return;
        };
        match answer {
            Some(answer) => pick.chosen(answer),
            None => pick.cancelled(),
        }
    }

    /// The picker is gone: whoever asked hears it was cancelled (unless it
    /// answered), and the window manager that it closed.
    fn picker_gone(&mut self) {
        self.answer(None);
        self.picking.host = None;
        if let Some(timer) = self.picking.timer.take() {
            self.loop_handle.remove(timer);
        }
        if std::mem::take(&mut self.picking.open) {
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
        if let wayluck_picker_manager_v1::Request::Pick { id } = request {
            let pick = data_init.init(id, ());
            state.pick(pick);
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
        // Only destroy (destroyed, below, also covers the client going).
    }

    /// Whoever asked went: the picker hears, and should close.
    fn destroyed(state: &mut Self, _client: ClientId, pick: &WayluckPickV1, _data: &()) {
        if state.picking.pick.as_ref() == Some(pick) {
            state.picking.pick = None;
            if let Some(host) = &state.picking.host {
                host.closed();
            }
        }
    }
}

impl GlobalDispatch<WayluckPickerHostV1, ()> for Wayluck {
    /// The picker is here: it hears what can be shared, once the window
    /// manager has offered its workspaces.
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WayluckPickerHostV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let host = data_init.init(resource, ());
        if state.picking.host.is_some() || !state.picking.open {
            return host.closed();
        }
        state.picking.host = Some(host.clone());
        if state.picking.pick.is_none() {
            host.closed();
        } else if state.picking.timer.is_none() {
            state.list(&host);
        }
    }

    /// Only the picker wayluck started sees it.
    fn can_view(client: Client, _data: &()) -> bool {
        client.get_data::<ClientState>().is_some_and(|c| c.picker)
    }
}

impl Dispatch<WayluckPickerHostV1, ()> for Wayluck {
    fn request(
        state: &mut Self,
        _client: &Client,
        host: &WayluckPickerHostV1,
        request: wayluck_picker_host_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use wayluck_picker_host_v1::Request;
        let mine = state.picking.host.as_ref() == Some(host);
        match request {
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
            Request::ShareWorkspace { workspace } if mine => state.share_workspace(workspace),
            // Captured like any source: as for an output or a window, a
            // workspace gone already gives a source that stops at once.
            Request::WorkspaceSource { source, workspace } => {
                let captured = match state.scenes.numbered(workspace) {
                    Some(scene) => Source::Scene(scene),
                    None => Source::Gone,
                };
                data_init.init(source, captured);
            }
            Request::Choose { answer } if mine => state.answer(Some(answer)),
            Request::Cancel if mine => state.answer(None),
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, host: &WayluckPickerHostV1, _data: &()) {
        if state.picking.host.as_ref() == Some(host) {
            state.picker_gone();
        }
    }
}
