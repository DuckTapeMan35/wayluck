//! Windows drawn moving: from where they were drawn (or where the window
//! manager says they start) to where it placed them, over a while, along a
//! cubic bézier curve (as CSS's cubic-bezier(): from (0, 0) to (1, 1), time
//! across, progress up). Its client draws at the new size at once; what it
//! draws is stretched to the rectangle it's drawn in meanwhile.

use crate::render::rounded::ClipIds;
use smithay::utils::{Logical, Rectangle};
use std::time::{Duration, Instant};

/// A timing curve: its two control points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

impl Curve {
    /// None if it isn't one: x1 and x2 are times, between 0 and 1 (so
    /// there's one progress for each time). Progress may go past 0 or 1
    /// (overshooting, bouncing back).
    pub fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Option<Self> {
        let times = (0.0..=1.0).contains(&x1) && (0.0..=1.0).contains(&x2);
        (times && y1.is_finite() && y2.is_finite()).then_some(Curve { x1, y1, x2, y2 })
    }

    /// How far along it is (0 at the start, 1 at the end) when time t (0
    /// to 1) of the whole has passed.
    pub fn at(&self, t: f64) -> f64 {
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        // The point on the curve whose time is t: time only grows along
        // it, so halving finds it.
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..40 {
            let s = (low + high) / 2.0;
            if bezier(self.x1, self.x2, s) < t {
                low = s;
            } else {
                high = s;
            }
        }
        bezier(self.y1, self.y2, (low + high) / 2.0)
    }
}

/// How windows move, as the window manager set it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Setting {
    pub duration: Duration,
    pub curve: Curve,
}

/// One coordinate of a cubic bézier from 0 to 1, with control points' p1
/// and p2, at s (0 to 1 along it).
fn bezier(p1: f64, p2: f64, s: f64) -> f64 {
    let r = 1.0 - s;
    3.0 * r * r * s * p1 + 3.0 * r * s * s * p2 + s * s * s
}

/// A window on its way: from a rectangle to another (in its scene).
#[derive(Clone, Debug)]
pub struct Animation {
    pub from: Rectangle<i32, Logical>,
    pub to: Rectangle<i32, Logical>,
    pub start: Instant,
    pub duration: Duration,
    pub curve: Curve,
    /// The ids its surfaces are drawn under, stretched.
    pub ids: ClipIds,
}

impl Animation {
    /// Where it's drawn at now; None once it's over (it's at to).
    pub fn at(&self, now: Instant) -> Option<Rectangle<i32, Logical>> {
        let gone = now.saturating_duration_since(self.start);
        if gone >= self.duration {
            return None;
        }
        let p = self
            .curve
            .at(gone.as_secs_f64() / self.duration.as_secs_f64());
        let between = |a: i32, b: i32| (a as f64 + (b - a) as f64 * p).round() as i32;
        let (from, to) = (self.from, self.to);
        Some(Rectangle::new(
            (between(from.loc.x, to.loc.x), between(from.loc.y, to.loc.y)).into(),
            (
                between(from.size.w, to.size.w).max(1),
                between(from.size.h, to.size.h).max(1),
            )
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_curve_starts_at_0_and_ends_at_1() {
        let ease = Curve::new(0.25, 0.1, 0.25, 1.0).unwrap();
        assert_eq!(ease.at(0.0), 0.0);
        assert_eq!(ease.at(1.0), 1.0);
    }

    #[test]
    fn a_straight_curve_is_time_itself() {
        let linear = Curve::new(0.0, 0.0, 1.0, 1.0).unwrap();
        for t in [0.1, 0.25, 0.5, 0.9] {
            assert!((linear.at(t) - t).abs() < 1e-6, "{t}: {}", linear.at(t));
        }
    }

    #[test]
    fn css_ease_is_four_fifths_there_halfway() {
        // cubic-bezier(0.25, 0.1, 0.25, 1) at half the time: 0.8024.
        let ease = Curve::new(0.25, 0.1, 0.25, 1.0).unwrap();
        assert!((ease.at(0.5) - 0.8024).abs() < 1e-3, "{}", ease.at(0.5));
    }

    #[test]
    fn a_curve_may_overshoot_but_its_time_may_not() {
        assert!(Curve::new(0.3, 1.6, 0.6, 1.0).is_some());
        assert!(Curve::new(1.2, 0.0, 0.5, 1.0).is_none());
        assert!(Curve::new(0.5, 0.0, -0.1, 1.0).is_none());
    }

    #[test]
    fn a_window_goes_from_one_rectangle_to_the_other() {
        let start = Instant::now();
        let animation = Animation {
            from: Rectangle::new((0, 0).into(), (100, 100).into()),
            to: Rectangle::new((200, 100).into(), (300, 100).into()),
            start,
            duration: Duration::from_secs(1),
            curve: Curve::new(0.0, 0.0, 1.0, 1.0).unwrap(),
            ids: ClipIds::new(),
        };
        let half = animation.at(start + Duration::from_millis(500)).unwrap();
        assert_eq!(half, Rectangle::new((100, 50).into(), (200, 100).into()));
        assert_eq!(animation.at(start + Duration::from_secs(1)), None);
    }
}
