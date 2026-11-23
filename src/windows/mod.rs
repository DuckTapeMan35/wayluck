mod activation;
mod animation;
mod apply;
mod atomic;
mod decoration;
mod list;
mod popups;
mod taskbars;
mod xdg_shell;

use crate::{
    Wayluck,
    capture::image_capture,
    entities::{Entity, Id},
    render::{
        Border,
        blur::{self, Mode},
        rounded::ClipIds,
        shadow::Shadow,
    },
    wm::{
        scenes::{self, SceneId},
        workspaces::WorkspaceId,
    },
};
pub use animation::{Curve, Setting, Spring};
pub use apply::Change;
pub use atomic::Pending;
pub use list::ToplevelList;
use smithay::{
    desktop::Window,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, Size},
    wayland::{
        compositor::{add_pre_commit_hook, with_states},
        seat::WaylandFocus,
        shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData},
    },
    xwayland::{X11Surface, xwm::WmWindowType},
};
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};
pub use taskbars::Taskbars;
pub use xdg_shell::handle_commit;

pub type WindowId = Id<Window>;

/// What kind of window one is, whoever its client is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Normal,
    Dialog,
    Utility,
    Splash,
    Notification,
    Menu,
    Toolbar,
}

#[derive(Default)]
pub struct Windows {
    by_id: HashMap<WindowId, Window>,
    borders: HashMap<WindowId, Border>,
    shadows: HashMap<WindowId, Shadow>,
    /// Mode::Client where the window manager set none.
    blurs: HashMap<WindowId, Mode>,
    pub blur: blur::Strength,
    /// Asked to be activated without the right to: until focused.
    pub asked_attention: HashSet<WindowId>,
    behind: HashMap<WindowId, blur::Behind>,
    /// Placed or hidden at least once: the first configure can carry it.
    decided: HashSet<WindowId>,
    /// Committed for the first time, not configured yet.
    waiting: HashSet<WindowId>,
    pub focused: Option<WindowId>,
    fullscreen: HashSet<WindowId>,
    /// The window manager's own: never focused, never listed.
    owned: HashSet<WindowId>,
    animations: HashMap<WindowId, animation::Animation>,
    placed: HashMap<WindowId, (SceneId, Rectangle<i32, Logical>)>,
    /// Resized from the left, top: drawn from the right, bottom meanwhile.
    from_end: HashMap<WindowId, (bool, bool)>,
    animation: Option<Setting>,
    animation_of: HashMap<WindowId, Setting>,
    /// None: on every workspace.
    workspaces: HashMap<WindowId, Option<WorkspaceId>>,
}

impl Windows {
    /// An X window has its surface by then (surface_associated).
    fn insert(&mut self, id: WindowId, window: Window) {
        if let Some(surface) = window.wl_surface() {
            with_states(&surface, |s| s.data_map.insert_if_missing(|| id));
        }
        self.by_id.insert(id, window);
    }

    /// The window id belongs to, for example: a dialog's main window.
    /// An X window's is the one it's WM_TRANSIENT_FOR.
    pub fn parent(&self, id: WindowId) -> Option<WindowId> {
        let window = self.get(id)?;
        if let Some(x11) = window.x11_surface() {
            let parent = x11.is_transient_for()?;
            return (self.by_id.iter())
                .find(|(_, w)| w.x11_surface().is_some_and(|p| p.window_id() == parent))
                .map(|(id, _)| *id);
        }
        Self::id_of(&window.toplevel()?.parent()?)
    }

    /// Whether it wants the user's attention: an X window urgent or
    /// demanding it, a Wayland window that asked to be brought forward.
    pub fn wants_attention(&self, id: WindowId) -> bool {
        let x11 = self.get(id).and_then(|w| w.x11_surface().cloned());
        let x_wants =
            x11.is_some_and(|x| x.demands_attention() || x.hints().is_some_and(|h| h.urgent));
        self.asked_attention.contains(&id) || x_wants
    }

    /// What kind of window it is (X windows' types and Wayland windows'
    /// parents in the same terms).
    pub fn kind(&self, id: WindowId) -> Kind {
        let has_parent = self.parent(id).is_some();
        let x11_type = (self.get(id))
            .and_then(|w| w.x11_surface().cloned())
            .and_then(|x11| x11.window_type());
        match x11_type {
            Some(WmWindowType::Dialog) => Kind::Dialog,
            Some(WmWindowType::Utility) => Kind::Utility,
            Some(WmWindowType::Splash) => Kind::Splash,
            Some(WmWindowType::Notification) => Kind::Notification,
            Some(WmWindowType::Toolbar) => Kind::Toolbar,
            Some(
                WmWindowType::Menu
                | WmWindowType::PopupMenu
                | WmWindowType::DropdownMenu
                | WmWindowType::Combo
                | WmWindowType::Tooltip
                | WmWindowType::Dnd,
            ) => Kind::Menu,
            Some(WmWindowType::Normal | WmWindowType::Desktop | WmWindowType::Dock) | None
                if has_parent =>
            {
                Kind::Dialog
            }
            Some(WmWindowType::Normal | WmWindowType::Desktop | WmWindowType::Dock) | None => {
                Kind::Normal
            }
        }
    }

