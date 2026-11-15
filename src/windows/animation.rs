//! Windows drawn moving: from where they were drawn (or where the window
//! manager says they start) to where it placed them, either over a while
//! along a curve (how far along at each time, as the window manager
//! sampled it), or pulled there by a spring (until they settle, keeping
//! their speed when placed elsewhere on their way). Its client draws at the
//! new size at once; what it draws is stretched to the rectangle it's drawn
//! in meanwhile.

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

/// A spring pulling a window to where it's placed (as if it weighed 1):
/// how hard (stiffness, per second squared) and how much it's held back
/// (damping, as a ratio: 1 gets there quickest without overshooting, less
/// bounces, more creeps).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub stiffness: f64,
    pub damping: f64,
}

impl Spring {
    /// Close enough to be there: under half a pixel away, and slower than
    /// this (pixels a second; a third of a pixel a frame at 60 a second).
    const SETTLED_SPEED: f64 = 20.0;
    /// However weak or bouncy: there by then.
    const LONGEST: Duration = Duration::from_secs(10);

    /// None if it isn't one: a stiffness that isn't a number or is below
    /// 0, or a damping that isn't a number or isn't above 0 (it would
    /// never settle).
    pub fn new(stiffness: f64, damping: f64) -> Option<Self> {
        let fine =
            stiffness.is_finite() && stiffness >= 0.0 && damping.is_finite() && damping > 0.0;
        fine.then_some(Spring { stiffness, damping })
    }

    /// How far from where it's pulled to, and how fast, t seconds after
    /// being off by x0 at speed v0.
    fn after(&self, x0: f64, v0: f64, t: f64) -> (f64, f64) {
        let w = self.stiffness.sqrt();
        let z = self.damping;
        if (z - 1.0).abs() < 1e-6 {
            // Critically damped.
            let (a, b) = (x0, v0 + w * x0);
            let e = (-w * t).exp();
            ((a + b * t) * e, (b - w * (a + b * t)) * e)
        } else if z < 1.0 {
            // Bouncing.
            let wd = w * (1.0 - z * z).sqrt();
            let (a, b) = (x0, (v0 + z * w * x0) / wd);
            let e = (-z * w * t).exp();
            let (sin, cos) = (wd * t).sin_cos();
            let x = e * (a * cos + b * sin);
            let v = e * ((b * wd - z * w * a) * cos - (a * wd + z * w * b) * sin);
            (x, v)
        } else {
            // Creeping.
            let root = (z * z - 1.0).sqrt();
            let (r1, r2) = (-w * (z - root), -w * (z + root));
            let c1 = (v0 - r2 * x0) / (r1 - r2);
            let c2 = x0 - c1;
            let (e1, e2) = ((r1 * t).exp(), (r2 * t).exp());
            (c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2)
        }
    }
}

/// How windows move, as the window manager set it.
#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    Curve { duration: Duration, curve: Curve },
    Spring(Spring),
}

impl Setting {
    /// Does it move them at all? (A duration of 0, a stiffness of 0: no.)
    pub fn moves(&self) -> bool {
        match self {
            Setting::Curve { duration, .. } => !duration.is_zero(),
            Setting::Spring(spring) => spring.stiffness > 0.0,
        }
    }
}

/// How a window on its way moves.
#[derive(Clone, Debug)]
pub enum Motion {
    Curve {
        duration: Duration,
        curve: Curve,
    },
    /// Starting at speed (pixels a second: x, y, width, height).
    Spring {
        spring: Spring,
        speed: [f64; 4],
    },
}

impl Motion {
    /// As setting says, starting at speed (a spring's; a curve starts
    /// still).
    pub fn new(setting: Setting, speed: [f64; 4]) -> Self {
        match setting {
            Setting::Curve { duration, curve } => Motion::Curve { duration, curve },
            Setting::Spring(spring) => Motion::Spring { spring, speed },
        }
    }
}

/// A window on its way: from a rectangle to another (in its scene).
#[derive(Clone, Debug)]
pub struct Animation {
    pub from: Rectangle<i32, Logical>,
    pub to: Rectangle<i32, Logical>,
    pub start: Instant,
    pub motion: Motion,
    /// The ids its surfaces are drawn under, stretched.
    pub ids: ClipIds,
}

impl Animation {
    /// Where it's drawn at now; None once it's over (it's at to).
    pub fn at(&self, now: Instant) -> Option<Rectangle<i32, Logical>> {
        let [x, y, w, h] = self.state(now)?.map(|(at, _)| at.round() as i32);
        Some(Rectangle::new((x, y).into(), (w.max(1), h.max(1)).into()))
    }

    /// How fast it's going now (pixels a second: x, y, width, height):
    /// still, if it's there, or moving along a curve (which isn't carried
    /// on).
    pub fn speed(&self, now: Instant) -> [f64; 4] {
        match self.motion {
            Motion::Spring { .. } => self.state(now).map_or([0.0; 4], |s| s.map(|(_, v)| v)),
            Motion::Curve { .. } => [0.0; 4],
        }
    }

