//! Client windows, by id. The id is what the window manager will know a
//! window by; a surface finds its window's id without searching (it's kept
//! in the surface's own data).

use crate::Wayluck;
use smithay::{
    desktop::Window,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData},
    },
};
use std::{collections::HashMap, fmt};

/// A window's id: stays the same for its life, never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(u64);

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Default)]
pub struct Windows {
    by_id: HashMap<WindowId, Window>,
    next: u64,
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
        self.by_id.remove(&id)
    }

    pub fn get(&self, id: WindowId) -> Option<&Window> {
        self.by_id.get(&id)
    }

    pub fn ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.by_id.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// The window a toplevel's surface belongs to; None for other surfaces
    /// (subsurfaces, popups, cursors).
    pub fn id_of(surface: &WlSurface) -> Option<WindowId> {
        with_states(surface, |s| s.data_map.get::<WindowId>().copied())
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
    Place(WindowId, Rectangle<i32, Logical>),
    /// Stop showing a window.
    Hide(WindowId),
    /// Give a window the keyboard (None: no window has it).
    Focus(Option<WindowId>),
}

impl Wayluck {
    /// A client's new window: kept, not shown until a window manager places
    /// it. With no window manager, shown a step down and right of the last one.
    pub fn add_window(&mut self, window: Window) -> WindowId {
        let step = 40 * self.windows.len() as i32;
        let id = self.windows.insert(window.clone());
        if self.wm.manager.is_none() {
            self.space.map_element(window, (step, step), true);
        }
        self.wm_window_added(id);
        id
    }

    /// Apply changes together: if any can't be, none are.
    pub fn apply(&mut self, changes: Vec<Change>) -> Result<(), &'static str> {
        // Check everything first, so nothing is half done.
        for change in &changes {
            let id = match change {
                Change::Place(id, _) | Change::Hide(id) | Change::Focus(Some(id)) => *id,
                Change::Focus(None) => continue,
            };
            if self.windows.get(id).is_none() {
                return Err("a window in it is gone");
            }
            if let Change::Place(_, rect) = change
                && (rect.size.w <= 0 || rect.size.h <= 0)
            {
                return Err("a size isn't positive");
            }
        }
        for change in changes {
            match change {
                Change::Place(id, rect) => {
                    let window = self.windows.get(id).unwrap().clone();
                    let toplevel = window.toplevel().unwrap();
                    toplevel.with_pending_state(|s| s.size = Some(rect.size));
                    // Before its first commit a client can't be configured;
                    // its first configure (handle_commit) carries the size.
                    if toplevel.is_initial_configure_sent() {
                        toplevel.send_pending_configure();
                    }
                    self.space.map_element(window, rect.loc, false);
                }
                Change::Hide(id) => {
                    let window = self.windows.get(id).unwrap().clone();
                    self.space.unmap_elem(&window);
                }
                Change::Focus(id) => self.focus(id),
            }
        }
        Ok(())
    }

    /// Give id keyboard focus (None: no window has it), and raise it.
    pub fn focus(&mut self, id: Option<WindowId>) {
        let window = id.and_then(|id| self.windows.get(id)).cloned();
        if let Some(w) = &window {
            self.space.raise_element(w, true);
        }
        // Activated (drawn as focused) for the focused one only.
        for w in self.space.elements() {
            w.set_activated(Some(w) == window.as_ref());
            if let Some(t) = w.toplevel() {
                t.send_pending_configure();
            }
        }
        let surface = window.and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
    }

    /// A window's client closed it, or went away.
    pub fn remove_window(&mut self, id: WindowId) {
        if let Some(window) = self.windows.remove(id) {
            self.space.unmap_elem(&window);
        }
        self.wm_window_closed(id);
    }
}
