//! Asking the user what to share. Whoever connects to the pick socket asks
//! (`wayluck pick`, xdg-desktop-portal-wlr's chooser_cmd), and hears only
//! the answer: one line, or the connection closed if the user chose
//! nothing. wayluck asks the user itself, with a picker it draws over a
//! monitor. The window manager is told a picker is open, and offers
//! its workspaces first; a workspace chosen is shared through a virtual
//! output the window manager keeps it on until capture stops.

mod choosing;
pub mod overlay;
mod pick;

pub use pick::run_pick;

use crate::{
    Wayluck, render::text::Text, wm::protocol::wayluck_wm_scene_v1::WayluckWmSceneV1,
    wm::scenes::SceneId,
};
use smithay::{
    output::Output,
    reexports::{
        calloop::{
            Interest, LoopHandle, Mode, PostAction, RegistrationToken,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::Resource,
    },
};
use std::{
    ffi::{OsStr, OsString},
    io::{ErrorKind, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
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
    asker: Option<Asker>,
    /// The window manager has been told a picker is open.
    open: bool,
    /// Waiting for the window manager to offer (or run out of patience).
    timer: Option<RegistrationToken>,
    offers: Vec<Offer>,
    /// The picker, once it's up.
    shown: Option<overlay::Shown>,
    /// The labels' font, once looked for (None in it: there's none).
    text: Option<Option<Text>>,
}

/// Whoever asked: their connection, and what watches it for them going.
struct Asker {
    connection: UnixStream,
    watch: RegistrationToken,
}

/// Where whoever wants the user asked connects: in the runtime directory,
/// named after the Wayland socket.
pub fn pick_socket(runtime_dir: &Path, wayland_display: &OsStr) -> PathBuf {
    let mut name = OsString::from("wayluck-pick.");
    name.push(wayland_display);
    runtime_dir.join(name)
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
    /// Listen on the pick socket, for the session whose Wayland socket is
    /// wayland_display.
    pub fn listen_for_picks(handle: &LoopHandle<'static, Wayluck>, wayland_display: &OsStr) {
        let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
            return tracing::warn!("no pick socket: XDG_RUNTIME_DIR isn't set");
        };
        let path = pick_socket(Path::new(&dir), wayland_display);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).and_then(|l| {
            l.set_nonblocking(true)?;
            Ok(l)
        });
        let listener = match listener {
            Ok(listener) => listener,
            Err(e) => return tracing::warn!("no pick socket at {}: {e}", path.display()),
        };
        let source = Generic::new(listener, Interest::READ, Mode::Level);
        let added = handle.insert_source(source, |_, listener, state| {
            while let Ok((connection, _)) = listener.accept() {
                state.pick(connection);
            }
            Ok(PostAction::Continue)
        });
        if let Err(e) = added {
            tracing::warn!("no pick socket: {e}");
        }
    }

    /// Someone asks the user what to share: the picker goes up, once the
    /// window manager, if any, has offered its workspaces.
    fn pick(&mut self, connection: UnixStream) {
        if self.picking.asker.is_some() || self.picking.open {
            return; // one at a time: closed, as if cancelled
        }
        let Some(watch) = self.watch_asker(&connection) else {
            return;
        };
        self.picking.asker = Some(Asker { connection, watch });
        self.picking.open = true;
        self.picking.offers.clear();
        let Some(manager) = self.wm.manager.clone() else {
            return self.show_picker();
        };
        manager.picker_opened();
        let timer = Timer::from_duration(OFFERS_PATIENCE);
        let timer = self.loop_handle.insert_source(timer, |_, _, state| {
            state.picking.timer = None;
            state.show_picker();
            TimeoutAction::Drop
        });
        self.picking.timer = timer.ok();
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
    /// picker goes up.
    pub fn offers_done(&mut self) {
        let Some(timer) = self.picking.timer.take() else {
            return; // not waiting for offers
        };
        self.loop_handle.remove(timer);
        self.show_picker();
    }

    /// Whoever asked going (hanging up) closes the picker.
    fn watch_asker(&mut self, connection: &UnixStream) -> Option<RegistrationToken> {
        let watched = connection.try_clone().and_then(|c| {
            c.set_nonblocking(true)?;
            Ok(c)
        });
        let source = Generic::new(watched.ok()?, Interest::READ, Mode::Level);
        let watch = self
            .loop_handle
            .insert_source(source, |_, connection, state| {
                // They say nothing: anything to read is their end closing.
                match (&**connection).read(&mut [0; 64]) {
                    Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(PostAction::Continue),
                    Ok(n) if n > 0 => Ok(PostAction::Continue),
                    _ => {
                        if let Some(asker) = state.picking.asker.take() {
                            state.loop_handle.remove(asker.watch);
                        }
                        state.picker_gone();
                        Ok(PostAction::Remove)
                    }
                }
            });
        watch.ok()
    }

    /// The picker answers: whoever asked hears (Some: chosen, None:
    /// cancelled, the connection closed with nothing said), if they're
    /// still there.
    fn answer(&mut self, answer: Option<String>) {
        let Some(mut asker) = self.picking.asker.take() else {
            return;
        };
        self.loop_handle.remove(asker.watch);
        if let Some(answer) = answer
            && let Err(e) = writeln!(asker.connection, "{answer}")
        {
            tracing::debug!("whoever asked what to share went: {e}");
        }
    }

    /// The picker is gone: whoever asked hears it was cancelled (unless it
    /// answered), and the window manager that it closed.
    pub fn picker_gone(&mut self) {
        self.answer(None);
        if self.picking.shown.take().is_some() {
            self.queue_redraw();
            // The client under the pointer has it again.
            self.repoint();
        }
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
