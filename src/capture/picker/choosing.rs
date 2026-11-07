//! Choosing in the picker: the arrows (or Tab) move the selection, as does
//! pointing at a tile; Enter or a click shares it. Escape closes it,
//! choosing nothing.

use super::overlay::{Choice, columns};
use crate::Wayluck;
use smithay::{
    input::keyboard::{Keysym, keysyms},
    utils::{Logical, Point},
};

impl Wayluck {
    /// A key pressed while the picker is up.
    pub fn picker_key(&mut self, key: Keysym) {
        let Some(shown) = &self.picking.shown else {
            return;
        };
        let (n, at) = (shown.choices.len(), shown.selected);
        let row = columns(n);
        let to = match key.raw() {
            keysyms::KEY_Escape => return self.picker_gone(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => {
                return self.picker_choose();
            }
            keysyms::KEY_Right | keysyms::KEY_Tab => at + 1,
            keysyms::KEY_Left | keysyms::KEY_ISO_Left_Tab => at.wrapping_sub(1),
            keysyms::KEY_Down => at + row,
            keysyms::KEY_Up => at.wrapping_sub(row),
            _ => return,
        };
        // Off the grid: it stays where it is.
        if to < n {
            self.picker_select(to);
        }
    }

    /// The pointer moved, to at (global): the tile under it, if any, is
    /// selected.
    pub fn picker_pointer(&mut self, at: Point<f64, Logical>) {
        if let Some(i) = self.picker_tile_at(at) {
            self.picker_select(i);
        }
    }

    /// A pointer button pressed (or released) with the pointer at at
    /// (global): true if it's the picker's, which no client hears. While
    /// it's up every button is; a click shares the tile under it. The
    /// release of a button pressed on it is too, once it's closed: the
    /// client under the pointer never had the press.
    pub fn picker_button(&mut self, code: u32, pressed: bool, at: Point<f64, Logical>) -> bool {
        if !pressed && let Some(i) = self.picking.buttons.iter().position(|b| *b == code) {
            self.picking.buttons.swap_remove(i);
            return true;
        }
        if !self.picker_shown() {
            return false;
        }
        if pressed {
            self.picking.buttons.push(code);
            self.picker_click(at);
        }
        true
    }

    /// A click at at (global): the tile there, if any, is shared.
    fn picker_click(&mut self, at: Point<f64, Logical>) {
        if let Some(i) = self.picker_tile_at(at) {
            self.picker_select(i);
            self.picker_choose();
        }
    }

    fn picker_tile_at(&self, at: Point<f64, Logical>) -> Option<usize> {
        (self.picker_tile_rects().iter()).position(|r| r.to_f64().contains(at))
    }

    fn picker_select(&mut self, i: usize) {
        let Some(shown) = &mut self.picking.shown else {
            return;
        };
        if shown.selected != i {
            shown.selected = i;
            self.queue_redraw();
        }
    }

    /// Share what's selected: whoever asked hears it, as the portal wants
    /// it ("Monitor: NAME", "Window: ID"), and the picker closes. A
    /// workspace is shared through a virtual output made for it.
    fn picker_choose(&mut self) {
        let Some(shown) = &self.picking.shown else {
            return;
        };
        match shown.choices.get(shown.selected).cloned() {
            Some(Choice::Monitor(output)) => {
                self.answer(Some(format!("Monitor: {}", output.name())));
            }
            Some(Choice::Window(id)) => self.answer(Some(format!("Window: {}", u32::from(id)))),
            Some(Choice::Workspace(scene)) => self.share_workspace(scene.number()),
            None => {}
        }
        self.picker_gone();
    }
}
