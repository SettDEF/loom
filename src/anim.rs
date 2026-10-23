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
    #[cfg_attr(not(test), allow(dead_code))]
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
    /// how it glides to a new place (windowsMove)
    pub move_dur: Duration,
    /// its first picture was not yet at the size it was given: the opening waits for that one
    pub deferred: bool,
}

/// How long an opening waits for a window to draw at the size it was given (a program that
/// keeps its own size opens after it anyway).
const SIZE_GRACE: Duration = Duration::from_millis(250);

impl WindowAnim {
    /// A new window, as the config's windowsIn and fadeIn say: grown from its popin size and faded
    /// in at its place. `fade` false: shown as it is (KDE's effects animate it instead).
    pub fn opening(fade: bool, x: f64, y: f64, cfg: &crate::layer_anim::Config) -> WindowAnim {
        let (m, f) = (cfg.windows_in, cfg.win_fade_in);
        let on = fade && cfg.enabled;
        let from = match m.style {
            Some(crate::layer_anim::Style::Popin(s)) => s,
            _ => 0.85,
        };
        let alpha = if on && f.on { Anim::new(0.0, 1.0, f.dur, f.curve) } else { Anim::still(1.0) };
        let scale = if on && m.on { Anim::new(from, 1.0, m.dur, m.curve) } else { Anim::still(1.0) };
        let mv = cfg.windows_move;
        let still = |v: f64| Anim { curve: mv.curve, ..Anim::still(v) };
        WindowAnim { x: still(x), y: still(y), alpha, scale, waiting: true, move_dur: if cfg.enabled && mv.on { mv.dur } else { Duration::ZERO }, deferred: false }
    }
    /// A frame came: the opening starts with the first one at the size it was given (`fits`),
    /// so it grows from its own middle, not from a first picture of another size; at the latest
    /// SIZE_GRACE after its first frame. Until it starts it is drawn faded out.
    pub fn shown(&mut self, now: Instant, fits: bool) {
        let start = |a: &mut WindowAnim, at: Instant| {
            a.alpha.start = at;
            a.scale.start = at;
        };
        if self.waiting {
            self.waiting = false;
            self.deferred = !fits;
            start(self, if fits { now } else { now + SIZE_GRACE });
        } else if self.deferred && fits {
            self.deferred = false;
            if self.alpha.start > now {
                start(self, now);
            }
        }
    }
    pub fn moving_to(&mut self, x: f64, y: f64, now: Instant) {
        self.x.retarget(x, self.move_dur, now);
        self.y.retarget(y, self.move_dur, now);
    }
    /// Straight there, no glide (a window dragged by hand).
    pub fn jump_to(&mut self, x: f64, y: f64) {
        self.x = Anim::still(x);
        self.y = Anim::still(y);
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
    fn an_opening_waits_for_the_size_it_was_given() {
        let mut cfg = crate::layer_anim::Config::load();
        cfg.enabled = true;
        cfg.win_fade_in = crate::layer_anim::Motion { on: true, dur: Duration::from_millis(200), curve: Bezier::DEFAULT, style: None };
        let t0 = Instant::now();
        let mut a = WindowAnim::opening(true, 0.0, 0.0, &cfg);
        // a first picture at another size: not started (faded out) until the right one comes
        a.shown(t0, false);
        assert!(a.alpha.value(t0 + Duration::from_millis(100)) < 1e-6, "still hidden");
        a.shown(t0 + Duration::from_millis(40), true);
        assert!(a.alpha.value(t0 + Duration::from_millis(41)) <= a.alpha.value(t0 + Duration::from_millis(120)));
        assert!(!a.deferred && a.alpha.start <= t0 + Duration::from_millis(40), "starts with the right size");
        // a program that never takes the size: opens after the grace anyway
        let mut b = WindowAnim::opening(true, 0.0, 0.0, &cfg);
        b.shown(t0, false);
        assert_eq!(b.alpha.start, t0 + SIZE_GRACE);
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
