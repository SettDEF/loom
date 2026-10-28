//! The user's AwesomeWM layouts (hypr/lua/awesome_layouts.lua, awful.layout.suit), the same
//! geometry: tile tileleft tilebottom tiletop fair fairh spiral dwindle max cornernw cornerne
//! cornersw cornerse, each workspace with its own suit, mwfact, nmaster, ncol. Driven by the same
//! layout messages (`hyprctl dispatch layoutmsg suit fair | next | prev | mwfact +0.05 | nmaster
//! +1 | ncol +1 | swapmaster | reset | state`). The state is read from Hyprland's file
//! (~/.cache/hypr-awesome.json) and kept in loom's own (~/.cache/loom-awesome.json), so trying
//! loom never changes Hyprland's.

use std::collections::BTreeMap;

use smithay::utils::{Logical, Rectangle};

pub const SUITS: [&str; 14] = ["tile", "center", "tileleft", "tilebottom", "tiletop", "fair", "fairh", "spiral", "dwindle", "max", "cornernw", "cornerne", "cornersw", "cornerse"];

/// A workspace's layout, as an Awesome tag has it.
#[derive(Clone, Debug, PartialEq)]
pub struct Suit {
    pub suit: String,
    pub mwfact: f64,
    pub nmaster: usize,
    pub ncol: usize,
    /// center: the room between the master and the screen's left and right edge (px; top and bottom 60 % of it)
    pub pad: i32,
}