    /// Where it is and how fast it's going now, for x, y, width and
    /// height; None once it's over.
    fn state(&self, now: Instant) -> Option<[(f64, f64); 4]> {
        let gone = now.saturating_duration_since(self.start);
        let (from, to) = (self.from, self.to);
        let from = [from.loc.x, from.loc.y, from.size.w, from.size.h].map(f64::from);
        let to = [to.loc.x, to.loc.y, to.size.w, to.size.h].map(f64::from);
        match &self.motion {
            Motion::Curve { duration, curve } => {
                if gone >= *duration {
                    return None;
                }
                let p = curve.at(gone.as_secs_f64() / duration.as_secs_f64());
                Some(std::array::from_fn(|i| {
                    (from[i] + (to[i] - from[i]) * p, 0.0)
                }))
            }
            Motion::Spring { spring, speed } => {
                if gone >= Spring::LONGEST {
                    return None;
                }
                let t = gone.as_secs_f64();
                let state: [(f64, f64); 4] = std::array::from_fn(|i| {
                    let (x, v) = spring.after(from[i] - to[i], speed[i], t);
                    (to[i] + x, v)
                });
                let settled = (state.iter().zip(to))
                    .all(|((at, v), to)| (at - to).abs() < 0.5 && v.abs() < Spring::SETTLED_SPEED);
                (!settled).then_some(state)
            }
        }
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
            motion: Motion::Curve {
                duration: Duration::from_secs(1),
                curve: linear(),
            },
            ids: ClipIds::new(),
        };
        let half = animation.at(start + Duration::from_millis(500)).unwrap();
        assert_eq!(half, Rectangle::new((100, 50).into(), (200, 100).into()));
        assert_eq!(animation.at(start + Duration::from_secs(1)), None);
    }

    /// From x 0 to x 1000, at speed (along x).
    fn sprung(spring: Spring, speed: f64) -> Animation {
        Animation {
            from: Rectangle::new((0, 0).into(), (100, 100).into()),
            to: Rectangle::new((1000, 0).into(), (100, 100).into()),
            start: Instant::now(),
            motion: Motion::Spring {
                spring,
                speed: [speed, 0.0, 0.0, 0.0],
            },
            ids: ClipIds::new(),
        }
    }

    /// Where it is along x, s seconds in.
    fn x_after(animation: &Animation, s: f64) -> Option<i32> {
        let now = animation.start + Duration::from_secs_f64(s);
        animation.at(now).map(|r| r.loc.x)
    }

    #[test]
    fn a_critically_damped_spring_gets_there_without_overshooting() {
        let animation = sprung(Spring::new(400.0, 1.0).unwrap(), 0.0);
        let path: Vec<_> = (0..200)
            .map_while(|i| x_after(&animation, i as f64 / 100.0))
            .collect();
        assert!(path.windows(2).all(|w| w[0] <= w[1]), "it went back");
        assert!(path.iter().all(|&x| x <= 1000), "it overshot");
        // ω = 20 a second: well within a second.
        assert!(path.len() < 100, "it took {} hundredths", path.len());
    }

    #[test]
    fn a_bouncy_spring_overshoots_and_settles() {
        let animation = sprung(Spring::new(400.0, 0.3).unwrap(), 0.0);
        let path: Vec<_> = (0..1000)
            .map_while(|i| x_after(&animation, i as f64 / 100.0))
            .collect();
        assert!(path.iter().any(|&x| x > 1000), "it never overshot");
        assert!(path.len() < 1000, "it never settled");
    }

    #[test]
    fn a_creeping_spring_gets_there_too() {
        let animation = sprung(Spring::new(400.0, 3.0).unwrap(), 0.0);
        let path: Vec<_> = (0..1000)
            .map_while(|i| x_after(&animation, i as f64 / 100.0))
            .collect();
        assert!(path.iter().all(|&x| x <= 1000));
        assert!(path.len() < 1000, "it never settled");
    }

    #[test]
    fn a_spring_starts_at_the_speed_it_had() {
        let spring = Spring::new(400.0, 1.0).unwrap();
        // Already going towards it fast: further along at once than from
        // still; going away: back a little first.
        let (still, fast, away) = (
            sprung(spring, 0.0),
            sprung(spring, 5000.0),
            sprung(spring, -5000.0),
        );
        let at = |a: &Animation| x_after(a, 0.01).unwrap();
        assert!(at(&fast) > at(&still));
        assert!(at(&away) < 0, "at {}", at(&away));
        let speed = fast.speed(fast.start);
        assert!((speed[0] - 5000.0).abs() < 1e-6);
    }

    #[test]
    fn a_spring_needs_numbers_and_some_damping() {
        assert!(Spring::new(f64::NAN, 1.0).is_none());
        assert!(Spring::new(-1.0, 1.0).is_none());
        assert!(Spring::new(400.0, 0.0).is_none());
        assert!(Spring::new(400.0, f64::INFINITY).is_none());
        assert!(Spring::new(0.0, 1.0).is_some());
    }
}
