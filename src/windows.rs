//! Client windows, by id. The id is what the window manager will know a
//! window by; a surface finds its window's id without searching (it's kept
//! in the surface's own data).

use crate::{
    Wayluck, image_capture, layers,
    protocol::wayluck_wm_transaction_v1::WayluckWmTransactionV1,
    render::Border,
    scenes::{self, SceneId},
};
use smithay::{
    desktop::Window,
    output::Output,
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Rectangle, SERIAL_COUNTER, Serial, Size},
    wayland::{
        compositor::{add_pre_commit_hook, with_states},
        shell::xdg::{ToplevelStateSet, ToplevelSurface, XdgToplevelSurfaceData},
    },
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

/// A window's id: stays the same for its life, never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(u32);

/// What the window manager is told it's called (wayluck_wm_manager_v1.window).
impl From<WindowId> for u32 {
    fn from(id: WindowId) -> u32 {
        id.0
    }
}

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Default)]
pub struct Windows {
    by_id: HashMap<WindowId, Window>,
    /// The borders the window manager gave windows.
    borders: HashMap<WindowId, Border>,
    /// Windows the window manager has placed or hidden at least once: their
    /// first configure can carry what it decided.
    decided: HashSet<WindowId>,
    /// Windows that committed for the first time and wait for that.
    waiting: HashSet<WindowId>,
    /// The window the window manager focused last.
    focused: Option<WindowId>,
    /// Windows the window manager made fullscreen.
    fullscreen: HashSet<WindowId>,
    next: u32,
}

impl Windows {
    /// Keep window under a new id, and tag its surface with it.
    pub fn insert(&mut self, window: Window) -> WindowId {
        self.next += 1;
        let id = WindowId(self.next);
        if let Some(toplevel) = window.toplevel() {
            with_states(toplevel.wl_surface(), |s| {
                s.data_map.insert_if_missing(|| id)
            });
        }
        self.by_id.insert(id, window);
        id
    }

    pub fn remove(&mut self, id: WindowId) -> Option<Window> {
        if self.focused == Some(id) {
            self.focused = None;
        }
        self.fullscreen.remove(&id);
        self.borders.remove(&id);
        self.decided.remove(&id);
        self.waiting.remove(&id);
        self.by_id.remove(&id)
    }

    pub fn get(&self, id: WindowId) -> Option<&Window> {
        self.by_id.get(&id)
    }

    /// Has the window manager placed (or hidden) it yet?
    pub fn decided(&self, id: WindowId) -> bool {
        self.decided.contains(&id)
    }

    /// It committed for the first time: its first configure waits for the
    /// window manager.
    pub fn wait(&mut self, id: WindowId) {
        self.waiting.insert(id);
    }

    pub fn border_mut(&mut self, id: WindowId) -> Option<&mut Border> {
        self.borders.get_mut(&id)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// The window a toplevel's surface belongs to; None for other surfaces
    /// (subsurfaces, popups, cursors).
    pub fn id_of(surface: &WlSurface) -> Option<WindowId> {
        with_states(surface, |s| s.data_map.get::<WindowId>().copied())
    }

    pub fn ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.by_id.keys().copied()
    }

    /// Did the window manager make it fullscreen?
    pub fn is_fullscreen(&self, id: WindowId) -> bool {
        self.fullscreen.contains(&id)
    }
}

/// A window's title and app id, as far as its client has said.
pub type Names = (Option<String>, Option<String>);

/// What a client called its window.
pub fn names(toplevel: &ToplevelSurface) -> Names {
    with_states(toplevel.wl_surface(), |states| {
        let data = states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap();
        (data.title.clone(), data.app_id.clone())
    })
}

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
}

impl Wayluck {
    /// A client's new window: kept, not shown until a window manager places
    /// it (which hears of it at its first commit, handlers/xdg_shell.rs).
    /// With no window manager, shown a step down and right of the last one.
    pub fn add_window(&mut self, window: Window) -> WindowId {
        let id = self.windows.insert(window.clone());
        // Its redraws may be held back for a transaction (atomic.rs).
        if let Some(toplevel) = window.toplevel() {
            add_pre_commit_hook::<Self, _>(toplevel.wl_surface(), |state, _, surface| {
                state.hold_back(surface)
            });
        }
        if self.wm.manager.is_none() {
            let at = scenes::cascade(self.windows.len() - 1);
            self.scenes.place(self.own_scene, id, at);
            self.sync_space();
        }
        id
    }

