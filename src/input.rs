//! Input from the backend, passed on to clients: keys to the focused window,
//! the pointer to whatever is under it. Until a window manager decides
//! focus, a click focuses (and raises) the window under the pointer.

use crate::{
    Wayluck,
    layers::Side,
    windows::{WindowId, Windows},
    wm,
};
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    desktop::WindowSurfaceType,
    input::{
        keyboard::{
            FilterResult, Keycode, KeysymHandle, Layout, ModifiersState, XkbConfig, keysyms,
        },
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

impl Wayluck {
    pub fn handle_input<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => self.on_key::<I>(event),
            InputEvent::PointerMotionAbsolute { event } => self.on_motion::<I>(event),
            InputEvent::PointerButton { event } => self.on_button::<I>(event),
            InputEvent::PointerAxis { event } => self.on_axis::<I>(event),
            InputEvent::PointerMotion { event } => self.on_relative_motion::<I>(event),
            _ => {}
        }
    }

    fn on_key<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        let keyboard = self.seat.get_keyboard().unwrap();
        let (code, key_state) = (event.key_code(), event.state());
        keyboard.input::<(), _>(
            self,
            code,
            key_state,
            SERIAL_COUNTER.next_serial(),
            event.time_msec(),
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
        let time = self.start_time.elapsed().as_millis() as u32;
        for key in keyboard.pressed_keys() {
            keyboard.input::<(), _>(
                self,
                key,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
    }

    fn on_motion<I: InputBackend>(&mut self, event: I::PointerMotionAbsoluteEvent) {
        let Some(output) = self.space.outputs().next() else {
            return;
        };
        let Some(area) = self.space.output_geometry(output) else {
            return;
        };
        let location = event.position_transformed(area.size) + area.loc.to_f64();
        self.move_pointer(location, event.time_msec());
    }

    /// A mouse or touchpad moved (on hardware): by so much from where the
    /// pointer is, kept on the output.
    fn on_relative_motion<I: InputBackend>(&mut self, event: I::PointerMotionEvent) {
        let pointer = self.seat.get_pointer().unwrap();
        let mut location = pointer.current_location() + event.delta();
        if let Some(area) = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
        {
            let (x0, y0) = (area.loc.x as f64, area.loc.y as f64);
            location.x = location.x.clamp(x0, x0 + area.size.w as f64 - 1.0);
            location.y = location.y.clamp(y0, y0 + area.size.h as f64 - 1.0);
        }
        self.move_pointer(location, event.time_msec());
    }

    /// The pointer is at location now, by the user's hand or a warp: clients
    /// under it hear, and so does the window manager.
    pub fn move_pointer(&mut self, location: Point<f64, Logical>, time: u32) {
        // Where wayluck draws the cursor, it moved.
        if self.cursor.drawn {
            self.queue_redraw();
        }
        let over = self.window_under(location);
        self.wm_pointer_moved(location, over);
        // While the window manager drags something, no client has the pointer.
        let under = if self.wm.pointer_grabbed {
            None
        } else {
            self.surface_under(location)
        };
        let pointer = self.seat.get_pointer().unwrap();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    fn on_button<I: InputBackend>(&mut self, event: I::PointerButtonEvent) {
        let pointer = self.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        let state = event.state();
        // A button the window manager bound, or any while it drags: not
        // the client's.
        let mods = self.seat.get_keyboard().unwrap().modifier_state();
        let pressed = state == ButtonState::Pressed;

        if self.wm_button(event.button_code(), pressed, wm::modifier_bits(&mods))
            || self.wm.pointer_grabbed
        {
            return;
        }

        let at = pointer.current_location();
        // A click on a bar or launcher that takes keys gives it the keyboard;
        // otherwise, on a window, the window manager hears about it.
        if pressed && !pointer.is_grabbed() && !self.layer_clicked(at) {
            let clicked = self.window_under(at);
            // The window manager decides about focus; without one, what was
            // clicked is raised and gets it (nothing, on the background).
            if self.wm.manager.is_none() {
                if let Some(w) = clicked.and_then(|id| self.windows.get(id)).cloned() {
                    self.space.raise_element(&w, false);
                }
                self.focus(clicked);
            } else if let Some(id) = clicked {
                self.wm_focus_request(id);
            }
        }
        pointer.button(
            self,
            &ButtonEvent {
                button: event.button_code(),
                state,
                serial,
                time: event.time_msec(),
            },
        );
        pointer.frame(self);
    }

    fn on_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        let source = event.source();
        let mut frame = AxisFrame::new(event.time_msec()).source(source);
        for axis in [Axis::Horizontal, Axis::Vertical] {
            // Smooth scrolling in pixels, or wheel clicks (120 per notch).
            let amount = event
                .amount(axis)
                .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
            if amount != 0.0 {
                frame = frame.value(axis, amount);
                if let Some(v120) = event.amount_v120(axis) {
                    frame = frame.v120(axis, v120 as i32);
                }
            }
            // A finger lifted off a touchpad: scrolling stops.
            if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                frame = frame.stop(axis);
            }
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// The surface under a point, and where that surface's origin is: a bar
    /// or overlay above windows, a window, or a wallpaper below them.
    fn surface_under(&self, at: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        if let Some(hit) = self.layer_surface_under(Side::Above, at) {
            return Some(hit);
        }
        let in_window = self.space.element_under(at).and_then(|(window, origin)| {
            window
                .surface_under(at - origin.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, offset)| (surface, (offset + origin).to_f64()))
        });
        in_window.or_else(|| self.layer_surface_under(Side::Below, at))
    }

    /// The window under a point, unless a bar or overlay above windows is.
    fn window_under(&self, at: Point<f64, Logical>) -> Option<WindowId> {
        if self.layer_surface_under(Side::Above, at).is_some() {
            return None;
        }
        let (window, _) = self.space.element_under(at)?;
        Windows::id_of(window.toplevel()?.wl_surface())
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
