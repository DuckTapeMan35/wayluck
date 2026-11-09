//! Client windows, by id. The id is what the window manager will know a
//! window by; a surface finds its window's id without searching (it's kept
//! in the surface's own data).

mod activation;
mod animation;
mod apply;
mod atomic;
mod decoration;
mod list;
mod popups;
mod xdg_shell;

use crate::{
    Wayluck,
    capture::image_capture,
    render::{
        Border,
        blur::{self, Mode},
        rounded::ClipIds,
        shadow::Shadow,
    },
    wm::scenes::{self, SceneId},
};
pub use animation::{Curve, Setting};
pub use apply::Change;
pub use atomic::Pending;
pub use list::ToplevelList;
use smithay::{
    desktop::Window,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle},
    wayland::{
        compositor::{add_pre_commit_hook, with_states},
        seat::WaylandFocus,
        shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData},
    },
    xwayland::{X11Surface, xwm::WmWindowType},
};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    time::Instant,
};
pub use xdg_shell::handle_commit;

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
    /// The borders the window manager gave windows.
    borders: HashMap<WindowId, Border>,
    /// The shadows it gave them.
    shadows: HashMap<WindowId, Shadow>,
    /// Where it said to blur behind them (Client, if it didn't).
    blurs: HashMap<WindowId, Mode>,
    /// How strongly, for all of them.
    pub blur: blur::Strength,
    /// Wayland windows that asked to be brought forward with no right to:
    /// they want attention, until they're focused.
    pub asked_attention: HashSet<WindowId>,
    /// What's kept of each one's blur, from frame to frame.
    behind: HashMap<WindowId, blur::Behind>,
    /// Windows the window manager has placed or hidden at least once: their
    /// first configure can carry what it decided.
    decided: HashSet<WindowId>,
    /// Windows that committed for the first time and wait for that.
    waiting: HashSet<WindowId>,
    /// The window the window manager focused last.
    pub focused: Option<WindowId>,
    /// Windows the window manager made fullscreen.
    fullscreen: HashSet<WindowId>,
    /// The window manager's own (own_window): never focused, never listed.
    owned: HashSet<WindowId>,
    /// Windows on their way to where they're placed.
    animations: HashMap<WindowId, animation::Animation>,
    /// Where each window was placed last, in which scene: where it's
    /// drawn, unless it's on its way there.
    placed: HashMap<WindowId, (SceneId, Rectangle<i32, Logical>)>,
    /// How windows move, as the window manager set it for all of them
    /// (None: they don't), and for some on their own.
    animation: Option<Setting>,
    animation_of: HashMap<WindowId, Setting>,
    next: u32,
}

impl Windows {
    /// Keep window under a new id, and tag its surface with it (an X
    /// window has one by then: surface_associated).
    pub fn insert(&mut self, window: Window) -> WindowId {
        self.next += 1;
        let id = WindowId(self.next);
        if let Some(surface) = window.wl_surface() {
            with_states(&surface, |s| s.data_map.insert_if_missing(|| id));
        }
        self.by_id.insert(id, window);
        id
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

    pub fn remove(&mut self, id: WindowId) -> Option<Window> {
        if self.focused == Some(id) {
            self.focused = None;
        }
        self.fullscreen.remove(&id);
        self.owned.remove(&id);
        self.borders.remove(&id);
        self.shadows.remove(&id);
        self.blurs.remove(&id);
        self.asked_attention.remove(&id);
        self.behind.remove(&id);
        self.decided.remove(&id);
        self.waiting.remove(&id);
        self.animations.remove(&id);
        self.placed.remove(&id);
        self.animation_of.remove(&id);
        self.by_id.remove(&id)
    }

    /// Where window id is drawn in its scene at now, while it's on its way
    /// to where it's placed; None if it isn't (it's drawn where it is).
    pub fn animated_at(&self, id: WindowId, now: Instant) -> Option<Rectangle<i32, Logical>> {
        self.animations.get(&id)?.at(now)
    }

    /// How windows move: all of them, or (with id) one, whatever all of
    /// them do (None: as all of them do again).
    pub fn set_animation(&mut self, id: Option<WindowId>, setting: Option<Setting>) {
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
        (!setting.duration.is_zero()).then_some(setting)
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
    /// A client's new window: kept, not shown until a window manager places
    /// it (which hears of it at its first commit).
    /// With no window manager, shown a step down and right of the last one.
    pub fn add_window(&mut self, window: Window) -> WindowId {
        let id = self.windows.insert(window.clone());
        // Its redraws may be held back for a transaction.
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
