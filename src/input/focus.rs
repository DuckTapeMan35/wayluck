//! What has the keyboard: a Wayland surface, or an X window. An X window
//! needs more than its surface's wl_keyboard.enter: its X client is told
//! too (X input focus, or WM_TAKE_FOCUS), by smithay's X11Surface. Leaving
//! takes X input focus away again, so no X window has it while a Wayland
//! one has the keyboard.

use crate::{Wayluck, layers, windows::WindowId};
use smithay::{
    backend::input::{InputTime, KeyState},
    desktop::PopupKind,
    input::{
        Seat,
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, SERIAL_COUNTER, Serial},
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
        time: InputTime,
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

impl Wayluck {
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
        self.taskbars_changed();
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
        let focus = match &self.layer_focus {
            Some(layer) => Some(KeyboardFocus::from(layer.wl_surface().clone())),
            None => (self.windows.focused)
                .and_then(|id| self.windows.get(id))
                .and_then(|w| match w.x11_surface() {
                    Some(x11) => KeyboardFocus::x11(x11),
                    None => w.toplevel().map(|t| t.wl_surface().clone().into()),
                }),
        };
        let keyboard = self.seat.get_keyboard().unwrap();
        if keyboard.current_focus() == focus {
            return;
        }
        let menus = self.x_has_keyboard();
        keyboard.set_focus(self, focus, SERIAL_COUNTER.next_serial());
        // X's menus came or went with it: drawn or not,
        // and the pointer over one that went is over what's below now.
        if self.x_has_keyboard() != menus {
            self.queue_redraw();
            self.repoint();
        }
    }
}
