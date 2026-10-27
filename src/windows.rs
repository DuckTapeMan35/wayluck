//! Client windows, by id. The id is what the window manager will know a
//! window by; a surface finds its window's id without searching (it's kept
//! in the surface's own data).

use std::{collections::HashMap, fmt};

use smithay::{
    desktop::Window, reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::compositor::with_states,
};

use crate::Wayluck;

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

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// The window a toplevel's surface belongs to; None for other surfaces
    /// (subsurfaces, popups, cursors).
    pub fn id_of(surface: &WlSurface) -> Option<WindowId> {
        with_states(surface, |s| s.data_map.get::<WindowId>().copied())
    }
}

impl Wayluck {
    /// A client's new window: kept, and (until a window manager places
    /// windows) shown a step down and right of the last one.
    pub fn add_window(&mut self, window: Window) -> WindowId {
        let step = 40 * self.windows.len() as i32;
        let id = self.windows.insert(window.clone());
        self.space.map_element(window, (step, step), true);
        id
    }

    /// A window's client closed it, or went away.
    pub fn remove_window(&mut self, id: WindowId) {
        if let Some(window) = self.windows.remove(id) {
            self.space.unmap_elem(&window);
        }
    }
}
