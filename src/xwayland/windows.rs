//! X windows wayluck shows: windows like any other once Xwayland has given
//! them a surface. The window manager places them, and hears what their
//! clients ask (names, parent, size limits, fullscreen).

use crate::Wayluck;
use smithay::{
    desktop::Window,
    xwayland::{X11Surface, xwm::WmWindowProperty},
};
use std::sync::Mutex;

/// An X window asked to be fullscreen (or not) before it was a window:
/// the window manager hears so with it.
#[derive(Default)]
struct AskedFullscreen(Mutex<bool>);

/// Xwayland made the surface a shown X window is drawn on: from now on
/// it's a window like the others, which the window manager hears of
/// (without one, it cascades). Not before: clicks and keys reach it
/// through its surface.
pub(super) fn associated(state: &mut Wayluck, window: &X11Surface) {
    let managed = !window.is_override_redirect() && window.is_mapped();
    if !managed || state.windows.find_x11(window).is_some() {
        return;
    }
    // A dock is no window, but placed where it says, like a bar.
    if super::docks::is_dock(window) {
        return super::docks::mapped(state, window.clone());
    }
    let id = state.add_window(Window::new_x11_window(window.clone()));
    tracing::info!(
        "X: window {} is {id} ({:?})",
        window.window_id(),
        window.title()
    );
    // Clients listing windows hear of it.
    state.list_window(id);
    // The window manager too, with everything its client set up first
    // (names, parent, size limits; fullscreen, if it asked already).
    state.wm_window_added(id);
    // Asked by message since it was mapped, or else set in _NET_WM_STATE
    // before (as EWMH allows; smithay reads it at the map request).
    let asked = (window.user_data().get::<AskedFullscreen>()).map(|a| *a.0.lock().unwrap());
    if let Some(on) = asked.or(window.is_fullscreen().then_some(true)) {
        state.wm_fullscreen_request(id, on);
    }
}

/// An X client wants its window shown: mapped, Xwayland makes it a
/// surface, and then it's a window.
pub(super) fn map_request(window: &X11Surface) {
    if let Err(e) = window.set_mapped(true) {
        tracing::warn!("X: can't show window {}: {e}", window.window_id());
    }
}

/// Hidden, an X window is closed: shown again, it's new.
pub(super) fn unmapped(state: &mut Wayluck, window: &X11Surface) {
    if let Some(id) = state.windows.find_x11(window) {
        tracing::info!("X: window {} ({id}) hidden", window.window_id());
        state.remove_window(id);
    }
    if !window.is_override_redirect() {
        let _ = window.set_mapped(false);
    }
}

pub(super) fn destroyed(state: &mut Wayluck, window: &X11Surface) {
    if let Some(id) = state.windows.find_x11(window) {
        tracing::info!("X: window {} ({id}) gone", window.window_id());
        state.remove_window(id);
    }
}

/// An X client asks for its window to be somewhere (x, y), or some size
/// (w, h). Its size it may pick (as a Wayland client does), until the
/// window manager has placed it; where it is, only until it's shown. X
/// needs an answer either way.
pub(super) fn configure_request(
    state: &mut Wayluck,
    window: &X11Surface,
    (x, y): (Option<i32>, Option<i32>),
    (w, h): (Option<u32>, Option<u32>),
) {
    tracing::debug!(
        "X: window {} asks for {x:?},{y:?} {w:?}x{h:?}",
        window.window_id()
    );
    let id = state.windows.find_x11(window);
    let placed = id.is_some_and(|id| state.wm.manager.is_some() && state.windows.decided(id));
    let mut rect = window.last_configure();
    if !placed {
        rect.size.w = w.map_or(rect.size.w, |w| w as i32);
        rect.size.h = h.map_or(rect.size.h, |h| h as i32);
    }
    if id.is_none() {
        rect.loc.x = x.unwrap_or(rect.loc.x);
        rect.loc.y = y.unwrap_or(rect.loc.y);
    }
    if let Err(e) = window.configure(rect) {
        tracing::debug!("X: can't configure window {}: {e}", window.window_id());
    }
}

/// What a client called its window changed, or its parent, or its size
/// limits: the window manager hears (and clients listing windows).
pub(super) fn property_changed(
    state: &mut Wayluck,
    window: &X11Surface,
    property: WmWindowProperty,
) {
    let Some(id) = state.windows.find_x11(window) else {
        return;
    };
    match property {
        WmWindowProperty::Title => {
            state.wm_window_title(id);
            state.list_names_changed(id);
        }
        WmWindowProperty::Class => {
            state.wm_window_app_id(id);
            state.list_names_changed(id);
        }
        WmWindowProperty::TransientFor => state.wm_window_parent(id),
        // Urgency is in WM_HINTS.
        WmWindowProperty::Hints => state.wm_window_attention(id),
        WmWindowProperty::NormalHints => {
            state.wm_size_hints(id);
            state.wm_window_hints(id);
        }
        WmWindowProperty::WindowType => state.wm_window_kind(id),
        _ => {}
    }
}

/// An X client says its window wants the user's attention (on), or no
/// longer.
pub(super) fn demands_attention(state: &mut Wayluck, window: &X11Surface, on: bool) {
    if let Err(e) = window.set_demands_attention(on) {
        tracing::debug!("X: can't tell window {}: {e}", window.window_id());
    }
    if let Some(id) = state.windows.find_x11(window) {
        state.wm_window_attention(id);
    }
}

/// An X client, or a pager, asked for a window to be the active one: the
/// window manager hears so, and decides.
pub(super) fn activate_request(state: &mut Wayluck, window: &X11Surface) {
    if let Some(id) = state.windows.find_x11(window) {
        state.wm_activate_request(id);
    }
}

/// An X client asked for its window to be fullscreen (on), or not. That's
/// the window manager's to give: it hears, now or (before it's a window)
/// with it, and its answer comes in a transaction. Without one, nothing
/// changes.
pub(super) fn fullscreen_request(state: &mut Wayluck, window: &X11Surface, on: bool) {
    match state.windows.find_x11(window) {
        Some(id) => {
            state.wm_fullscreen_request(id, on);
        }
        None => {
            let asked = window.user_data();
            asked.insert_if_missing_threadsafe(AskedFullscreen::default);
            *asked.get::<AskedFullscreen>().unwrap().0.lock().unwrap() = on;
        }
    }
}
