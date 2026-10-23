//! Hyprland's window rules, applied when a window first shows: `windowrule = match:class RE,
//! match:title RE, EFFECT ARGS` (and the older `windowrulev2 = EFFECT, class:RE, title:RE`).
//! Effects: float, tile, center, size, move, pin, no_initial_focus, workspace. Sizes and places
//! may be numbers, `50%` or expressions of monitor_w / monitor_h (`(monitor_w*.60)`). no_blur is
//! read by blur.rs; the rest is not acted on (yet).

use regex::Regex;
use smithay::{desktop::Window, utils::{Logical, Point, Rectangle, Size}};

use crate::state::Loom;

struct Rule {
    class: Option<Regex>,
    title: Option<Regex>,
    effects: Vec<(String, String)>,
}

/// What the rules say for one window (later rules win).
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    pub float: Option<bool>,
    pub center: bool,
    pub size: Option<(String, String)>,
    pub at: Option<(String, String)>,
    pub pin: bool,
    pub no_focus: bool,
    pub workspace: Option<String>,
}

#[derive(Default)]
pub struct Rules(Vec<Rule>);

fn whole(p: &str) -> Option<Regex> {
    // the regex crate has no lookaround (`^(?!.*GIMP)`): such a rule is skipped
    Regex::new(&format!("^(?:{})$", p.trim())).ok()
}

fn on(v: &str) -> bool {
    !matches!(v.trim(), "off" | "false" | "0" | "no")
}

impl Rules {
    pub fn load() -> Rules {
        let mut rules = Vec::new();
        for (k, v) in crate::config::hyprland_lines(&crate::config::hyprland_conf()) {
            if matches!(k.as_str(), "windowrule" | "windowrulev2") {
                rules.extend(parse(&v));
            }
        }
        Rules(rules)
    }

    pub fn applied(&self, class: &str, title: &str) -> Applied {
        let mut a = Applied::default();
        for r in &self.0 {
            if r.class.as_ref().is_some_and(|re| !re.is_match(class)) || r.title.as_ref().is_some_and(|re| !re.is_match(title)) {
                continue;
            }
            for (e, arg) in &r.effects {
                match e.as_str() {
                    "float" => a.float = Some(on(arg)),
                    "tile" => a.float = Some(!on(arg)),
                    "center" => a.center = on(arg),
                    "size" => a.size = arg.split_once(' ').map(|(w, h)| (w.to_string(), h.to_string())),
                    "move" => a.at = arg.split_once(' ').map(|(x, y)| (x.to_string(), y.to_string())),
                    "pin" => a.pin = on(arg),
                    "no_initial_focus" | "noinitialfocus" => a.no_focus = on(arg),
                    "workspace" => a.workspace = Some(arg.trim().to_string()),
                    _ => {}
                }
            }
        }
        a
    }
}

/// One rule line, either syntax. Its expressions keep their spaces inside parentheses.
fn parse(v: &str) -> Option<Rule> {
    let parts = split_top(v);
    let mut r = Rule { class: None, title: None, effects: Vec::new() };
    let mut bare = Vec::new();
    for p in parts {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if let Some(re) = p.strip_prefix("match:class").or_else(|| p.strip_prefix("match:initial_class")).or_else(|| p.strip_prefix("class:")).or_else(|| p.strip_prefix("initialClass:")) {
            r.class = Some(whole(re)?);
        } else if let Some(re) = p.strip_prefix("match:title").or_else(|| p.strip_prefix("match:initial_title")).or_else(|| p.strip_prefix("title:")).or_else(|| p.strip_prefix("initialTitle:")) {
            r.title = Some(whole(re)?);
        } else if p.starts_with("match:") {
            // another matcher (xwayland, float …): loom does not read it, so the rule is not used
            return None;
        } else {
            bare.push(p.to_string());
        }
    }
    // windowrule v1: `float, ^(pavucontrol)$`
    if r.class.is_none() && r.title.is_none() && bare.len() == 2 && bare[1].starts_with('^') {
        r.class = Some(whole(&bare[1])?);
        bare.truncate(1);
    }
    for b in bare {
        let (e, arg) = b.split_once(' ').unwrap_or((&b, "on"));
        r.effects.push((e.to_string(), arg.trim().to_string()));
    }
    (!r.effects.is_empty()).then_some(r)
}