    /// The window an X window is, if it's shown (mapped).
    pub fn find_x11(&self, x11: &X11Surface) -> Option<WindowId> {
        (self.by_id.iter())
            .find(|(_, w)| w.x11_surface() == Some(x11))
            .map(|(id, _)| *id)
    }

    /// The window X window is (by its id), if it's shown.
    pub fn find_x11_window(&self, window: u32) -> Option<WindowId> {
        (self.by_id.iter())
            .find(|(_, w)| w.x11_surface().is_some_and(|x| x.window_id() == window))
            .map(|(id, _)| *id)
    }

    fn remove(&mut self, id: WindowId) -> Option<Window> {
        if self.focused == Some(id) {
            self.focused = None;
        }
        self.fullscreen.remove(&id);
        self.owned.remove(&id);
        self.borders.remove(&id);
        self.workspaces.remove(&id);
        self.shadows.remove(&id);
        self.blurs.remove(&id);
        self.asked_attention.remove(&id);
        self.behind.remove(&id);
        self.decided.remove(&id);
        self.waiting.remove(&id);
        self.animations.remove(&id);
        self.placed.remove(&id);
        self.from_end.remove(&id);
        self.animation_of.remove(&id);
        self.by_id.remove(&id)
    }

    /// Window id is placed at rect in scene: which edges stay where they
    /// were, to draw it from while its client catches up. On each axis,
    /// its end if only its start moved; its start if its end did; as it
    /// was if neither did (dragged sideways, its top's still the one).
    fn placed_at(&mut self, id: WindowId, scene: SceneId, rect: Rectangle<i32, Logical>) {
        let before = (self.placed.insert(id, (scene, rect)))
            .filter(|(s, _)| *s == scene)
            .map(|(_, r)| r);
        let was = self.from_end.get(&id).copied().unwrap_or_default();
        let Some(before) = before else {
            self.from_end.remove(&id);
            return;
        };
        let axis = |start: i32, end: i32, new_start: i32, new_end: i32, was: bool| match (
            start != new_start,
            end != new_end,
        ) {
            (true, false) => true,
            (_, true) => false,
            (false, false) => was,
        };
        let x = axis(
            before.loc.x,
            before.loc.x + before.size.w,
            rect.loc.x,
            rect.loc.x + rect.size.w,
            was.0,
        );
        let y = axis(
            before.loc.y,
            before.loc.y + before.size.h,
            rect.loc.y,
            rect.loc.y + rect.size.h,
            was.1,
        );
        self.from_end.insert(id, (x, y));
    }

    /// The size window id is placed at, if the window manager placed it.
    pub fn placed_size(&self, id: WindowId) -> Option<Size<i32, Logical>> {
        self.placed.get(&id).map(|(_, rect)| rect.size)
    }

    /// Where window id, placed at at, is drawn at size (what its client
    /// drew): at at, unless its client hasn't caught up with the size it's
    /// placed at and it was resized from its start: then its end stays put.
    pub fn drawn_from(
        &self,
        id: WindowId,
        at: Point<i32, Logical>,
        size: Size<i32, Logical>,
    ) -> Point<i32, Logical> {
        let (Some((_, placed)), Some(&(x, y))) = (self.placed.get(&id), self.from_end.get(&id))
        else {
            return at;
        };
        let mut at = at;
        if x {
            at.x += placed.size.w - size.w;
        }
        if y {
            at.y += placed.size.h - size.h;
        }
        at
    }

    /// Where window id is drawn in its scene at now, while it's on its way
    /// to where it's placed; None if it isn't (it's drawn where it is).
    pub fn animated_at(&self, id: WindowId, now: Instant) -> Option<Rectangle<i32, Logical>> {
        self.animations.get(&id)?.at(now)
    }

    /// How windows move: all of them, or (with id) one, whatever all of
    /// them do (None: as all of them do again).
    pub fn set_curve(&mut self, id: Option<WindowId>, setting: Option<Setting>) {
        match id {
            None => self.animation = setting,
            Some(id) => match setting {
                Some(setting) => {
                    self.animation_of.insert(id, setting);
                }
                None => {
                    self.animation_of.remove(&id);
                }
            },
        }
    }

