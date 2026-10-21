//! Animations: a value moving from one place to another along a curve, as Hyprland's config
//! writes them (`bezier = name, x1, y1, x2, y2`; `animation = windows, 1, speed, curve`).
//! A window opening fades and grows in, a window moving glides to its new place.

use std::time::{Duration, Instant};

/// A CSS-style cubic bezier from (0,0) to (1,1) through (x1,y1) and (x2,y2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bezier {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl Bezier {
    /// Hyprland's default curve.
    pub const DEFAULT: Bezier = Bezier { x1: 0.05, y1: 0.9, x2: 0.1, y2: 1.05 };
    /// A gentle one for fades.
    pub const EASE_OUT: Bezier = Bezier { x1: 0.25, y1: 1.0, x2: 0.5, y2: 1.0 };

    fn coord(a: f64, b: f64, t: f64) -> f64 {
        // the curve's one axis at t: 3(1-t)²t·a + 3(1-t)t²·b + t³
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    }

    /// How far along (0..1, may overshoot) at time fraction `x`: t found for x, then y at t.
    pub fn at(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        let (mut lo, mut hi) = (0.0, 1.0);
        let mut t = x;
        for _ in 0..30 {
            let cx = Self::coord(self.x1, self.x2, t);
            if (cx - x).abs() < 1e-5 {
                break;
            }
            if cx < x {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) / 2.0;
        }
        Self::coord(self.y1, self.y2, t)
    }
}

/// One value on its way: from, to, when it started, how long, along which curve.
#[derive(Clone, Copy, Debug)]
pub struct Anim {
    pub from: f64,
    pub to: f64,
    pub start: Instant,
    pub dur: Duration,
    pub curve: Bezier,
}

impl Anim {
    pub fn new(from: f64, to: f64, dur: Duration, curve: Bezier) -> Anim {
        Anim { from, to, start: Instant::now(), dur, curve }
    }
    /// Already there (nothing to animate).
    pub fn still(v: f64) -> Anim {
        Anim { from: v, to: v, start: Instant::now(), dur: Duration::ZERO, curve: Bezier::DEFAULT }
    }
    pub fn value(&self, now: Instant) -> f64 {
        if self.dur.is_zero() {
            return self.to;
        }
        let x = now.saturating_duration_since(self.start).as_secs_f64() / self.dur.as_secs_f64();
        if x >= 1.0 {
            return self.to;
        }
        self.from + (self.to - self.from) * self.curve.at(x)
    }
    pub fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= self.dur
    }
    /// The same value heading somewhere else, from where it is now (no jump mid-way).
    pub fn retarget(&mut self, to: f64, dur: Duration, now: Instant) {
        if (to - self.to).abs() < f64::EPSILON {
            return;
        }
        *self = Anim { from: self.value(now), to, start: now, dur, curve: self.curve };
    }
}

/// A window's animated look: where it is drawn (x, y), its opacity and scale.
#[derive(Clone, Copy, Debug)]
pub struct WindowAnim {
    pub x: Anim,
    pub y: Anim,
    pub alpha: Anim,
    pub scale: Anim,
    /// made, but nothing drawn yet: hidden, and the opening starts with its first frame
    pub waiting: bool,
}

impl WindowAnim {
    /// A new window: fades in and grows from 85 % at its place.
    pub fn opening(x: f64, y: f64) -> WindowAnim {
        let d = Duration::from_millis(280);
        WindowAnim { x: Anim::still(x), y: Anim::still(y), alpha: Anim::new(0.0, 1.0, d, Bezier::EASE_OUT), scale: Anim::new(0.85, 1.0, d, Bezier::DEFAULT), waiting: true }
    }
    /// Its first frame came: the opening starts now.
    pub fn shown(&mut self, now: Instant) {
        if self.waiting {
            self.waiting = false;
            self.alpha.start = now;
            self.scale.start = now;
        }
    }
    pub fn moving_to(&mut self, x: f64, y: f64, now: Instant) {
        let d = Duration::from_millis(320);
        self.x.retarget(x, d, now);
        self.y.retarget(y, d, now);
    }
    pub fn done(&self, now: Instant) -> bool {
        !self.waiting && self.x.done(now) && self.y.done(now) && self.alpha.done(now) && self.scale.done(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_start_and_end_where_they_should() {
        for b in [Bezier::DEFAULT, Bezier::EASE_OUT, Bezier { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 }] {
            assert!(b.at(0.0).abs() < 1e-3);
            assert!((b.at(1.0) - 1.0).abs() < 1e-3);
        }
        let linear = Bezier { x1: 1.0 / 3.0, y1: 1.0 / 3.0, x2: 2.0 / 3.0, y2: 2.0 / 3.0 };
        assert!((linear.at(0.5) - 0.5).abs() < 1e-3);
        assert!(Bezier::DEFAULT.at(0.3) > 0.9, "Hyprland's default is quick, then settles");
    }

    #[test]
    fn retargeting_goes_on_from_where_it_is() {
        let t0 = Instant::now();
        let mut a = Anim { from: 0.0, to: 100.0, start: t0, dur: Duration::from_millis(100), curve: Bezier { x1: 1.0 / 3.0, y1: 1.0 / 3.0, x2: 2.0 / 3.0, y2: 2.0 / 3.0 } };
        let mid = t0 + Duration::from_millis(50);
        assert!((a.value(mid) - 50.0).abs() < 1.0);
        a.retarget(0.0, Duration::from_millis(100), mid);
        assert!((a.value(mid) - 50.0).abs() < 1.0, "no jump");
        assert!(a.value(mid + Duration::from_millis(200)).abs() < 1e-6);
    }
}
