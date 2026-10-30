//! Scenes: windows laid out together (a workspace, to a window manager that
//! has them), at positions relative to the scene's top left corner, stacked
//! in the order they were placed. An output shows one scene, or none, drawn
//! from its own top left corner; several outputs may show the same one.
//!
//! The window manager makes them. Without one, wayluck has its own,
//! shown on the first output: windows cascade there.
//!
//! The space (smithay's, for input, popups and frame callbacks) is what's on
//! the monitors, and follows the scenes: sync_space maps each window of a
//! shown scene at its monitor's corner plus where it is in the scene.

use crate::{Wayluck, windows::WindowId};
use smithay::{
    output::Output,
    utils::{Logical, Point, Size},
};
use std::collections::{HashMap, HashSet};

/// A scene's id: what the window manager's object for it carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SceneId(u32);

impl SceneId {
    /// As a number, for clients that name scenes (the picker's workspaces).
    pub fn number(self) -> u32 {
        self.0
    }
}

/// One scene: its windows, bottom first, and where each is.
#[derive(Default)]
pub struct Scene {
    /// The size it's laid out for (0×0 until it's given one).
    pub size: Size<i32, Logical>,
    stack: Vec<(WindowId, Point<i32, Logical>)>,
}

impl Scene {
    /// Its windows and where they are in it, bottom first.
    pub fn windows(&self) -> impl DoubleEndedIterator<Item = (WindowId, Point<i32, Logical>)> + '_ {
        self.stack.iter().copied()
    }
}

#[derive(Default)]
pub struct Scenes {
    by_id: HashMap<SceneId, Scene>,
    /// The scene each window is in.
    of: HashMap<WindowId, SceneId>,
    /// What each output shows.
    shown: Vec<(Output, SceneId)>,
    next: u32,
}

impl Scenes {
    pub fn add(&mut self) -> SceneId {
        self.next += 1;
        let id = SceneId(self.next);
        self.by_id.insert(id, Scene::default());
        id
    }

    /// Forget a scene: its windows are in none, and no output shows it.
    pub fn remove(&mut self, id: SceneId) {
        if let Some(scene) = self.by_id.remove(&id) {
            for (window, _) in scene.stack {
                self.of.remove(&window);
            }
        }
        self.shown.retain(|(_, s)| *s != id);
    }

    pub fn get(&self, id: SceneId) -> Option<&Scene> {
        self.by_id.get(&id)
    }

    pub fn exists(&self, id: SceneId) -> bool {
        self.by_id.contains_key(&id)
    }

    /// The scene with this number, if there is one.
    pub fn numbered(&self, number: u32) -> Option<SceneId> {
        Some(SceneId(number)).filter(|id| self.exists(*id))
    }

    /// Put a window in a scene at a place, on top of the others there (out
    /// of the scene it was in, if another).
    pub fn place(&mut self, scene: SceneId, window: WindowId, at: Point<i32, Logical>) {
        self.take_out(window);
        if let Some(s) = self.by_id.get_mut(&scene) {
            s.stack.push((window, at));
            self.of.insert(window, scene);
        }
    }

    /// Take a window out of its scene (hidden, or closed).
    pub fn take_out(&mut self, window: WindowId) {
        if let Some(scene) = self.of.remove(&window)
            && let Some(s) = self.by_id.get_mut(&scene)
        {
            s.stack.retain(|(w, _)| *w != window);
        }
    }

    /// Put a window on top of its scene, where it is.
    pub fn raise(&mut self, window: WindowId) {
        let Some(&scene) = self.of.get(&window) else {
            return;
        };
        let at = (self.by_id.get(&scene))
            .and_then(|s| s.stack.iter().find(|(w, _)| *w == window))
            .map(|(_, at)| *at);
        if let Some(at) = at {
            self.place(scene, window, at);
        }
    }

    pub fn set_size(&mut self, scene: SceneId, size: Size<i32, Logical>) {
        if let Some(s) = self.by_id.get_mut(&scene) {
            s.size = size;
        }
    }

    /// What an output shows from now on (None: no scene).
    pub fn show(&mut self, output: &Output, scene: Option<SceneId>) {
        self.shown.retain(|(o, _)| o != output);
        if let Some(scene) = scene.filter(|s| self.exists(*s)) {
            self.shown.push((output.clone(), scene));
        }
    }

    /// The scene an output shows, if any.
    pub fn shown_on(&self, output: &Output) -> Option<&Scene> {
        let (_, id) = self.shown.iter().find(|(o, _)| o == output)?;
        self.by_id.get(id)
    }

    /// No output shows anything (a window manager has just come: it says).
    pub fn show_nothing(&mut self) {
        self.shown.clear();
    }
}

impl Wayluck {
    /// The space as the scenes say: every window of a scene shown on a
    /// monitor, at that monitor's corner plus where it is in the scene
    /// (the first monitor's, for a scene shown on several), stacked as in
    /// its scene. Windows in no shown scene leave it.
    pub fn sync_space(&mut self) {
        let mut placed = Vec::new();
        let mut seen = HashSet::new();
        for output in self.space.outputs() {
            let Some((_, id)) = self.scenes.shown.iter().find(|(o, _)| o == output) else {
                continue;
            };
            let (Some(scene), Some(area)) =
                (self.scenes.get(*id), self.space.output_geometry(output))
            else {
                continue;
            };
            if !seen.insert(*id) {
                continue; // drawn there too, but on the space once
            }
            for (window, at) in scene.windows() {
                if let Some(w) = self.windows.get(window) {
                    placed.push((w.clone(), area.loc + at));
                }
            }
        }
        let gone: Vec<_> = (self.space.elements())
            .filter(|w| !placed.iter().any(|(p, _)| p == *w))
            .cloned()
            .collect();
        for window in gone {
            self.space.unmap_elem(&window);
        }
        // Each mapped on top of the last: the order is the stacking.
        for (window, at) in placed {
            self.space.map_element(window, at, false);
        }
        self.queue_redraw();
    }

    /// Without a window manager: wayluck's own scene, on the first output,
    /// with every window where it is now (or cascading, if it isn't shown).
    pub fn show_own_scene(&mut self) {
        let own = self.own_scene;
        let first = self.space.outputs().next().cloned();
        let origin = (first.as_ref())
            .and_then(|o| self.space.output_geometry(o))
            .map_or_else(Point::default, |g| g.loc);
        let ids: Vec<WindowId> = self.windows.ids().collect();
        for (n, id) in ids.into_iter().enumerate() {
            let at = (self.windows.get(id))
                .and_then(|w| self.space.element_location(w))
                .map_or_else(|| cascade(n), |loc| loc - origin);
            self.scenes.place(own, id, at);
        }
        self.scenes.show_nothing();
        if let Some(first) = first {
            self.scenes.show(&first, Some(own));
        }
        self.sync_space();
    }
}

/// Where the n-th window goes with no window manager: a step down and right
/// of the last.
pub fn cascade(n: usize) -> Point<i32, Logical> {
    let step = 40 * n as i32;
    (step, step).into()
}
