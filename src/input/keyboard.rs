//! Keys: wayluck's own first, then the window manager's bindings, then the
//! focused client. And the keyboard's layout.

use crate::{Wayluck, wm};
use smithay::{
    backend::input::{Event, InputBackend, InputTime, KeyState, KeyboardKeyEvent},
    input::keyboard::{
        FilterResult, Keycode, KeysymHandle, Layout, ModifiersState, XkbConfig, keysyms,
    },
    utils::SERIAL_COUNTER,
};

impl Wayluck {
    pub(super) fn on_key<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        self.key(event.key_code(), event.state(), event.time());
    }

    /// A key went down or up: wayluck's own keys and the window manager's
    /// bindings first, then the focused client.
    pub fn key(&mut self, code: Keycode, key_state: KeyState, time: InputTime) {
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.input::<(), _>(
            self,
            code,
            key_state,
            SERIAL_COUNTER.next_serial(),
            time,
            |state, mods, keysym| state.filter_key(code, key_state, mods, keysym),
        );
    }

    /// A key the window manager bound goes to it, not to the client: it
    /// hears the press, and the release goes nowhere.
    fn filter_key(
        &mut self,
        code: Keycode,
        key_state: KeyState,
        mods: &ModifiersState,
        keysym: KeysymHandle<'_>,
    ) -> FilterResult<()> {
        if key_state == KeyState::Released {
            if let Some(i) = self.keys_to_wm.iter().position(|k| *k == code) {
                self.keys_to_wm.swap_remove(i);
                return FilterResult::Intercept(());
            }
            return FilterResult::Forward;
        }

        // wayluck's own keys come before anyone's.
        if let Some(key) = compositor_key(mods, &keysym) {
            self.keys_to_wm.push(code);
            match key {
                CompositorKey::Quit => {
                    tracing::info!("exiting (Ctrl+Alt+Backspace)");
                    self.loop_signal.stop();
                }
                CompositorKey::Vt(vt) => {
                    if let Some(change_vt) = &self.change_vt {
                        change_vt(vt);
                    }
                }
            }
            return FilterResult::Intercept(());
        }

        // The picker has every key while it's up: Escape closes it.
        if self.picker_shown() {
            self.keys_to_wm.push(code);
            self.picker_key(keysym.modified_sym());
            return FilterResult::Intercept(());
        }

        // As typed (shift+1 is !) and as on the key (1).
        let mut syms = keysym.raw_syms();
        syms.push(keysym.modified_sym());
        let Some(binding) = self.wm_binding(&syms, wm::modifier_bits(mods)) else {
            return FilterResult::Forward;
        };
        binding.pressed();
        if !self.keys_to_wm.contains(&code) {
            self.keys_to_wm.push(code);
        }
        FilterResult::Intercept(())
    }

    /// The keyboard's layout, by XKB names (empty: XKB's default). Clients
    /// get the new keymap; one XKB can't make leaves the old one.
    pub fn set_keymap(&mut self, model: &str, layout: &str, variant: &str, options: &str) {
        let config = XkbConfig {
            model,
            layout,
            variant,
            options: (!options.is_empty()).then(|| options.to_string()),
            ..XkbConfig::default()
        };
        let keyboard = self.seat.get_keyboard().unwrap();
        match keyboard.set_xkb_config(self, config) {
            Ok(()) => tracing::info!("keymap: {layout:?} {variant:?} {options:?}"),
            Err(e) => tracing::warn!("no keymap for {layout:?} {variant:?}: {e:?}"),
        }
    }

    /// Switch to the keymap's layout at index (from 0).
    pub fn set_layout(&mut self, index: u32) {
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.with_xkb_state(self, |mut xkb| xkb.set_layout(Layout(index)));
    }

    /// Lost the keyboard (nested: the host focused another window): let go of
    /// every key held. Their releases go elsewhere, and a modifier left down
    /// would turn every later key into a shortcut (Return into Super+Return).
    pub fn release_all_keys(&mut self) {
        let keyboard = self.seat.get_keyboard().unwrap();
        for key in keyboard.pressed_keys() {
            keyboard.input::<(), _>(
                self,
                key,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                InputTime::now(),
                |_, _, _| FilterResult::Forward,
            );
        }
    }
}

/// A key wayluck keeps for itself.
enum CompositorKey {
    /// Ctrl+Alt+Backspace: exit, whatever else is going on.
    Quit,
    /// Ctrl+Alt+F1…F12: switch to that TTY.
    Vt(i32),
}

/// Is this press one of wayluck's own keys? XKB's default keymap already
/// makes Ctrl+Alt+F-keys XF86Switch_VT_1 to 12.
fn compositor_key(mods: &ModifiersState, keysym: &KeysymHandle<'_>) -> Option<CompositorKey> {
    let sym = keysym.modified_sym().raw();
    if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&sym) {
        return Some(CompositorKey::Vt(
            (sym - keysyms::KEY_XF86Switch_VT_1 + 1) as i32,
        ));
    }
    let backspace = keysym
        .raw_syms()
        .iter()
        .any(|s| s.raw() == keysyms::KEY_BackSpace);
    (mods.ctrl && mods.alt && backspace).then_some(CompositorKey::Quit)
}