impl Default for Suit {
    fn default() -> Self {
        Suit { suit: "tile".into(), mwfact: 0.5, nmaster: 1, ncol: 1, pad: 160 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct B {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn b(x: i32, y: i32, w: i32, h: i32) -> B {
    B { x, y, w, h }
}

/// `area` cut into `n` equal slices, one under another (`vertical`) or side by side.
fn slices(a: B, n: usize, vertical: bool) -> Vec<B> {
    let n = n.max(1) as i64;
    (0..n)
        .map(|i| {
            if vertical {
                let (y0, y1) = (a.y as i64 + a.h as i64 * i / n, a.y as i64 + a.h as i64 * (i + 1) / n);
                b(a.x, y0 as i32, a.w, (y1 - y0) as i32)
            } else {
                let (x0, x1) = (a.x as i64 + a.w as i64 * i / n, a.x as i64 + a.w as i64 * (i + 1) / n);
                b(x0 as i32, a.y, (x1 - x0) as i32, a.h)
            }
        })
        .collect()
}

/// tile and its orientations: `orient` is where the masters sit.
fn tile(n: usize, a: B, s: &Suit, orient: char) -> Vec<B> {
    let horizontal = orient == 'l' || orient == 'r';
    let nmaster = s.nmaster.min(n);
    let nother = n - nmaster;
    let mut out = Vec::new();
    let (mut master, mut other) = (Some(a), None);
    if nmaster > 0 && nother > 0 {
        let size = ((if horizontal { a.w } else { a.h }) as f64 * s.mwfact).floor() as i32;
        let (m, o) = match orient {
            'l' => (b(a.x, a.y, size, a.h), b(a.x + size, a.y, a.w - size, a.h)),
            'r' => (b(a.x + a.w - size, a.y, size, a.h), b(a.x, a.y, a.w - size, a.h)),
            't' => (b(a.x, a.y, a.w, size), b(a.x, a.y + size, a.w, a.h - size)),
            _ => (b(a.x, a.y + a.h - size, a.w, size), b(a.x, a.y, a.w, a.h - size)),
        };
        master = Some(m);
        other = Some(o);
    } else if nmaster == 0 {
        master = None;
        other = Some(a);
    }
    if let (Some(m), true) = (master, nmaster > 0) {
        out.extend(slices(m, nmaster, horizontal));
    }
    if let (Some(o), true) = (other, nother > 0) {
        // ncol columns; the later ones take the remainder, so the one by the masters stays tidy
        let ncol = s.ncol.clamp(1, nother);
        let mut cols = slices(o, ncol, !horizontal);
        if orient == 'r' || orient == 'b' {
            cols.reverse();
        }
        let (base, extra) = (nother / ncol, nother % ncol);
        for (c, col) in cols.into_iter().enumerate() {
            let count = base + usize::from(c + 1 > ncol - extra);
            out.extend(slices(col, count, horizontal));
        }
    }
    out
}

/// fair: a grid, the last line stretched.
/// center: the master floats in the middle with `pad` of room to the left and right edge (60 % of it above and
/// below), drawn over the rest, which share the whole screen behind it (fair). The window manager raises the
/// master and puts a new window first, so it becomes the master. Super+minus / Super+plus: layoutmsg pad -40 / +40.
fn center(n: usize, a: B, s: &Suit) -> Vec<B> {
    let pad = s.pad.clamp(0, a.w / 3);
    let vpad = (pad as f64 * 0.6) as i32;
    let mut out = vec![b(a.x + pad, a.y + vpad, a.w - 2 * pad, a.h - 2 * vpad)];
    match n.saturating_sub(1) {
        0 => {}
        1 => out.push(a),
        k => out.extend(fair(k, a, false)),
    }
    out
}

fn fair(n: usize, a: B, horizontal: bool) -> Vec<B> {
    if n == 2 {
        return slices(a, 2, horizontal);
    }
    let rows = (n as f64).sqrt().ceil() as usize;
    let cols = n.div_ceil(rows);
    let lines = slices(a, cols, horizontal);
    let mut out = Vec::new();
    let mut placed = 0;
    for (l, line) in lines.into_iter().enumerate() {
        let remaining = cols - l;
        let count = (n - placed).div_ceil(remaining);
        out.extend(slices(line, count, !horizontal));
        placed += count;
    }
    out
}

/// spiral and dwindle: each window halves what is left, turning (spiral) or not.
fn spiral(n: usize, a: B, s: &Suit, is_spiral: bool) -> Vec<B> {
    let mut out = Vec::new();
    let mut rest = a;
    for i in 1..=n {
        if i == n {
            out.push(rest);
            break;
        }
        let k = (i - 1) % 4;
        let ratio = if i == 1 { s.mwfact } else { 0.5 };
        let (cur, nxt) = if k % 2 == 0 {
            let w = (rest.w as f64 * ratio).floor() as i32;
            if is_spiral && k == 2 {
                (b(rest.x + rest.w - w, rest.y, w, rest.h), b(rest.x, rest.y, rest.w - w, rest.h))
            } else {
                (b(rest.x, rest.y, w, rest.h), b(rest.x + w, rest.y, rest.w - w, rest.h))
            }
        } else {
            let h = (rest.h as f64 * ratio).floor() as i32;
            if is_spiral && k == 3 {
                (b(rest.x, rest.y + rest.h - h, rest.w, h), b(rest.x, rest.y, rest.w, rest.h - h))
            } else {
                (b(rest.x, rest.y, rest.w, h), b(rest.x, rest.y + h, rest.w, rest.h - h))
            }
        };
        out.push(cur);
        rest = nxt;
    }
    out
}

/// corner: the master in a corner, the others beside it and below (or above) it.
fn corner(n: usize, a: B, s: &Suit, which: &str) -> Vec<B> {
    if n == 1 {
        return vec![a];
    }
    let mw = (a.w as f64 * s.mwfact).floor() as i32;
    let mh = (a.h as f64 * s.mwfact).floor() as i32;
    let left = which == "cornernw" || which == "cornersw";
    let top = which == "cornernw" || which == "cornerne";
    let mx = if left { a.x } else { a.x + a.w - mw };
    let my = if top { a.y } else { a.y + a.h - mh };
    let others = n - 1;
    if others == 1 {
        return vec![b(mx, a.y, mw, a.h), b(if left { a.x + mw } else { a.x }, a.y, a.w - mw, a.h)];
    }
    let mut out = vec![b(mx, my, mw, mh)];
    let ncolside = others.div_ceil(2);
    let nrow = others - ncolside;
    let side = b(if left { a.x + mw } else { a.x }, a.y, a.w - mw, a.h);
    let row = b(mx, if top { a.y + mh } else { a.y }, mw, a.h - mh);
    out.extend(slices(side, ncolside, true));
    out.extend(slices(row, nrow, false));
    out
}

/// The boxes for `n` windows in `area` (in their order: masters first), before gaps.
pub fn arrange(n: usize, area: Rectangle<i32, Logical>, s: &Suit) -> Vec<Rectangle<i32, Logical>> {
    if n == 0 {
        return Vec::new();
    }
    let a = b(area.loc.x, area.loc.y, area.size.w, area.size.h);
    let boxes = if s.suit == "center" {
        center(n, a, s)
    } else if n == 1 || s.suit == "max" {
        vec![a; n]
    } else {
        match s.suit.as_str() {
            "tile" => tile(n, a, s, 'l'),
            "tileleft" => tile(n, a, s, 'r'),
            "tilebottom" => tile(n, a, s, 't'),
            "tiletop" => tile(n, a, s, 'b'),
            "fair" => fair(n, a, false),
            "fairh" => fair(n, a, true),
            "spiral" => spiral(n, a, s, true),
            "dwindle" => spiral(n, a, s, false),
            other => corner(n, a, s, other),
        }
    };
    (0..n).map(|i| boxes.get(i).copied().unwrap_or(a)).map(|x| Rectangle::new((x.x, x.y).into(), (x.w, x.h).into())).collect()
}

/// `+0.05`, `-1`, `0.6`: added, taken, or set; kept in lo..=hi.
fn adjust(cur: f64, arg: &str, lo: f64, hi: f64) -> f64 {
    let arg = arg.trim();
    let v = match arg.chars().next() {
        Some('+') => arg[1..].parse::<f64>().map(|n| cur + n),
        Some('-') => arg[1..].parse::<f64>().map(|n| cur - n),
        _ => arg.parse::<f64>(),
    };
    v.map(|v| v.clamp(lo, hi)).unwrap_or(cur)
}

/// Every workspace's suit, by name, and where loom keeps them.
pub struct Layouts {
    pub by_ws: BTreeMap<String, Suit>,
}

fn home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

impl Layouts {
    /// loom's state, else Hyprland's (read only), else empty.
    pub fn load() -> Layouts {
        let own = home().join(".cache/loom-awesome.json");
        let text = std::fs::read_to_string(&own).or_else(|_| std::fs::read_to_string(home().join(".cache/hypr-awesome.json"))).unwrap_or_default();
        let mut by_ws = BTreeMap::new();
        if let Ok(serde_json::Value::Object(o)) = serde_json::from_str::<serde_json::Value>(&text) {
            for (ws, v) in o {
                let suit = v["suit"].as_str().unwrap_or("tile");
                if SUITS.contains(&suit) {
                    by_ws.insert(ws, Suit { suit: suit.into(), mwfact: v["mwfact"].as_f64().unwrap_or(0.5), nmaster: v["nmaster"].as_u64().unwrap_or(1) as usize, ncol: v["ncol"].as_u64().unwrap_or(1) as usize, pad: v["pad"].as_i64().unwrap_or(160) as i32 });
                }
            }
        }
        Layouts { by_ws }
    }

    fn save(&self) {
        let o: serde_json::Map<String, serde_json::Value> = self.by_ws.iter().map(|(k, s)| (k.clone(), serde_json::json!({ "suit": s.suit, "mwfact": (s.mwfact * 100.0).round() / 100.0, "nmaster": s.nmaster, "ncol": s.ncol, "pad": s.pad }))).collect();
        let _ = std::fs::write(home().join(".cache/loom-awesome.json"), serde_json::Value::Object(o).to_string());
    }

    pub fn of(&self, ws: &str) -> Suit {
        self.by_ws.get(ws).cloned().unwrap_or_default()
    }

    /// A layout message for workspace `ws`; Err for one it does not know. `swapmaster` is the
    /// window manager's (the order is its), answered here with Ok(true).
    pub fn message(&mut self, ws: &str, msg: &str) -> Result<bool, String> {
        let (cmd, arg) = msg.trim().split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((msg.trim(), ""));
        let s = self.by_ws.entry(ws.to_string()).or_default();
        match cmd {
            "suit" if SUITS.contains(&arg) => s.suit = arg.into(),
            "next" | "prev" => {
                let i = SUITS.iter().position(|x| *x == s.suit).unwrap_or(0);
                let n = SUITS.len();
                s.suit = SUITS[if cmd == "next" { (i + 1) % n } else { (i + n - 1) % n }].into();
            }
            "mwfact" | "splitratio" => s.mwfact = adjust(s.mwfact, arg, 0.05, 0.95),
            "nmaster" => s.nmaster = adjust(s.nmaster as f64, arg, 0.0, 10.0) as usize,
            "ncol" => s.ncol = adjust(s.ncol as f64, arg, 1.0, 10.0) as usize,
            "pad" => s.pad = adjust(s.pad as f64, arg, 0.0, 1200.0) as i32,
            "reset" => *s = Suit::default(),
            "swapmaster" => {
                self.save();
                return Ok(true);
            }
            "state" => {}
            _ => return Err(format!("unknown layout message: {msg}")),
        }
        self.save();
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rectangle<i32, Logical> {
        Rectangle::new((0, 0).into(), (1000, 600).into())
    }
    fn s(suit: &str) -> Suit {
        Suit { suit: suit.into(), ..Suit::default() }
    }

    #[test]
    fn center_floats_the_master_and_puts_the_rest_behind_on_the_whole_screen() {
        let one = arrange(1, area(), &s("center"));
        assert_eq!((one[0].loc.x, one[0].loc.y, one[0].size.w, one[0].size.h), (160, 96, 680, 408), "alone: still in the middle");
        let r = arrange(3, area(), &s("center"));
        assert_eq!((r[0].loc.x, r[0].size.w), (160, 680), "the master keeps its padding");
        assert_eq!(r[1].size.w * r[1].size.h + r[2].size.w * r[2].size.h, 1000 * 600, "the other two share the whole screen behind it");
        let mut l = Layouts { by_ws: Default::default() };
        l.message("1", "suit center").unwrap();
        l.message("1", "pad +40").unwrap();
        assert_eq!(l.of("1").pad, 200, "Super+plus widens the room");
        l.message("1", "pad -400").unwrap();
        assert_eq!(l.of("1").pad, 0, "never below none");
    }

    #[test]
    fn tile_puts_the_master_left_and_stacks_the_rest() {
        let r = arrange(3, area(), &s("tile"));
        assert_eq!(r[0], Rectangle::new((0, 0).into(), (500, 600).into()));
        assert_eq!((r[1].loc.x, r[1].size.h, r[2].loc.y), (500, 300, 300));
        let r = arrange(3, area(), &s("tileleft"));
        assert_eq!(r[0].loc.x, 500, "tileleft: master on the right");
    }

    #[test]
    fn fair_spiral_corner_max() {
        let r = arrange(4, area(), &s("fair"));
        assert_eq!(r.iter().map(|x| (x.size.w, x.size.h)).collect::<Vec<_>>(), vec![(500, 300); 4], "a 2x2 grid");
        let r = arrange(3, area(), &s("spiral"));
        assert_eq!((r[0].size.w, r[1].size.h, r[2].size.h), (500, 300, 300));
        let r = arrange(4, area(), &s("cornernw"));
        assert_eq!(r[0], Rectangle::new((0, 0).into(), (500, 300).into()), "master in the top-left corner");
        assert!(arrange(3, area(), &s("max")).iter().all(|x| *x == area()));
    }

    #[test]
    fn messages_change_the_workspace_suit() {
        let mut l = Layouts { by_ws: BTreeMap::new() };
        l.by_ws.insert("1".into(), Suit::default());
        assert!(l.message("1", "suit fair").is_ok());
        assert_eq!(l.of("1").suit, "fair");
        l.message("1", "next").unwrap();
        assert_eq!(l.of("1").suit, "fairh");
        l.message("1", "mwfact +0.1").unwrap();
        assert!((l.of("1").mwfact - 0.6).abs() < 1e-9);
        assert!(l.message("1", "nonsense").is_err());
        assert_eq!(l.of("2").suit, "tile", "another workspace keeps its own");
    }
}
