//! Windows drawn moving: from where they were drawn (or where the window
//! manager says they start) to where it placed them, over a while, along a
//! curve (how far along at each time, as the window manager sampled it).
//! Its client draws at the new size at once; what it draws is stretched to
//! the rectangle it's drawn in meanwhile.

use crate::render::rounded::ClipIds;
use smithay::utils::{Logical, Rectangle};
use std::time::{Duration, Instant};

/// A timing curve: how far along at evenly spaced times from the start
/// (0) to the end (1), at least two. Between them, it's straight.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve(Vec<f32>);

impl Curve {
    /// Longest curve taken: finer is no smoother on screen.
    const MOST: usize = 1024;

    /// None if it isn't one: fewer than two values, more than MOST, or one
    /// that isn't a number. Progress may go past 0 or 1 (overshooting,
    /// bouncing back), and needn't start at 0 or end at 1.
    pub fn new(samples: Vec<f32>) -> Option<Self> {
        let fine =
            (2..=Self::MOST).contains(&samples.len()) && samples.iter().all(|v| v.is_finite());
        fine.then_some(Curve(samples))
    }

    /// How far along it is when time t (0 to 1) of the whole has passed:
    /// 1 at the end, whatever the curve says then.
    pub fn at(&self, t: f64) -> f64 {
        if t >= 1.0 {
            return 1.0;
        }
        let last = (self.0.len() - 1) as f64;
        let at = t.max(0.0) * last;
        let i = (at.floor() as usize).min(self.0.len() - 2);
        let (a, b) = (self.0[i] as f64, self.0[i + 1] as f64);
        a + (b - a) * (at - i as f64)
    }
}

/// How windows move, as the window manager set it.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub duration: Duration,
    pub curve: Curve,
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

    fn linear() -> Curve {
        Curve::new(vec![0.0, 1.0]).unwrap()
    }

    #[test]
    fn a_curve_goes_straight_between_its_samples() {
        let curve = Curve::new(vec![0.0, 0.5, 0.75, 1.0]).unwrap();
        assert_eq!(curve.at(0.0), 0.0);
        assert!((curve.at(1.0 / 6.0) - 0.25).abs() < 1e-6);
        assert!((curve.at(0.5) - 0.625).abs() < 1e-6);
        assert_eq!(curve.at(1.0), 1.0);
    }

    #[test]
    fn a_curve_may_overshoot() {
        let curve = Curve::new(vec![0.0, 1.2, 1.0]).unwrap();
        assert!((curve.at(0.5) - 1.2).abs() < 1e-6);
    }

    #[test]
    fn a_curve_needs_two_numbers_and_no_more_than_it_takes() {
        assert!(Curve::new(vec![0.5]).is_none());
        assert!(Curve::new(vec![0.0, f32::NAN]).is_none());
        assert!(Curve::new(vec![0.0, f32::INFINITY]).is_none());
        assert!(Curve::new(vec![0.0; 1025]).is_none());
        assert!(Curve::new(vec![0.0; 1024]).is_some());
    }

    #[test]
    fn a_window_goes_from_one_rectangle_to_the_other() {
        let start = Instant::now();
        let animation = Animation {
            from: Rectangle::new((0, 0).into(), (100, 100).into()),
            to: Rectangle::new((200, 100).into(), (300, 100).into()),
            start,
            duration: Duration::from_secs(1),
            curve: linear(),
            ids: ClipIds::new(),
        };
        let half = animation.at(start + Duration::from_millis(500)).unwrap();
        assert_eq!(half, Rectangle::new((100, 50).into(), (200, 100).into()));
        assert_eq!(animation.at(start + Duration::from_secs(1)), None);
    }
}
