//! The pointer: to whatever is under it, unless the window manager is
//! dragging. Until a window manager decides focus, a click focuses (and
//! raises) the window under the pointer.

use crate::{
    Wayluck,
    layers::Side,
    windows::{WindowId, Windows},
    wm,
};
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputTime,
        PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    desktop::WindowSurfaceType,
    input::pointer::{AxisFrame, ButtonEvent, MotionEvent},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::seat::WaylandFocus,
};

impl Wayluck {
    pub(super) fn on_motion<I: InputBackend>(&mut self, event: I::PointerMotionAbsoluteEvent) {
        let Some(output) = self.space.outputs().next() else {
            return;
        };
        let Some(area) = self.space.output_geometry(output) else {
            return;
        };
        let location = event.position_transformed(area.size) + area.loc.to_f64();
        self.move_pointer(location, event.time());
    }

    /// A mouse or touchpad moved (on hardware): by so much from where the
    /// pointer is, kept on the output.
    pub(super) fn on_relative_motion<I: InputBackend>(&mut self, event: I::PointerMotionEvent) {
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
        self.move_pointer(location, event.time());
    }

    /// The pointer is at location now, by the user's hand or a warp: clients
    /// under it hear, and so does the window manager.
    pub fn move_pointer(&mut self, location: Point<f64, Logical>, time: InputTime) {
        // Where wayluck draws the cursor, it moved.
        if self.cursor.drawn {
            self.queue_redraw();
        }
        let over = self.window_under(location);
        self.wm_pointer_moved(location, over);
        // The picker, if it's up, selects the tile under it.
        if self.picker_shown() {
            self.picker_pointer(location);
        }
        // While the window manager drags something, no client has the pointer.
        let under = if self.wm.pointer_grabbed || self.picker_shown() {
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

    /// What's under the pointer changed, though it didn't move: whatever
    /// is there now has it.
    pub fn repoint(&mut self) {
        if self.wm.pointer_grabbed {
            return;
        }
        let pointer = self.seat.get_pointer().unwrap();
        let location = pointer.current_location();
        let under = if self.picker_shown() {
            None
        } else {
            self.surface_under(location)
        };
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
    }

    pub(super) fn on_button<I: InputBackend>(&mut self, event: I::PointerButtonEvent) {
        self.button(event.button_code(), event.state(), event.time());
    }

    /// A pointer button went down or up (an evdev code: 0x110 is the left).
    pub fn button(&mut self, code: u32, state: ButtonState, time: InputTime) {
        let pointer = self.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        // A button the window manager bound, or any while it drags: not
        // the client's.
        let mods = self.seat.get_keyboard().unwrap().modifier_state();
        let pressed = state == ButtonState::Pressed;

        // The picker has the pointer's buttons while it's up: a click
        // shares the tile under it.
        if self.picker_shown() {
            if pressed {
                self.picker_click(pointer.current_location());
            }
            return;
        }

        if self.wm_button(code, pressed, wm::modifier_bits(&mods)) || self.wm.pointer_grabbed {
            return;
        }

        let at = pointer.current_location();
        // A click on a bar or launcher that takes keys gives it the keyboard;
        // otherwise, on a window, the window manager hears about it.
        // A click in an X menu is its program's: focus stays where it is.
        let on_menu =
            self.layer_surface_under(Side::Above, at).is_none() && self.x_menu_under(at).is_some();
        if pressed && !pointer.is_grabbed() && !on_menu && !self.layer_clicked(at) {
            let clicked = self.window_under(at);
            // The window manager decides about focus; without one, what was
            // clicked is raised and gets it (nothing, on the background).
            if self.wm.manager.is_none() {
                if let Some(id) = clicked {
                    self.scenes.raise(id);
                    self.sync_space();
                }
                self.focus(clicked);
            } else if let Some(id) = clicked {
                self.wm_focus_request(id);
            }
        }
        pointer.button(
            self,
            &ButtonEvent {
                button: code,
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    pub(super) fn on_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        let source = event.source();
        let mut frame = AxisFrame::new(event.time()).source(source);
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
        // X's menus, above windows.
        if let Some(hit) = self.x_menu_under(at) {
            return Some(hit);
        }
        let in_window = self.space.element_under(at).and_then(|(window, origin)| {
            window
                .surface_under(at - origin.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, offset)| (surface, (offset + origin).to_f64()))
        });
        in_window.or_else(|| self.layer_surface_under(Side::Below, at))
    }

    /// The window under a point, unless a bar or overlay above windows is, or an X menu.
    fn window_under(&self, at: Point<f64, Logical>) -> Option<WindowId> {
        if self.layer_surface_under(Side::Above, at).is_some() || self.x_menu_under(at).is_some() {
            return None;
        }
        let (window, _) = self.space.element_under(at)?;
        Windows::id_of(&*window.wl_surface()?)
    }
}
