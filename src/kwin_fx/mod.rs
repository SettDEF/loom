//! KDE's window animations: KWin's scripted effects (/usr/share/kwin/effects/*, the user's
//! ~/.local/share/kwin/effects, KDE Store ones) run unchanged in QuickJS, against the part of
//! KWin's effect API they use (prelude.js). loom sends them window events; their animate()/set()
//! calls become native animations here (scale, opacity, translation, size, position with Qt's
//! easing curves), applied as each window is drawn. A closing window is drawn from a snapshot
//! while its close animation runs.
//!
//! Which effects: the ones KDE enables by default (metadata EnabledByDefault), changed by
//! kwinrc [Plugins] <name>Enabled, as KDE does.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use rquickjs::{Context, Function, Runtime};
use serde_json::Value;

const PRELUDE: &str = include_str!("prelude.js");

/// One animated attribute.
#[derive(Clone, Debug)]
pub struct Attr {
    pub kind: i64,
    pub from: (f64, f64),
    pub to: (f64, f64),
    pub curve: i64,
    pub start: Instant,
    pub dur: Duration,
}

/// One animate()/set() call: its attributes, on one window.
#[derive(Clone, Debug)]
pub struct Anim {
    pub addr: u64,
    pub attrs: Vec<Attr>,
    /// set(): stays at its target when done (until cancelled)
    pub keep: bool,
    /// redirected: runs back towards its source
    pub backward: bool,
    pub effect: usize,
}

/// What the effects do to a window this frame (applied around its middle, size from its corner).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub scale: (f64, f64),
    pub opacity: f64,
    pub translate: (f64, f64),
}

impl Default for Shape {
    fn default() -> Self {
        Shape { scale: (1.0, 1.0), opacity: 1.0, translate: (0.0, 0.0) }
    }
}

#[derive(Default)]
struct Shared {
    anims: HashMap<u64, Anim>,
    next: u64,
    /// the effect whose code runs now (its animate() calls are its own)
    current: usize,
    /// the window sizes, for Size/Position animations without a `to` (their own geometry)
    geometry: HashMap<u64, (f64, f64, f64, f64)>,
    display: (f64, f64),
}

pub struct Fx {
    _rt: Runtime,
    effects: Vec<(String, Context)>,
    shared: Rc<RefCell<Shared>>,
}

/// Qt's easing curves (QEasingCurve::Type), t in 0..1.
pub fn ease(curve: i64, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let inv = |f: &dyn Fn(f64) -> f64, t: f64| 1.0 - f(1.0 - t);
    let inout = |f: &dyn Fn(f64) -> f64, t: f64| if t < 0.5 { f(2.0 * t) / 2.0 } else { 1.0 - f(2.0 - 2.0 * t) / 2.0 };
    let outin = |f: &dyn Fn(f64) -> f64, t: f64| if t < 0.5 { inv(f, 2.0 * t) / 2.0 } else { 0.5 + f(2.0 * t - 1.0) / 2.0 };
    let family = |n: i64| -> Box<dyn Fn(f64) -> f64> {
        match n {
            0 => Box::new(|t: f64| t * t),
            1 => Box::new(|t: f64| t * t * t),
            2 => Box::new(|t: f64| t.powi(4)),
            3 => Box::new(|t: f64| t.powi(5)),
            4 => Box::new(|t: f64| 1.0 - (t * std::f64::consts::FRAC_PI_2).cos()),
            5 => Box::new(|t: f64| if t <= 0.0 { 0.0 } else { 2f64.powf(10.0 * (t - 1.0)) - 0.001 }),
            6 => Box::new(|t: f64| 1.0 - (1.0 - t * t).max(0.0).sqrt()),
            7 => Box::new(|t: f64| if t <= 0.0 || t >= 1.0 { t } else { -(2f64.powf(10.0 * (t - 1.0))) * ((t - 1.075) * std::f64::consts::TAU / 0.3).sin() }),
            8 => Box::new(|t: f64| t * t * (2.70158 * t - 1.70158)),
            _ => Box::new(|t: f64| {
                // bounce (in): the out-bounce mirrored
                let u = 1.0 - t;
                let o = if u < 1.0 / 2.75 { 7.5625 * u * u } else if u < 2.0 / 2.75 { let v = u - 1.5 / 2.75; 7.5625 * v * v + 0.75 } else if u < 2.5 / 2.75 { let v = u - 2.25 / 2.75; 7.5625 * v * v + 0.9375 } else { let v = u - 2.625 / 2.75; 7.5625 * v * v + 0.984375 };
                1.0 - o
            }),
        }
    };
    if curve <= 0 {
        return t;
    }
    // 1..40: families of four (In, Out, InOut, OutIn): Quad Cubic Quart Quint Sine Expo Circ Elastic Back Bounce
    let fam = family((curve - 1) / 4);
    match (curve - 1) % 4 {
        0 => fam(t),
        1 => inv(&*fam, t),
        2 => inout(&*fam, t),
        _ => outin(&*fam, t),
    }
}