/// `a, (b, c), d` split at the commas outside parentheses.
fn split_top(v: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in v.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out
}

/// A size or place: a number, `N%` of `axis`, or an expression of monitor_w / monitor_h
/// (+ - * / and parentheses).
pub fn eval(s: &str, axis: f64, mon: Size<f64, Logical>) -> Option<f64> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        return p.trim().parse::<f64>().ok().map(|p| axis * p / 100.0);
    }
    let toks: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
    let mut i = 0;
    let v = expr(&toks, &mut i, mon)?;
    (i == toks.len()).then_some(v)
}

fn expr(t: &[char], i: &mut usize, mon: Size<f64, Logical>) -> Option<f64> {
    let mut v = term(t, i, mon)?;
    while let Some(&c) = t.get(*i).filter(|c| matches!(c, '+' | '-')) {
        *i += 1;
        let r = term(t, i, mon)?;
        v = if c == '+' { v + r } else { v - r };
    }
    Some(v)
}

fn term(t: &[char], i: &mut usize, mon: Size<f64, Logical>) -> Option<f64> {
    let mut v = atom(t, i, mon)?;
    while let Some(&c) = t.get(*i).filter(|c| matches!(c, '*' | '/')) {
        *i += 1;
        let r = atom(t, i, mon)?;
        v = if c == '*' { v * r } else { v / r };
    }
    Some(v)
}

fn atom(t: &[char], i: &mut usize, mon: Size<f64, Logical>) -> Option<f64> {
    match t.get(*i)? {
        '(' => {
            *i += 1;
            let v = expr(t, i, mon)?;
            (t.get(*i) == Some(&')')).then(|| *i += 1)?;
            Some(v)
        }
        '-' => {
            *i += 1;
            atom(t, i, mon).map(|v| -v)
        }
        c if c.is_ascii_digit() || *c == '.' => {
            let start = *i;
            while t.get(*i).is_some_and(|c| c.is_ascii_digit() || *c == '.') {
                *i += 1;
            }
            t[start..*i].iter().collect::<String>().parse().ok()
        }
        _ => {
            let start = *i;
            while t.get(*i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') {
                *i += 1;
            }
            match t[start..*i].iter().collect::<String>().as_str() {
                "monitor_w" => Some(mon.w),
                "monitor_h" => Some(mon.h),
                _ => None,
            }
        }
    }
}

