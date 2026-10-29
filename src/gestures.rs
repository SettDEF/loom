//! Touchpad gestures, Hyprland's way: `gesture = FINGERS, DIRECTION, ACTION[, ARGS]` lines
//! (swipe, horizontal, vertical, up, down, left, right, pinch, pinchin, pinchout; actions move,
//! float, workspace, close, fullscreen, dispatcher) and `gestures { workspace_swipe_* }`. A
//! gesture no line takes goes to the program under the pointer (pointer-gestures: a browser's
//! pinch zoom, say).

use smithay::{
    delegate_pointer_gestures,
    input::pointer::{GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent},
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::pointer_gestures::PointerGesturesState,
};

use crate::state::Loom;

pub fn init(dh: &smithay::reexports::wayland_server::DisplayHandle) {
    std::mem::forget(PointerGesturesState::new::<Loom>(dh));
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gesture {
    pub fingers: u32,
    pub dir: String,
    pub action: String,
    pub args: String,
}

/// The `gesture =` lines and the swipe settings.
#[derive(Clone, Debug)]
pub struct Config {
    pub gestures: Vec<Gesture>,
    /// how far (px) a full workspace swipe is; the share of it that commits
    pub distance: f64,
    pub cancel_ratio: f64,
    pub lock_threshold: f64,
    pub create_new: bool,
    pub invert: bool,
}

impl Config {
    pub fn load() -> Config {
        let mut c = Config { gestures: Vec::new(), distance: 300.0, cancel_ratio: 0.5, lock_threshold: 10.0, create_new: true, invert: true };
        for (k, v) in crate::config::hyprland_lines(&crate::config::hyprland_conf()) {
            let num = |d: f64| v.trim().parse().unwrap_or(d);
            match k.as_str() {
                "gesture" => c.gestures.extend(parse(&v)),
                "gestures:workspace_swipe_distance" => c.distance = num(c.distance).max(1.0),
                "gestures:workspace_swipe_cancel_ratio" => c.cancel_ratio = num(c.cancel_ratio).clamp(0.0, 1.0),
                "gestures:workspace_swipe_direction_lock_threshold" => c.lock_threshold = num(c.lock_threshold),
                "gestures:workspace_swipe_create_new" => c.create_new = !matches!(v.trim(), "false" | "0"),
                "gestures:workspace_swipe_invert" => c.invert = !matches!(v.trim(), "false" | "0"),
                _ => {}
            }
        }
        c
    }

    /// The line for `fingers` and a swipe locked to `dir` (left/right/up/down), or a pinch.
    fn find(&self, fingers: u32, matches: impl Fn(&str) -> bool) -> Option<&Gesture> {
        // the most specific first: up before vertical before swipe
        self.gestures.iter().filter(|g| g.fingers == fingers && matches(&g.dir)).min_by_key(|g| match g.dir.as_str() {
            "up" | "down" | "left" | "right" | "pinchin" | "pinchout" => 0,
            "horizontal" | "vertical" => 1,
            _ => 2,
        })
    }
}

/// `3, swipe, move,` / `4, up, dispatcher, global, quickshell:overviewWorkspacesToggle`
fn parse(v: &str) -> Option<Gesture> {
    let p: Vec<&str> = v.split(',').map(str::trim).collect();
    let fingers = p.first()?.parse().ok()?;
    let dir = p.get(1)?.to_string();
    // `mod: SUPER` / `scale: …` options before the action are skipped
    let mut i = 2;
    while p.get(i).is_some_and(|s| s.contains(':') && !s.starts_with("quickshell")) {
        i += 1;
    }
    let action = p.get(i)?.to_string();
    let args = p.get(i + 1..).map(|a| a.iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    Some(Gesture { fingers, dir, action, args })
}

/// A swipe or pinch under way.
#[derive(Clone, Debug, Default)]
pub struct Active {
    pub fingers: u32,
    /// how far the fingers went (swipe), the pinch's scale
    pub moved: Point<f64, Logical>,
    pub scale: f64,
    /// the direction it locked to, and the line it matched (None: the program gets it)
    pub locked: Option<&'static str>,
    pub taken: Option<Gesture>,
    /// a window dragged by `move`: the virtual pointer it follows
    pub drag_at: Option<Point<f64, Logical>>,
    /// forwarded to the program
    pub forwarded: bool,
    /// the surface its begin went to (its end goes there too, or none at all)
    pub to: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    pub pinch: bool,
}

impl Loom {
    pub fn swipe_begin(&mut self, fingers: u32, time: u32) {
        self.activity(false);
        let bound = self.gesture_cfg.gestures.iter().any(|g| g.fingers == fingers && !g.dir.starts_with("pinch"));
        self.gesture = Some(Active { fingers, scale: 1.0, forwarded: !bound, to: self.seat.get_pointer().and_then(|p| p.current_focus()).filter(|_| !bound), ..Default::default() });
        if !bound {
            let p = self.seat.get_pointer().expect("pointer");
            p.gesture_swipe_begin(self, &GestureSwipeBeginEvent { serial: SERIAL_COUNTER.next_serial(), time, fingers });
        }
    }

    pub fn swipe_update(&mut self, delta: Point<f64, Logical>, time: u32) {
        let Some(mut g) = self.gesture.take() else { return };
        if g.forwarded {
            if g.to.is_none() {
                self.gesture = Some(g);
                return;
            }
            self.gesture = Some(g);
            let p = self.seat.get_pointer().expect("pointer");
            p.gesture_swipe_update(self, &GestureSwipeUpdateEvent { time, delta });
            return;
        }
        g.moved += delta;
        if g.locked.is_none() && (g.moved.x.abs() >= self.gesture_cfg.lock_threshold || g.moved.y.abs() >= self.gesture_cfg.lock_threshold) {
            let d = if g.moved.x.abs() >= g.moved.y.abs() { if g.moved.x < 0.0 { "left" } else { "right" } } else if g.moved.y < 0.0 { "up" } else { "down" };
            g.locked = Some(d);
            let horiz = matches!(d, "left" | "right");
            g.taken = self.gesture_cfg.find(g.fingers, |dir| dir == d || dir == "swipe" || (dir == "horizontal" && horiz) || (dir == "vertical" && !horiz)).cloned();
            // move: the focused window follows the fingers from here
            if g.taken.as_ref().is_some_and(|t| t.action == "move") {
                if let Some(w) = self.focused.clone() {
                    self.drag_start_on(&w, false, 0);
                    g.drag_at = Some(self.pointer_at);
                }
            }
        }
        if let Some(at) = g.drag_at.as_mut() {
            *at += delta;
            let p = *at;
            self.drag_update(p);
        }
        self.gesture = Some(g);
    }

    pub fn swipe_end(&mut self, cancelled: bool, time: u32) {
        let Some(g) = self.gesture.take() else { return };
        if g.forwarded {
            if g.to.is_none() {
                return;
            }
            let p = self.seat.get_pointer().expect("pointer");
            p.gesture_swipe_end(self, &GestureSwipeEndEvent { serial: SERIAL_COUNTER.next_serial(), time, cancelled });
            return;
        }
        if g.drag_at.is_some() {
            self.drag_end();
            return;
        }
        let Some(t) = g.taken.filter(|_| !cancelled) else { return };
        let along = if matches!(g.locked, Some("left" | "right")) { g.moved.x } else { g.moved.y };
        match t.action.as_str() {
            "workspace" => {
                // far enough (a share of the swipe distance): the next workspace that way
                if along.abs() < self.gesture_cfg.distance * self.gesture_cfg.cancel_ratio {
                    return;
                }
                let forward = (along < 0.0) == self.gesture_cfg.invert;
                // the next one this screen can show: one the other screen shows is skipped (the swipe
                // went there and took the focus to the other screen)
                let here = self.output.as_ref().map(|o| o.name());
                let elsewhere = |k: i32| self.shown_on.iter().any(|(o, v)| *v == k && Some(o) != here.as_ref());
                let mut n = self.active_ws;
                loop {
                    n = if forward { n + 1 } else { n - 1 };
                    if n < 1 {
                        return;
                    }
                    if !elsewhere(n) {
                        break;
                    }
                }
                if !self.gesture_cfg.create_new && !self.workspace_ids().contains(&n) {
                    return;
                }
                self.switch_workspace(n);
            }
            other => {
                // a short flick does not count
                if along.abs() >= 30.0 {
                    self.gesture_action(other, &t.args);
                }
            }
        }
    }

    pub fn pinch_begin(&mut self, fingers: u32, time: u32) {
        self.activity(false);
        let bound = self.gesture_cfg.gestures.iter().any(|g| g.fingers == fingers && g.dir.starts_with("pinch"));
        self.gesture = Some(Active { fingers, scale: 1.0, forwarded: !bound, pinch: true, to: self.seat.get_pointer().and_then(|p| p.current_focus()).filter(|_| !bound), ..Default::default() });
        if !bound {
            let p = self.seat.get_pointer().expect("pointer");
            p.gesture_pinch_begin(self, &GesturePinchBeginEvent { serial: SERIAL_COUNTER.next_serial(), time, fingers });
        }
    }

    pub fn pinch_update(&mut self, delta: Point<f64, Logical>, scale: f64, rotation: f64, time: u32) {
        let Some(mut g) = self.gesture.take() else { return };
        g.scale = scale;
        let fwd = g.forwarded && g.to.is_some();
        self.gesture = Some(g);
        if fwd {
            let p = self.seat.get_pointer().expect("pointer");
            p.gesture_pinch_update(self, &GesturePinchUpdateEvent { time, delta, scale, rotation });
        }
    }

    pub fn pinch_end(&mut self, cancelled: bool, time: u32) {
        let Some(g) = self.gesture.take() else { return };
        if g.forwarded {
            if g.to.is_none() {
                return;
            }
            let p = self.seat.get_pointer().expect("pointer");
            p.gesture_pinch_end(self, &GesturePinchEndEvent { serial: SERIAL_COUNTER.next_serial(), time, cancelled });
            return;
        }
        // in or out, by enough (a tenth of the size)
        if cancelled || (g.scale - 1.0).abs() < 0.1 {
            return;
        }
        let way = if g.scale < 1.0 { "pinchin" } else { "pinchout" };
        if let Some(t) = self.gesture_cfg.find(g.fingers, |dir| dir == way || dir == "pinch").cloned() {
            self.gesture_action(&t.action, &t.args);
        }
    }

    pub fn hold_begin(&mut self, fingers: u32, time: u32) {
        let p = self.seat.get_pointer().expect("pointer");
        self.hold_to = p.current_focus();
        p.gesture_hold_begin(self, &GestureHoldBeginEvent { serial: SERIAL_COUNTER.next_serial(), time, fingers });
    }

    pub fn hold_end(&mut self, cancelled: bool, time: u32) {
        // its begin went to a surface the pointer has left since: ended there already
        if self.hold_to.take().is_none() {
            return;
        }
        let p = self.seat.get_pointer().expect("pointer");
        p.gesture_hold_end(self, &GestureHoldEndEvent { serial: SERIAL_COUNTER.next_serial(), time, cancelled });
    }

    /// The pointer goes to `to`: a gesture begun on another surface ends there now, cancelled.
    /// Without it that program never hears the end, waits for it, and stops scrolling (Zen, Qt)
    /// until it is restarted.
    pub fn gestures_leaving(&mut self, to: Option<&smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>, time: u32) {
        let p = self.seat.get_pointer().expect("pointer");
        if self.hold_to.as_ref().is_some_and(|s| Some(s) != to) {
            eprintln!("loom: the pointer left a surface during a hold: its end sent there, cancelled");
            self.hold_to = None;
            p.gesture_hold_end(self, &GestureHoldEndEvent { serial: SERIAL_COUNTER.next_serial(), time, cancelled: true });
        }
        let Some(g) = self.gesture.as_mut() else { return };
        if g.to.as_ref().is_none_or(|s| Some(s) == to) {
            return;
        }
        g.to = None;
        let pinch = g.pinch;
        eprintln!("loom: the pointer left a surface during a {}: its end sent there, cancelled", if pinch { "pinch" } else { "swipe" });
        if pinch {
            p.gesture_pinch_end(self, &GesturePinchEndEvent { serial: SERIAL_COUNTER.next_serial(), time, cancelled: true });
        } else {
            p.gesture_swipe_end(self, &GestureSwipeEndEvent { serial: SERIAL_COUNTER.next_serial(), time, cancelled: true });
        }
    }

    /// A gesture line's action, once the gesture is done.
    fn gesture_action(&mut self, action: &str, args: &str) {
        let d = match action {
            "dispatcher" => args.to_string(),
            "float" => "togglefloating".into(),
            "close" => "killactive".into(),
            "fullscreen" => "fullscreen".into(),
            other => {
                eprintln!("loom: gesture action {other} is not known");
                return;
            }
        };
        let r = self.hypr_dispatch(&d);
        if r != "ok" {
            eprintln!("loom: gesture `{d}`: {r}");
        }
    }
}

delegate_pointer_gestures!(Loom);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_users_gesture_lines() {
        assert_eq!(parse("3, swipe, move,"), Some(Gesture { fingers: 3, dir: "swipe".into(), action: "move".into(), args: String::new() }));
        assert_eq!(parse("4, up, dispatcher, global, quickshell:overviewWorkspacesToggle").unwrap().args, "global quickshell:overviewWorkspacesToggle");
        assert_eq!(parse("3, pinch, float").unwrap().action, "float");
        let c = Config { gestures: ["4, horizontal, workspace", "4, up, dispatcher, global, a", "3, swipe, move"].iter().filter_map(|l| parse(l)).collect(), distance: 700.0, cancel_ratio: 0.2, lock_threshold: 10.0, create_new: true, invert: true };
        assert_eq!(c.find(4, |d| d == "up" || d == "swipe" || d == "vertical").unwrap().action, "dispatcher");
        assert_eq!(c.find(4, |d| d == "left" || d == "swipe" || d == "horizontal").unwrap().action, "workspace");
        assert!(c.find(2, |_| true).is_none());
    }
}