/// A from/to value: a number, or {value1, value2}; none = the attribute's resting value.
fn pair(v: &Value) -> Option<(f64, f64)> {
    match v {
        Value::Number(n) => n.as_f64().map(|x| (x, x)),
        Value::Object(o) => {
            let a = o.get("value1").and_then(|x| x.as_f64())?;
            Some((a, o.get("value2").and_then(|x| x.as_f64()).unwrap_or(a)))
        }
        _ => None,
    }
}

/// Where an attribute rests (no animation): scale and opacity 1, translation 0, size and
/// position the window's own.
fn resting(kind: i64, geo: (f64, f64, f64, f64)) -> (f64, f64) {
    match kind {
        0 | 1 | 2 | 3 => (1.0, 1.0),
        5 => (geo.0, geo.1),
        6 => (geo.2, geo.3),
        _ => (0.0, 0.0),
    }
}

impl Shared {
    fn animate(&mut self, json: &str) -> u64 {
        let Ok(v) = serde_json::from_str::<Value>(json) else { return 0 };
        let addr = v["window"].as_u64().unwrap_or(0);
        let geo = self.geometry.get(&addr).copied().unwrap_or((0.0, 0.0, 1.0, 1.0));
        let now = Instant::now();
        let dur = v["duration"].as_f64().unwrap_or(200.0);
        let delay = v["delay"].as_f64().unwrap_or(0.0);
        let curve = v["curve"].as_i64().unwrap_or(0);
        let attrs = v["animations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                let kind = a["type"].as_i64()?;
                let rest = resting(kind, geo);
                let d = a["duration"].as_f64().unwrap_or(dur);
                let dl = a["delay"].as_f64().unwrap_or(delay);
                Some(Attr { kind, from: pair(&a["from"]).unwrap_or(rest), to: pair(&a["to"]).unwrap_or(rest), curve: a["curve"].as_i64().unwrap_or(curve), start: now + Duration::from_millis(dl as u64), dur: Duration::from_millis(d.max(1.0) as u64) })
            })
            .collect::<Vec<_>>();
        if attrs.is_empty() || addr == 0 {
            return 0;
        }
        self.next += 1;
        let id = self.next;
        self.anims.insert(id, Anim { addr, attrs, keep: v["keep"].as_bool().unwrap_or(false), backward: false, effect: self.current });
        id
    }
}

/// The effect folders: KDE's, then the user's (a user one of the same name wins).
fn effect_dirs() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut by_name: HashMap<String, PathBuf> = HashMap::new();
    for root in [PathBuf::from("/usr/share/kwin/effects"), home.join(".local/share/kwin/effects")] {
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            if e.path().join("contents/code/main.js").exists() {
                by_name.insert(e.file_name().to_string_lossy().to_string(), e.path());
            }
        }
    }
    let mut v: Vec<PathBuf> = by_name.into_values().collect();
    v.sort();
    v
}