    /// No window moves, or is on its way anywhere (the window manager that
    /// said how is gone).
    pub fn forget_animations(&mut self) {
        self.animation = None;
        self.animation_of.clear();
        self.animations.clear();
    }

    /// How window id moves; None: it doesn't.
    fn animation_for(&self, id: WindowId) -> Option<Setting> {
        let setting = (self.animation_of.get(&id))
            .or(self.animation.as_ref())?
            .clone();
        setting.moves().then_some(setting)
    }

    /// The ids window id's surfaces are drawn under, stretched on its way
    /// somewhere.
    pub fn animation_ids(&mut self, id: WindowId) -> Option<&mut ClipIds> {
        self.animations.get_mut(&id).map(|a| &mut a.ids)
    }

    /// Are windows on their way somewhere, or did one just get there?
    /// Those there are forgotten: drawn once more, they're where they're
    /// placed.
    pub fn animations_going(&mut self, now: Instant) -> bool {
        let any = !self.animations.is_empty();
        self.animations.retain(|_, a| a.at(now).is_some());
        any
    }

    /// The windows on their way somewhere, at now.
    pub fn animated(&self, now: Instant) -> impl Iterator<Item = WindowId> + '_ {
        (self.animations.iter())
            .filter(move |(_, a)| a.at(now).is_some())
            .map(|(id, _)| *id)
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

    /// It's the window manager's own (own_window).
    pub fn own(&mut self, id: WindowId) {
        self.owned.insert(id);
    }

    pub fn is_owned(&self, id: WindowId) -> bool {
        self.owned.contains(&id)
    }

    pub fn border(&self, id: WindowId) -> Option<&Border> {
        self.borders.get(&id)
    }

    pub fn border_mut(&mut self, id: WindowId) -> Option<&mut Border> {
        self.borders.get_mut(&id)
    }

    pub fn shadow_mut(&mut self, id: WindowId) -> Option<&mut Shadow> {
        self.shadows.get_mut(&id)
    }

    pub fn blur_mode(&self, id: WindowId) -> Mode {
        self.blurs.get(&id).copied().unwrap_or_default()
    }

    pub fn behind_mut(&mut self, id: WindowId) -> &mut blur::Behind {
        self.behind.entry(id).or_default()
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

    /// Say which workspace a window is on (None: all).
    pub fn set_workspace(&mut self, id: WindowId, workspace: Option<WorkspaceId>) {
        self.workspaces.insert(id, workspace);
    }

    /// The windows said to be on a workspace, and which (None: all).
    pub fn workspaces(&self) -> impl Iterator<Item = (WindowId, Option<WorkspaceId>)> + '_ {
        self.workspaces.iter().map(|(id, w)| (*id, *w))
    }

    /// Did the window manager make it fullscreen?
    pub fn is_fullscreen(&self, id: WindowId) -> bool {
        self.fullscreen.contains(&id)
    }
}

/// A window's title and app id, as far as its client has said.
pub type Names = (Option<String>, Option<String>);

/// What a client called its window: an X window's title, and its class
/// (WM_CLASS's second half) as its app id.
pub fn names(window: &Window) -> Names {
    if let Some(x11) = window.x11_surface() {
        let some = |s: String| Some(s).filter(|s| !s.is_empty());
        return (some(x11.title()), some(x11.class()));
    }
    match window.toplevel() {
        Some(toplevel) => toplevel_names(toplevel),
        None => (None, None),
    }
}

/// What a Wayland client called its window.
pub fn toplevel_names(toplevel: &ToplevelSurface) -> Names {
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

impl Wayluck {
    /// Not shown until a window manager places it, which hears of it at its
    /// first commit; with none, cascaded.
    pub fn add_window(&mut self, window: Window) -> WindowId {
        let id = self.spawn(despawn_window);
        self.windows.insert(id, window.clone());
        if let Some(surface) = window.wl_surface() {
            add_pre_commit_hook::<Self, _>(&surface, |state, _, surface| state.hold_back(surface));
        }
        if self.wm.manager.is_none() {
            let at = scenes::cascade(self.windows.len() - 1);
            self.scenes.place(self.own_scene, id, at);
            self.sync_space();
        }
        id
    }

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

fn despawn_window(state: &mut Wayluck, entity: Entity) {
    let id = WindowId::of_despawned(entity);
    state.scenes.take_out(id);
    state.unlist_window(id);
    state.stop_capture_sessions(&image_capture::Source::Window(id));
    if let Some(window) = state.windows.remove(id) {
        state.space.unmap_elem(&window);
        state.queue_redraw();
    }
    state.stop_waiting_for(id);
    state.wm_window_closed(id);
}
