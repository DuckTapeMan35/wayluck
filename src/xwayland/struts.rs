//! Struts: the room an X dock keeps for itself along the screen's edges
//! (_NET_WM_STRUT_PARTIAL, or the older _NET_WM_STRUT), which windows
//! should leave it, as a layer-shell bar's exclusive zone.

use crate::Wayluck;
use smithay::utils::{Logical, Rectangle};

/// How far from each edge of X's screen (the box around all outputs) a
/// dock keeps, and along which stretch of that edge (first and last pixel,
/// in X's coordinates).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Strut {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
    pub left_along: (u32, u32),
    pub right_along: (u32, u32),
    pub top_along: (u32, u32),
    pub bottom_along: (u32, u32),
}

impl Strut {
    /// From a strut property's numbers: 12 (partial), or 4 (along the
    /// whole of each edge).
    pub fn from_property(values: &[u32]) -> Option<Strut> {
        let whole = (0, u32::MAX);
        let along = |i: usize| (values[i], values[i + 1]);
        match values.len() {
            4 => Some(Strut {
                left: values[0],
                right: values[1],
                top: values[2],
                bottom: values[3],
                left_along: whole,
                right_along: whole,
                top_along: whole,
                bottom_along: whole,
            }),
            12.. => Some(Strut {
                left: values[0],
                right: values[1],
                top: values[2],
                bottom: values[3],
                left_along: along(4),
                right_along: along(6),
                top_along: along(8),
                bottom_along: along(10),
            }),
            _ => None,
        }
    }

    /// What it keeps of usable (an output's room, global coordinates)
    /// taken away: an edge kept along part of the output moves that edge
    /// of usable in. screen: X's screen.
    pub fn reserve(
        &self,
        mut usable: Rectangle<i32, Logical>,
        screen: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        let clamp = |v: u32| v.min(i32::MAX as u32) as i32;
        let crosses = |(first, last): (u32, u32), from: i32, length: i32| {
            clamp(first) < from + length && clamp(last) >= from
        };
        let (x0, y0) = (usable.loc.x, usable.loc.y);
        let (x1, y1) = (x0 + usable.size.w, y0 + usable.size.h);
        let (mut left, mut top, mut right, mut bottom) = (x0, y0, x1, y1);
        if self.left > 0 && crosses(self.left_along, y0, usable.size.h) {
            left = left.max(screen.loc.x + clamp(self.left));
        }
        if self.right > 0 && crosses(self.right_along, y0, usable.size.h) {
            right = right.min(screen.loc.x + screen.size.w - clamp(self.right));
        }
        if self.top > 0 && crosses(self.top_along, x0, usable.size.w) {
            top = top.max(screen.loc.y + clamp(self.top));
        }
        if self.bottom > 0 && crosses(self.bottom_along, x0, usable.size.w) {
            bottom = bottom.min(screen.loc.y + screen.size.h - clamp(self.bottom));
        }
        usable.loc = (left, top).into();
        usable.size = ((right - left).max(0), (bottom - top).max(0)).into();
        usable
    }
}

impl Wayluck {
    /// usable, less what the shown X docks keep.
    pub fn x_reserve(&self, usable: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
        let screen = (self.space.outputs())
            .filter_map(|o| self.space.output_geometry(o))
            .reduce(|a, b| a.merge(b))
            .unwrap_or_default();
        let docks = self.x_docks.iter().filter_map(|d| d.x11_surface());
        let struts = docks.filter_map(|x11| self.x_struts.get(&x11.window_id()));
        struts.fold(usable, |usable, strut| strut.reserve(usable, screen))
    }

    /// An X window's strut changed (None: it has none now).
    pub fn x_strut_changed(&mut self, window: u32, strut: Option<Strut>) {
        tracing::debug!("X: window {window} keeps {strut:?}");
        match strut {
            Some(strut) => self.x_struts.insert(window, strut),
            None => self.x_struts.remove(&window),
        };
        self.reserved_changed();
    }

    /// What's kept for docks may have changed: each output's room is
    /// worked out again, and the window manager hears if it changed.
    pub fn reserved_changed(&mut self) {
        let outputs: Vec<_> = self.space.outputs().cloned().collect();
        for output in outputs {
            self.usable_changed(&output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn a_bar_along_the_top_takes_its_height() {
        let screen = rect(0, 0, 1920, 1080);
        let bar = Strut::from_property(&[0, 0, 30, 0]).unwrap();
        assert_eq!(bar.reserve(screen, screen), rect(0, 30, 1920, 1050));
    }

    #[test]
    fn a_partial_strut_only_touches_outputs_along_it() {
        // Two outputs side by side; a bar along the top of the left one.
        let screen = rect(0, 0, 3840, 1080);
        let (left, right) = (rect(0, 0, 1920, 1080), rect(1920, 0, 1920, 1080));
        let bar = Strut::from_property(&[0, 0, 30, 0, 0, 0, 0, 0, 0, 1919, 0, 0]).unwrap();
        assert_eq!(bar.reserve(left, screen), rect(0, 30, 1920, 1050));
        assert_eq!(bar.reserve(right, screen), right);
    }

    #[test]
    fn a_strut_along_the_far_edge_is_from_the_screens_edge() {
        let screen = rect(0, 0, 3840, 1080);
        let right = rect(1920, 0, 1920, 1080);
        let panel = Strut::from_property(&[0, 40, 0, 0]).unwrap();
        assert_eq!(panel.reserve(right, screen), rect(1920, 0, 1880, 1080));
    }

    #[test]
    fn struts_of_the_wrong_size_are_none() {
        assert_eq!(Strut::from_property(&[1, 2, 3]), None);
    }
}
