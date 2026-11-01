//! What has the keyboard: a Wayland surface, or an X window. An X window
//! needs more than its surface's wl_keyboard.enter: its X client is told
//! too (X input focus, or WM_TAKE_FOCUS), by smithay's X11Surface. Leaving
//! takes X input focus away again, so no X window has it while a Wayland
//! one has the keyboard.

use crate::Wayluck;
use smithay::{
    backend::input::KeyState,
    desktop::PopupKind,
    input::{
        Seat,
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Serial},
    wayland::seat::WaylandFocus,
    xwayland::X11Surface,
};
use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardFocus {
    /// A window's, a bar's, a popup's surface.
    Surface(WlSurface),
    /// An X window, and the surface Xwayland draws it on (KeyboardFocus::x11).
    /// Boxed: an X11Surface is big, and most focus is a surface's.
    X11(Box<X11Surface>, WlSurface),
}

impl KeyboardFocus {
    /// An X window can have the keyboard once Xwayland gave it a surface.
    pub fn x11(window: &X11Surface) -> Option<Self> {
        Some(KeyboardFocus::X11(
            Box::new(window.clone()),
            window.wl_surface()?,
        ))
    }

    /// The surface keys go to.
    pub fn surface(&self) -> &WlSurface {
        match self {
            KeyboardFocus::Surface(surface) | KeyboardFocus::X11(_, surface) => surface,
        }
    }
}

impl From<WlSurface> for KeyboardFocus {
    fn from(surface: WlSurface) -> Self {
        KeyboardFocus::Surface(surface)
    }
}

/// A menu that grabs the keyboard.
impl From<PopupKind> for KeyboardFocus {
    fn from(popup: PopupKind) -> Self {
        KeyboardFocus::Surface(popup.wl_surface().clone())
    }
}

/// The pointer goes where the keyboard is grabbed to.
impl From<KeyboardFocus> for WlSurface {
    fn from(focus: KeyboardFocus) -> Self {
        focus.surface().clone()
    }
}

impl IsAlive for KeyboardFocus {
    fn alive(&self) -> bool {
        match self {
            KeyboardFocus::Surface(surface) => surface.alive(),
            KeyboardFocus::X11(x11, surface) => x11.alive() && surface.alive(),
        }
    }
}

impl WaylandFocus for KeyboardFocus {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        Some(Cow::Borrowed(self.surface()))
    }
}

// Each to whichever it is. For an X window, smithay's X11Surface does the X
// side and passes the rest on to its surface.
impl KeyboardTarget<Wayluck> for KeyboardFocus {
    fn enter(
        &self,
        seat: &Seat<Wayluck>,
        data: &mut Wayluck,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        match self {
            KeyboardFocus::Surface(s) => KeyboardTarget::enter(s, seat, data, keys, serial),
            KeyboardFocus::X11(x, _) => KeyboardTarget::enter(&**x, seat, data, keys, serial),
        }
    }

    fn leave(&self, seat: &Seat<Wayluck>, data: &mut Wayluck, serial: Serial) {
        match self {
            KeyboardFocus::Surface(s) => KeyboardTarget::leave(s, seat, data, serial),
            KeyboardFocus::X11(x, _) => KeyboardTarget::leave(&**x, seat, data, serial),
        }
    }

    fn key(
        &self,
        seat: &Seat<Wayluck>,
        data: &mut Wayluck,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            KeyboardFocus::Surface(s) => {
                KeyboardTarget::key(s, seat, data, key, state, serial, time)
            }
            KeyboardFocus::X11(x, _) => {
                KeyboardTarget::key(&**x, seat, data, key, state, serial, time)
            }
        }
    }

    fn modifiers(
        &self,
        seat: &Seat<Wayluck>,
        data: &mut Wayluck,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        match self {
            KeyboardFocus::Surface(s) => {
                KeyboardTarget::modifiers(s, seat, data, modifiers, serial)
            }
            KeyboardFocus::X11(x, _) => {
                KeyboardTarget::modifiers(&**x, seat, data, modifiers, serial)
            }
        }
    }
}