    /// Apply changes together: if any can't be, none are. Clients are told
    /// at once (sizes, states); what's shown changes when the windows it
    /// resizes have redrawn for it (atomic.rs). tx hears applied then.
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
                | Change::Tiled(id, _) => *id,
                Change::Focus(None) | Change::Show(..) | Change::SceneSize(..) => continue,
            };
            if self.windows.get(id).is_none() {
                return Err("a window in it is gone");
            }
            if let Change::Place(_, _, rect) = change
                && (rect.size.w <= 0 || rect.size.h <= 0)
            {
                return Err("a size isn't positive");
            }
        }
        // One at a time: one still waiting for its clients is shown first.
        self.show_pending();

        // What clients are told.
        let mut resized = Vec::new();
        for change in &changes {
            match *change {
                Change::Place(_, id, rect) => {
                    let toplevel = self.windows.get(id).unwrap().toplevel().unwrap();
                    let before = toplevel.with_pending_state(|s| s.size.replace(rect.size));
                    // A window not configured yet has nothing on screen to keep.
                    if before != Some(rect.size) && toplevel.is_initial_configure_sent() {
                        resized.push(id);
                    }
                    self.windows.decided.insert(id);
                }
                Change::Hide(id) => {
                    self.windows.decided.insert(id);
                }
                Change::Tiled(id, edges) => {
                    let toplevel = self.windows.get(id).unwrap().toplevel().unwrap();
                    toplevel.with_pending_state(|s| set_tiled(&mut s.states, edges));
                }
                Change::Fullscreen(id, on) => {
                    let toplevel = self.windows.get(id).unwrap().toplevel().unwrap();
                    toplevel.with_pending_state(|s| {
                        if on {
                            s.states.set(xdg_toplevel::State::Fullscreen);
                        } else {
                            s.states.unset(xdg_toplevel::State::Fullscreen);
                        }
                    });
                }
                Change::Focus(_)
                | Change::Border(..)
                | Change::Show(..)
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
        for change in &changes {
            match *change {
                Change::Place(scene, id, rect) => {
                    if self.windows.get(id).is_some() {
                        self.scenes.place(scene, id, rect.loc);
                    }
                }
                Change::Hide(id) => self.scenes.take_out(id),
                Change::Show(ref output, scene) => self.scenes.show(output, scene),
                Change::SceneSize(scene, size) => self.scenes.set_size(scene, size),
                _ => {}
            }
        }
        self.sync_space();
        for change in changes {
            match change {
                Change::Place(..) | Change::Hide(_) | Change::Show(..) | Change::SceneSize(..) => {}
                Change::Focus(id) => self.focus(id.filter(|id| self.windows.get(*id).is_some())),
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
                Change::Tiled(..) => {} // told already
            }
        }
    }

    /// Give id keyboard focus (None: no window has it). Where it stacks is
    /// up to whoever places it.
    pub fn focus(&mut self, id: Option<WindowId>) {
        let window = id.and_then(|id| self.windows.get(id)).cloned();
        // Activated (drawn as focused) for the focused one only.
        for w in self.space.elements() {
            w.set_activated(Some(w) == window.as_ref());
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        }
        self.windows.focused = window.and(id);
        // Focus moved on purpose: a bar or launcher that took the keyboard
        // when clicked lets go (one that asks for it exclusively doesn't).
        if self
            .layer_focus
            .as_ref()
            .is_some_and(|l| !layers::exclusive(l))
        {
            self.layer_focus = None;
        }
        self.give_keyboard();
    }

    /// The keyboard to whoever should have it: a layer surface holding it, else the focused window.
    pub fn give_keyboard(&mut self) {
        let surface = match &self.layer_focus {
            Some(layer) => Some(layer.wl_surface().clone()),
            None => self
                .windows
                .focused
                .and_then(|id| self.windows.get(id))
                .and_then(|w| w.toplevel())
                .map(|t| t.wl_surface().clone()),
        };
        let keyboard = self.seat.get_keyboard().unwrap();
        if keyboard.current_focus() != surface {
            keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        }
    }

    /// A window's client closed it, or went away.
    pub fn remove_window(&mut self, id: WindowId) {
        self.scenes.take_out(id);
        self.unlist_window(id);
        self.stop_capture_sessions(&image_capture::Source::Window(id));
        if let Some(window) = self.windows.remove(id) {
            self.space.unmap_elem(&window);
            self.queue_redraw();
        }
        self.stop_waiting_for(id);
        self.wm_window_closed(id);
    }
}

impl Wayluck {
    /// No window manager to wait for any more: windows still waiting for
    /// their first configure get it, empty (the client picks its size).
    pub fn configure_waiting(&mut self) {
        for id in std::mem::take(&mut self.windows.waiting) {
            if let Some(toplevel) = self.windows.get(id).and_then(|w| w.toplevel()) {
                toplevel.send_configure();
            }
        }
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
