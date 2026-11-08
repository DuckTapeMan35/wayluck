//! Changes the window manager asked for, together: what clients are told
//! at once (sizes, states), and what's shown, once they've redrawn for it.

use super::{WindowId, animation::Animation};
use crate::{
    Wayluck,
    render::{Border, rounded::Outline},
    wm::protocol::wayluck_wm_transaction_v1::WayluckWmTransactionV1,
    wm::scenes::SceneId,
};
use smithay::{
    output::Output,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel,
    utils::{Logical, Rectangle, Serial, Size},
    wayland::shell::xdg::ToplevelStateSet,
};
use std::{collections::HashMap, time::Instant};

/// One change to what's shown.
pub enum Change {
    /// Show a window at a rectangle, asking its client for that size.
    Place(SceneId, WindowId, Rectangle<i32, Logical>),
    /// Stop showing a window.
    Hide(WindowId),
    /// What an output shows: a scene, or nothing.
    Show(Output, Option<SceneId>),
    /// The size a scene is laid out for.
    SceneSize(SceneId, Size<i32, Logical>),
    /// Give a window the keyboard (None: no window has it).
    Focus(Option<WindowId>),
    /// Draw a border around a window: its width, its colours (0xAARRGGBB;
    /// top, right, bottom, left) and its radius.
    Border(WindowId, u32, [u32; 4], u32),
    /// Tell a window which of its edges are tiled (the protocol's edges bits).
    Tiled(WindowId, u32),
    /// Tell a window whether it's fullscreen.
    Fullscreen(WindowId, bool),
    /// A scene's outlines are those added after this (none, if none are).
    Outlines(SceneId),
    /// Draw an outline on a scene.
    Outline(SceneId, Outline),
    /// A window these changes place isn't animated (None: none is).
    Instant(Option<WindowId>),
    /// Where a window placed in the same changes is drawn moving from (in
    /// its scene), if it's animated.
    StartFrom(WindowId, Rectangle<i32, Logical>),
}

impl Wayluck {
    /// Apply changes together: if any can't be, none are. Clients are told
    /// at once (sizes, states); what's shown changes when the windows it
    /// resizes have redrawn for it. tx hears applied then.
    pub fn apply(
        &mut self,
        changes: Vec<Change>,
        tx: Option<WayluckWmTransactionV1>,
    ) -> Result<(), &'static str> {
        // Check everything first, so nothing is half done.
        for change in &changes {
            let scene = match change {
                Change::Place(scene, ..)
                | Change::SceneSize(scene, _)
                | Change::Outlines(scene)
                | Change::Outline(scene, _)
                | Change::Show(_, Some(scene)) => Some(*scene),
                _ => None,
            };
            if scene.is_some_and(|s| !self.scenes.exists(s)) {
                return Err("a scene in it is gone");
            }
            let id = match change {
                Change::Place(_, id, _)
                | Change::Hide(id)
                | Change::Focus(Some(id))
                | Change::Border(id, ..)
                | Change::Fullscreen(id, _)
                | Change::StartFrom(id, _)
                | Change::Instant(Some(id))
                | Change::Tiled(id, _) => *id,
                Change::Focus(None)
                | Change::Show(..)
                | Change::SceneSize(..)
                | Change::Instant(None)
                | Change::Outline(..)
                | Change::Outlines(_) => continue,
            };
            if self.windows.get(id).is_none() {
                return Err("a window in it is gone");
            }
            if let Change::Place(_, _, rect) | Change::StartFrom(_, rect) = change
                && (rect.size.w <= 0 || rect.size.h <= 0)
            {
                return Err("a size isn't positive");
            }
            if let Change::StartFrom(id, _) = *change
                && !changes
                    .iter()
                    .any(|c| matches!(c, Change::Place(_, p, _) if *p == id))
            {
                return Err("a window starting from somewhere isn't placed");
            }
        }
        // One at a time: one still waiting for its clients is shown first.
        self.show_pending();

