//! What the window manager takes from clients: key and button
//! combinations. And where the pointer is, which it hears.

use super::protocol::{
    wayluck_wm_button_binding_v1::{self, WayluckWmButtonBindingV1},
    wayluck_wm_key_binding_v1::{self, WayluckWmKeyBindingV1},
    wayluck_wm_manager_v1,
};
use crate::{Wayluck, windows::WindowId};
use smithay::{
    input::keyboard::{Keysym, ModifiersState},
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, Resource, backend::ClientId,
    },
    utils::{Logical, Point},
};

/// A key combination the window manager has taken.
pub struct KeyBinding {
    pub(super) keysym: u32,
    /// The protocol's modifiers bits (modifier_bits).
    pub(super) modifiers: u32,
}

impl Dispatch<WayluckWmKeyBindingV1, KeyBinding> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _binding: &WayluckWmKeyBindingV1,
        _request: wayluck_wm_key_binding_v1::Request,
        _data: &KeyBinding,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy, handled below.
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        binding: &WayluckWmKeyBindingV1,
        _data: &KeyBinding,
    ) {
        state.wm.bindings.retain(|b| b != binding);
    }
}

/// A pointer button combination the window manager has taken.
pub struct ButtonBinding {
    /// Its Linux code.
    pub(super) button: u32,
    pub(super) modifiers: u32,
}

impl Dispatch<WayluckWmButtonBindingV1, ButtonBinding> for Wayluck {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _binding: &WayluckWmButtonBindingV1,
        _request: wayluck_wm_button_binding_v1::Request,
        _data: &ButtonBinding,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy, handled below.
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        binding: &WayluckWmButtonBindingV1,
        _data: &ButtonBinding,
    ) {
        state.wm.buttons.retain(|b| b != binding);
        state.wm.buttons_down.retain(|(_, b)| b != binding);
    }
}

/// Modifiers as the protocol numbers them (X's): Caps and Num Lock left out.
pub fn modifier_bits(m: &ModifiersState) -> u32 {
    use wayluck_wm_manager_v1::Modifiers;
    [
        (m.shift, Modifiers::Shift),
        (m.ctrl, Modifiers::Ctrl),
        (m.alt, Modifiers::Mod1),
        (m.iso_level5_shift, Modifiers::Mod3),
        (m.logo, Modifiers::Mod4),
        (m.iso_level3_shift, Modifiers::Mod5),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(Modifiers::empty(), |all, (_, m)| all | m)
    .bits()
}

impl Wayluck {
    /// The window manager's binding for a key (any of its keysyms) with
    /// these modifiers, if it has one.
    pub fn wm_binding(&self, syms: &[Keysym], modifiers: u32) -> Option<WayluckWmKeyBindingV1> {
        self.wm
            .bindings
            .iter()
            .find(|b| {
                let k = b
                    .data::<KeyBinding>()
                    .expect("bindings are made with theirs");
                k.modifiers == modifiers && syms.iter().any(|s| s.raw() == k.keysym)
            })
            .cloned()
    }

    /// A button pressed or released: true if it's one the window manager
    /// took (then the client under the pointer mustn't see it).
    pub fn wm_button(&mut self, button: u32, pressed: bool, modifiers: u32) -> bool {
        if !pressed {
            let Some(i) = self.wm.buttons_down.iter().position(|(b, _)| *b == button) else {
                return false;
            };
            let (_, binding) = self.wm.buttons_down.swap_remove(i);
            binding.released();
            return true;
        }
        let found = self.wm.buttons.iter().find(|b| {
            let d = b
                .data::<ButtonBinding>()
                .expect("bindings are made with theirs");
            d.button == button && d.modifiers == modifiers
        });
        let Some(binding) = found.cloned() else {
            return false;
        };
        binding.pressed();
        self.wm.buttons_down.push((button, binding));
        true
    }

    /// The pointer moved: the window manager hears where, and when it's
    /// over another window.
    pub fn wm_pointer_moved(&mut self, at: Point<f64, Logical>, over: Option<WindowId>) {
        let Some(manager) = &self.wm.manager else {
            return;
        };
        manager.pointer_position(at.x.floor() as i32, at.y.floor() as i32);
        if over == self.wm.hovered {
            return;
        }
        if let Some(r) = self.wm.hovered.and_then(|id| self.wm.windows.get(&id)) {
            r.pointer_leave();
        }
        if let Some(r) = over.and_then(|id| self.wm.windows.get(&id)) {
            r.pointer_enter();
        }
        self.wm.hovered = over;
    }
}