/// KDE's choice: EnabledByDefault, changed by kwinrc [Plugins] <name>Enabled.
fn enabled(dir: &Path, name: &str) -> bool {
    let meta: Value = std::fs::read_to_string(dir.join("metadata.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let default = meta["KPlugin"]["EnabledByDefault"].as_bool().unwrap_or(false);
    let rc = std::fs::read_to_string(PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/kwinrc")).unwrap_or_default();
    let mut in_plugins = false;
    for l in rc.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_plugins = l == "[Plugins]";
            continue;
        }
        if in_plugins {
            if let Some(v) = l.strip_prefix(&format!("{name}Enabled=")) {
                return v.trim() == "true";
            }
        }
    }
    default
}

impl Fx {
    /// The enabled effects, loaded (`only`: just these, for tests and init.lua).
    pub fn load(only: Option<&[String]>) -> Fx {
        let rt = Runtime::new().expect("quickjs");
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut effects = Vec::new();
        for dir in effect_dirs() {
            let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let on = match only {
                Some(list) => list.iter().any(|n| n == &name),
                None => enabled(&dir, &name),
            };
            // login/logout/session effects belong to Plasma's own screens
            if !on || matches!(name.as_str(), "login" | "logout" | "sessionquit") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(dir.join("contents/code/main.js")) else { continue };
            let Ok(ctx) = Context::full(&rt) else { continue };
            let index = effects.len();
            let sh = shared.clone();
            let ok = ctx.with(|ctx| -> rquickjs::Result<()> {
                let g = ctx.globals();
                let s1 = sh.clone();
                g.set("__loom_animate", Function::new(ctx.clone(), move |json: String| -> f64 {
                    let mut s = s1.borrow_mut();
                    s.current = index;
                    s.animate(&json) as f64
                })?)?;
                let s2 = sh.clone();
                g.set("__loom_cancel", Function::new(ctx.clone(), move |id: f64| -> bool { s2.borrow_mut().anims.remove(&(id as u64)).is_some() })?)?;
                let s3 = sh.clone();
                g.set("__loom_complete", Function::new(ctx.clone(), move |id: f64| -> bool {
                    let mut s = s3.borrow_mut();
                    match s.anims.get_mut(&(id as u64)) {
                        Some(a) => {
                            for at in &mut a.attrs {
                                at.start = Instant::now() - at.dur;
                            }
                            true
                        }
                        None => false,
                    }
                })?)?;
                let s4 = sh.clone();
                g.set("__loom_redirect", Function::new(ctx.clone(), move |id: f64, dir: f64, _term: f64| -> bool {
                    let mut s = s4.borrow_mut();
                    let Some(a) = s.anims.get_mut(&(id as u64)) else { return false };
                    let back = dir as i64 == 1;
                    if a.backward != back {
                        // the same path the other way, from where it is now
                        let now = Instant::now();
                        for at in &mut a.attrs {
                            let done = now.saturating_duration_since(at.start).min(at.dur);
                            at.start = now - (at.dur - done);
                        }
                        a.backward = back;
                    }
                    true
                })?)?;
                let s5 = sh.clone();
                g.set("__loom_retarget", Function::new(ctx.clone(), move |id: f64, to: String, remaining: f64| -> bool {
                    let mut s = s5.borrow_mut();
                    let Some(a) = s.anims.get_mut(&(id as u64)) else { return false };
                    let Some(t) = serde_json::from_str::<Value>(&to).ok().and_then(|v| pair(&v)) else { return false };
                    let now = Instant::now();
                    for at in &mut a.attrs {
                        let k = ease(at.curve, now.saturating_duration_since(at.start).as_secs_f64() / at.dur.as_secs_f64());
                        at.from = (at.from.0 + (at.to.0 - at.from.0) * k, at.from.1 + (at.to.1 - at.from.1) * k);
                        at.to = t;
                        at.start = now;
                        if remaining >= 0.0 {
                            at.dur = Duration::from_millis(remaining.max(1.0) as u64);
                        }
                    }
                    true
                })?)?;
                g.set("__loom_log", Function::new(ctx.clone(), move |m: String| eprintln!("loom: kwin effect: {m}"))?)?;
                let d = sh.borrow().display;
                g.set("__loom_display", vec![d.0, d.1])?;
                ctx.eval::<(), _>(PRELUDE)?;
                ctx.eval::<(), _>(src.as_bytes().to_vec())?;
                Ok(())
            });
            match ok {
                Ok(()) => effects.push((name, ctx)),
                Err(e) => {
                    let detail = ctx.with(|c| c.catch().as_exception().map(|x| format!("{x}")).unwrap_or_default());
                    eprintln!("loom: kwin effect {name} did not load: {e} {detail}");
                }
            }
        }
        eprintln!("loom: kwin effects: {}", effects.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "));
        Fx { _rt: rt, effects, shared }
    }

    pub fn names(&self) -> Vec<String> {
        self.effects.iter().map(|(n, _)| n.clone()).collect()
    }

    /// A window event to every effect: `window` its properties, `stacking` everything's, `extra`
    /// further arguments (e.g. maximized horizontally/vertically).
    pub fn event(&self, name: &str, window: Option<&Value>, stacking: &[Value], active: Option<u64>, extra: &[Value]) {
        let data = serde_json::json!({ "window": window, "stacking": stacking, "active": active }).to_string();
        let extra = Value::Array(extra.to_vec()).to_string();
        for (i, (ename, ctx)) in self.effects.iter().enumerate() {
            self.shared.borrow_mut().current = i;
            let r = ctx.with(|ctx| -> rquickjs::Result<()> {
                let lo: rquickjs::Object = ctx.globals().get("__loom")?;
                let f: Function = lo.get("event")?;
                f.call::<_, ()>((name, data.as_str(), extra.as_str()))
            });
            if let Err(e) = r {
                let detail = ctx.with(|c| c.catch().as_exception().map(|x| format!("{x}")).unwrap_or_default());
                eprintln!("loom: kwin effect {ename}: {name}: {e} {detail}");
            }
        }
    }

    /// The window's geometry (logical), for animations without a `to`; the display's size.
    pub fn set_geometry(&self, addr: u64, g: (f64, f64, f64, f64)) {
        self.shared.borrow_mut().geometry.insert(addr, g);
    }
    pub fn set_display(&self, w: f64, h: f64) {
        self.shared.borrow_mut().display = (w, h);
    }

    /// Any animation running (frames must be drawn).
    pub fn animating(&self, now: Instant) -> bool {
        self.shared.borrow().anims.values().any(|a| !a.keep && a.attrs.iter().any(|at| now < at.start + at.dur))
    }

    /// Any animation on this window (a closed one stays drawn while it runs).
    pub fn animates(&self, addr: u64) -> bool {
        self.shared.borrow().anims.values().any(|a| a.addr == addr)
    }

    /// What the effects do to window `addr` now (`geo`: its logical x, y, w, h).
    pub fn shape(&self, addr: u64, geo: (f64, f64, f64, f64), now: Instant) -> Shape {
        let s = self.shared.borrow();
        let mut out = Shape::default();
        for a in s.anims.values().filter(|a| a.addr == addr) {
            for at in &a.attrs {
                let mut t = if now < at.start { 0.0 } else { (now - at.start).as_secs_f64() / at.dur.as_secs_f64() };
                if a.backward {
                    t = 1.0 - t;
                }
                let k = ease(at.curve, t);
                let v = (at.from.0 + (at.to.0 - at.from.0) * k, at.from.1 + (at.to.1 - at.from.1) * k);
                match at.kind {
                    0 => out.opacity *= v.0,
                    3 => out.scale = (out.scale.0 * v.0, out.scale.1 * v.1),
                    7 => out.translate = (out.translate.0 + v.0, out.translate.1 + v.1),
                    // Size: to that size from the window's corner; Position: moved to that point
                    6 => {
                        if geo.2 > 0.0 && geo.3 > 0.0 {
                            let (sx, sy) = (v.0 / geo.2, v.1 / geo.3);
                            out.translate = (out.translate.0 - geo.2 * (1.0 - sx) / 2.0, out.translate.1 - geo.3 * (1.0 - sy) / 2.0);
                            out.scale = (out.scale.0 * sx, out.scale.1 * sy);
                        }
                    }
                    5 => out.translate = (out.translate.0 + v.0 - geo.0, out.translate.1 + v.1 - geo.1),
                    _ => {}
                }
            }
        }
        out
    }

    /// Finished animations ended (their effects told, animationEnded); set() ones stay.
    pub fn tick(&self, now: Instant) {
        let done: Vec<(u64, u64, usize)> = self
            .shared
            .borrow()
            .anims
            .iter()
            .filter(|(_, a)| !a.keep && a.attrs.iter().all(|at| now >= at.start + at.dur))
            .map(|(id, a)| (*id, a.addr, a.effect))
            .collect();
        for (id, addr, eff) in done {
            self.shared.borrow_mut().anims.remove(&id);
            if let Some((_, ctx)) = self.effects.get(eff) {
                let _ = ctx.with(|ctx| -> rquickjs::Result<()> {
                    let lo: rquickjs::Object = ctx.globals().get("__loom")?;
                    let f: Function = lo.get("ended")?;
                    f.call::<_, ()>((addr as f64, id as f64))
                });
            }
        }
    }

    /// A window gone for good (its close animation over): the effects forget it.
    pub fn forget(&self, addr: u64) {
        self.shared.borrow_mut().anims.retain(|_, a| a.addr != addr);
        self.shared.borrow_mut().geometry.remove(&addr);
        for (_, ctx) in &self.effects {
            let _ = ctx.with(|ctx| -> rquickjs::Result<()> {
                let lo: rquickjs::Object = ctx.globals().get("__loom")?;
                let f: Function = lo.get("forget")?;
                f.call::<_, ()>((addr as f64,))
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_curves_start_and_end_right() {
        for c in [0, 1, 2, 5, 6, 7, 21, 26, 33, 34, 38] {
            assert!(ease(c, 0.0).abs() < 0.01, "curve {c} starts at 0");
            assert!((ease(c, 1.0) - 1.0).abs() < 0.01, "curve {c} ends at 1");
        }
        assert!(ease(6, 0.5) > 0.5, "OutCubic is ahead at the middle");
        assert!(ease(5, 0.5) < 0.5, "InCubic is behind at the middle");
    }

    #[test]
    fn kde_scale_effect_animates_an_opening_window() {
        if !Path::new("/usr/share/kwin/effects/scale/contents/code/main.js").exists() {
            return;
        }
        let fx = Fx::load(Some(&["scale".to_string()]));
        assert_eq!(fx.names(), vec!["scale".to_string()]);
        let w = serde_json::json!({ "addr": 7, "windowClass": "alacritty Alacritty", "caption": "t", "normalWindow": true, "dialog": false, "popupWindow": false, "managed": true, "visible": true, "hasDecoration": false, "geometry": { "x": 0, "y": 0, "width": 800, "height": 600 } });
        fx.set_geometry(7, (0.0, 0.0, 800.0, 600.0));
        fx.event("windowAdded", Some(&w), std::slice::from_ref(&w), Some(7), &[]);
        assert!(fx.animates(7), "the scale effect animated the new window");
        let s = fx.shape(7, (0.0, 0.0, 800.0, 600.0), Instant::now());
        assert!(s.scale.0 < 1.0 && s.opacity < 1.0, "it starts small and see-through: {s:?}");
        let later = fx.shape(7, (0.0, 0.0, 800.0, 600.0), Instant::now() + Duration::from_secs(2));
        assert!((later.scale.0 - 1.0).abs() < 1e-6 && (later.opacity - 1.0).abs() < 1e-6, "and ends at rest: {later:?}");
    }
}

#[cfg(test)]
mod load_all {
    #[test]
    fn kde_defaults_load() {
        let fx = super::Fx::load(None);
        eprintln!("LOADED {:?}", fx.names());
    }
}