        // What clients are told.
        let mut resized = Vec::new();
        for change in &changes {
            match *change {
                Change::Place(_, id, rect) => {
                    let window = self.windows.get(id).unwrap();
                    if let Some(toplevel) = window.toplevel() {
                        let before = toplevel.with_pending_state(|s| s.size.replace(rect.size));
                        // A window not configured yet has nothing on screen to keep.
                        if before != Some(rect.size) && toplevel.is_initial_configure_sent() {
                            resized.push(id);
                        }
                    } else if let Some(x11) = window.x11_surface() {
                        // Told at once, and not waited for: where it goes, it
                        // hears when it's shown there (sync_space).
                        let rect = Rectangle::new(x11.last_configure().loc, rect.size);
                        if let Err(e) = x11.configure(rect) {
                            tracing::debug!("X: can't resize window {}: {e}", x11.window_id());
                        }
                    }
                    self.windows.decided.insert(id);
                }
                Change::Hide(id) => {
                    self.windows.decided.insert(id);
                }
                // X has no tiled state to tell.
                Change::Tiled(id, edges) => {
                    if let Some(toplevel) = self.windows.get(id).unwrap().toplevel() {
                        toplevel.with_pending_state(|s| set_tiled(&mut s.states, edges));
                    }
                }
                Change::Fullscreen(id, on) => {
                    let window = self.windows.get(id).unwrap();
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.with_pending_state(|s| {
                            if on {
                                s.states.set(xdg_toplevel::State::Fullscreen);
                            } else {
                                s.states.unset(xdg_toplevel::State::Fullscreen);
                            }
                        });
                    } else if let Some(x11) = window.x11_surface()
                        && let Err(e) = x11.set_fullscreen(on)
                    {
                        tracing::debug!("X: can't tell window {}: {e}", x11.window_id());
                    }
                }
                Change::Focus(_)
                | Change::Outlines(_)
                | Change::Outline(..)
                | Change::Border(..)
                | Change::Show(..)
                | Change::Instant(_)
                | Change::StartFrom(..)
                | Change::SceneSize(..) => {}
            }
        }
        let serials = self.send_configures();

        // What's shown: now, or when the resized windows have redrawn.
        let waiting: HashMap<_, _> = resized
            .into_iter()
            .filter_map(|id| serials.get(&id).map(|s| (id, *s)))
            .collect();
        if waiting.is_empty() {
            self.show(changes);
            if let Some(tx) = tx {
                tx.applied();
            }
        } else {
            self.wait_for(waiting, changes, tx);
        }
        Ok(())
    }

    /// What changed for each client (size, states), in one configure. A
    /// window that hasn't committed yet gets it as its first configure,
    /// when it does (handle_commit); one that has, and was waiting for the
    /// window manager, gets its first one now. The serials sent, by window.
    fn send_configures(&mut self) -> HashMap<WindowId, Serial> {
        let mut sent = HashMap::new();
        for (id, window) in &self.windows.by_id {
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            let serial = if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure()
            } else if self.windows.waiting.contains(id) && self.windows.decided.contains(id) {
                Some(toplevel.send_configure())
            } else {
                None
            };
            if let Some(serial) = serial {
                sent.insert(*id, serial);
            }
        }
        let decided = &self.windows.decided;
        self.windows.waiting.retain(|id| !decided.contains(id));
        sent
    }

    /// The part of a transaction that's seen: which scene windows are in,
    /// what outputs show, focus and borders. A window closed meanwhile is
    /// skipped.
    pub fn show(&mut self, changes: Vec<Change>) {
        // Where things are first: focus and borders then apply to windows
        // where they are now.
        let now = Instant::now();
        let instant = |id: WindowId| {
            let instant =
                |c: &Change| matches!(*c, Change::Instant(w) if w.is_none_or(|w| w == id));
            changes.iter().any(instant)
        };
        let starts: HashMap<_, _> = (changes.iter())
            .filter_map(|c| match *c {
                Change::StartFrom(id, from) => Some((id, from)),
                _ => None,
            })
            .collect();
        for change in &changes {
            match *change {
                Change::Place(scene, id, rect) => {
                    if self.windows.get(id).is_some() {
                        // Placed again where it's going: it keeps going.
                        let going = (self.windows.animations.get(&id))
                            .is_some_and(|a| a.to == rect && a.at(now).is_some());
                        let same_scene =
                            matches!(self.windows.placed.get(&id), Some((s, _)) if *s == scene);
                        if going && same_scene && !starts.contains_key(&id) && !instant(id) {
                            continue;
                        }
                        let from = starts.get(&id).copied().or(self.drawn_in(id, scene, now));
                        self.scenes.place(scene, id, rect.loc);
                        self.windows.placed.insert(id, (scene, rect));
                        self.windows.animations.remove(&id);
                        let setting = self.windows.animation_for(id).filter(|_| !instant(id));
                        if let (Some(from), Some(setting)) = (from, setting)
                            && from != rect
                        {
                            let animation = Animation {
                                from,
                                to: rect,
                                start: now,
                                duration: setting.duration,
                                curve: setting.curve,
                                ids: Default::default(),
                            };
                            self.windows.animations.insert(id, animation);
                        }
                    }
                }
                Change::Hide(id) => {
                    self.scenes.take_out(id);
                    self.windows.placed.remove(&id);
                    self.windows.animations.remove(&id);
                }
                Change::Show(ref output, scene) => self.scenes.show(output, scene),
                Change::SceneSize(scene, size) => self.scenes.set_size(scene, size),
                _ => {}
            }
        }
        self.sync_space();
        for change in changes {
            match change {
                Change::Outlines(scene) => self.scenes.clear_outlines(scene),
                Change::Outline(scene, outline) => self.scenes.add_outline(scene, outline),
                // The window manager's own windows don't take the keyboard.
                Change::Focus(id) => {
                    self.focus(id.filter(|id| {
                        self.windows.get(*id).is_some() && !self.windows.is_owned(*id)
                    }))
                }
                Change::Border(id, width, colors, radius) => {
                    if self.windows.get(id).is_some() {
                        self.windows
                            .borders
                            .entry(id)
                            .and_modify(|b| b.set(width, colors, radius))
                            .or_insert_with(|| Border::new(width, colors, radius));
                    }
                }
                Change::Fullscreen(id, on) => {
                    if !on {
                        self.windows.fullscreen.remove(&id);
                    } else if self.windows.get(id).is_some() {
                        self.windows.fullscreen.insert(id);
                    }
                }
                Change::Place(..)
                | Change::Hide(_)
                | Change::Show(..)
                | Change::SceneSize(..)
                | Change::Tiled(..)
                | Change::Instant(_)
                | Change::StartFrom(..) => {} // told already
            }
        }
    }

    /// Where window id is drawn in scene at now (on its way, or where it
    /// was placed); None if it isn't in that scene.
    fn drawn_in(
        &self,
        id: WindowId,
        scene: SceneId,
        now: Instant,
    ) -> Option<Rectangle<i32, Logical>> {
        let (placed_in, placed_at) = *self.windows.placed.get(&id)?;
        (placed_in == scene).then(|| self.windows.animated_at(id, now).unwrap_or(placed_at))
    }
}

/// The protocol's edges bits as xdg_toplevel's tiled states.
fn set_tiled(states: &mut ToplevelStateSet, edges: u32) {
    for (bit, state) in [
        (1, xdg_toplevel::State::TiledTop),
        (2, xdg_toplevel::State::TiledBottom),
        (4, xdg_toplevel::State::TiledLeft),
        (8, xdg_toplevel::State::TiledRight),
    ] {
        if edges & bit != 0 {
            states.set(state);
        } else {
            states.unset(state);
        }
    }
}