impl Loom {
    /// The rules for a window that is about to show (its class and title known): floating,
    /// size and place, pinned, its workspace, no focus.
    pub fn apply_window_rules(&mut self, w: &Window) {
        let a = self.win_rules.applied(&self.class_of(w), &self.title_of(w));
        if a == Applied::default() {
            return;
        }
        let Some(o) = self.output.clone() else { return };
        let Some(screen) = self.space.output_geometry(&o) else { return };
        let mon = screen.size.to_f64();
        let Some(i) = self.managed.iter().position(|m| &m.window == w) else { return };
        if let Some(f) = a.float {
            self.managed[i].floating = f;
        }
        if a.pin && self.managed[i].floating {
            self.managed[i].pinned = true;
        }
        if let Some(ws) = a.workspace.as_deref().and_then(|s| s.split_whitespace().next()?.parse::<i32>().ok()) {
            self.managed[i].ws = ws;
            if !self.is_shown(ws) {
                self.space.unmap_elem(w);
            }
        }
        if self.managed[i].floating {
            // its size: the rule's, else what it asks for (known with its first frame)
            let size = a.size.as_ref().and_then(|(sw, sh)| Some(Size::<i32, Logical>::from((eval(sw, mon.w, mon)?.round() as i32, eval(sh, mon.h, mon)?.round() as i32))));
            // the rule's size, else none: it picks its own (not the tiled size it was about to get)
            if let Some(t) = w.toplevel() {
                t.with_pending_state(|s| s.size = size);
            }
            if let (Some(sz), Some(x)) = (size, w.x11_surface()) {
                let _ = x.configure(Rectangle::new(x.geometry().loc, sz));
            }
            let sz = size.unwrap_or_else(|| w.geometry().size);
            let place = |x: &str, y: &str| -> Option<Point<i32, Logical>> { Some(Point::from((eval(x, mon.w, mon)?.round() as i32, eval(y, mon.h, mon)?.round() as i32)) + screen.loc) };
            let at: Option<Point<i32, Logical>> = match &a.at {
                Some((x, y)) => place(x, y),
                None if a.center || a.float == Some(true) => {
                    // centred (Hyprland's default for a floating window); before its first frame
                    // its size may be unknown: centred again then
                    if sz.w <= 0 {
                        self.center_pending.insert(self.managed[i].addr);
                    }
                    Some(screen.loc + Point::from(((screen.size.w - sz.w.max(0)) / 2, (screen.size.h - sz.h.max(0)) / 2)))
                }
                None => None,
            };
            if let Some(at) = at {
                if self.is_shown(self.managed[i].ws) {
                    self.space.map_element(w.clone(), at, true);
                }
                if let Some(a) = self.anims.get_mut(&self.managed[i].addr) {
                    a.jump_to(at.x as f64, at.y as f64);
                }
            }
        }
        if a.no_focus && self.focused.as_ref() == Some(w) {
            // the window that had the keyboard keeps it
            let ws = self.managed[i].ws;
            if let Some(prev) = self.managed.iter().rev().find(|m| m.ws == ws && &m.window != w).map(|m| m.window.clone()) {
                self.focus_window(&prev);
            }
        }
        self.relayout();
    }

    /// A floating window centred before its size was known: centred now it has a frame.
    pub fn center_if_pending(&mut self, w: &Window) {
        let Some(addr) = self.managed_of(w).map(|m| m.addr) else { return };
        if !self.center_pending.remove(&addr) {
            return;
        }
        let (Some(o), sz) = (self.output.clone(), w.geometry().size) else { return };
        let Some(screen) = self.space.output_geometry(&o) else { return };
        let at = screen.loc + Point::from(((screen.size.w - sz.w) / 2, (screen.size.h - sz.h) / 2));
        self.space.map_element(w.clone(), at, true);
        if let Some(a) = self.anims.get_mut(&addr) {
            a.jump_to(at.x as f64, at.y as f64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_users_rules() {
        let r = Rules(["match:title ^(Open File)(.*)$, float on", "match:class ^(pavucontrol)$, size (monitor_w*.45) (monitor_h*.45)", "match:class ^(pavucontrol)$, center on", "float, class:^(kitty)$", "match:class .*, no_blur on", "match:title ^(?!.*GIMP).*$, float on"].iter().filter_map(|l| parse(l)).collect());
        assert_eq!(r.0.len(), 5, "the lookahead one is skipped");
        assert_eq!(r.applied("org.kde.dolphin", "Open File — Dolphin").float, Some(true));
        let p = r.applied("pavucontrol", "Volume Control");
        assert!(p.center && p.size == Some(("(monitor_w*.45)".into(), "(monitor_h*.45)".into())));
        assert_eq!(r.applied("kitty", "x").float, Some(true));
        assert_eq!(r.applied("zash", "x"), Applied::default());
        let mon: Size<f64, Logical> = (1600.0, 1000.0).into();
        assert_eq!(eval("(monitor_w*.45)", 1600.0, mon), Some(720.0));
        assert_eq!(eval("50%", 1000.0, mon), Some(500.0));
        assert_eq!(eval("monitor_w-20", 1600.0, mon), Some(1580.0));
        assert_eq!(eval("800", 1600.0, mon), Some(800.0));
    }
}
