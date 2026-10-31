//! Hyprland 0.56's dispatch form, `hl.dsp.NAME(ARGS)` with Lua literals (the shell's
//! HyprDispatch.qml sends only these): read without a Lua runtime (strings, numbers, booleans,
//! names, `{ key = value }` tables) and carried out by loom's dispatchers.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Str(String),
    Num(f64),
    Bool(bool),
    /// a bare name (`nil`, an unquoted word)
    Name(String),
    Table(BTreeMap<String, Val>),
}

impl Val {
    /// As dispatcher text: strings bare, numbers without a needless `.0`.
    pub fn text(&self) -> String {
        match self {
            Val::Str(s) | Val::Name(s) => s.clone(),
            Val::Num(n) if n.fract() == 0.0 => format!("{}", *n as i64),
            Val::Num(n) => n.to_string(),
            Val::Bool(b) => b.to_string(),
            Val::Table(_) => String::new(),
        }
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && (self.s[self.i] as char).is_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_' || self.s[self.i] == b'.') {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).to_string())
    }
    fn value(&mut self) -> Option<Val> {
        self.ws();
        match *self.s.get(self.i)? {
            q @ (b'"' | b'\'') => {
                self.i += 1;
                let mut out = Vec::new();
                while let Some(&c) = self.s.get(self.i) {
                    self.i += 1;
                    match c {
                        b'\\' => {
                            let e = *self.s.get(self.i)?;
                            self.i += 1;
                            out.push(match e {
                                b'n' => b'\n',
                                b't' => b'\t',
                                other => other,
                            });
                        }
                        c if c == q => return Some(Val::Str(String::from_utf8_lossy(&out).to_string())),
                        c => out.push(c),
                    }
                }
                None
            }
            b'{' => {
                self.i += 1;
                let mut t = BTreeMap::new();
                let mut n = 1;
                loop {
                    if self.eat(b'}') {
                        return Some(Val::Table(t));
                    }
                    // `key = value`, or a positional value (keyed 1, 2, …)
                    let save = self.i;
                    let key = self.name();
                    let (k, v) = if key.is_some() && self.eat(b'=') {
                        (key.unwrap_or_default(), self.value()?)
                    } else {
                        self.i = save;
                        let k = n.to_string();
                        n += 1;
                        (k, self.value()?)
                    };
                    t.insert(k, v);
                    self.eat(b',');
                }
            }
            c if c == b'-' || c.is_ascii_digit() => {
                let start = self.i;
                self.i += 1;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                    self.i += 1;
                }
                String::from_utf8_lossy(&self.s[start..self.i]).parse().ok().map(Val::Num)
            }
            _ => match self.name()?.as_str() {
                "true" => Some(Val::Bool(true)),
                "false" => Some(Val::Bool(false)),
                other => Some(Val::Name(other.to_string())),
            },
        }
    }
}

/// `hl.dsp.window.move({ workspace = 3 })` → ("window.move", [Table{workspace: 3}]).
pub fn parse(src: &str) -> Option<(String, Vec<Val>)> {
    call(src, "hl.dsp.")
}

/// `PREFIXname(args)`, e.g. `hl.window_rule({ … })` with prefix `hl.`.
pub fn call(src: &str, prefix: &str) -> Option<(String, Vec<Val>)> {
    let src = src.trim();
    let rest = src.strip_prefix(prefix)?;
    let open = rest.find('(')?;
    let name = rest[..open].trim().to_string();
    let mut p = P { s: rest[open + 1..].as_bytes(), i: 0 };
    let mut args = Vec::new();
    loop {
        if p.eat(b')') {
            return Some((name, args));
        }
        args.push(p.value()?);
        p.eat(b',');
    }
}

/// The table argument's field, if given.
fn field<'a>(args: &'a [Val], key: &str) -> Option<&'a Val> {
    args.iter().find_map(|a| if let Val::Table(t) = a { t.get(key) } else { None })
}

/// The first plain (non-table) argument.
fn first(args: &[Val]) -> Option<String> {
    args.iter().find(|a| !matches!(a, Val::Table(_))).map(Val::text)
}

/// An `hl.dsp.…` call as loom's classic dispatches (`workspace 3`, `focuswindow class:x` …);
/// several when a window must be focused first. None for one loom does not know.
pub fn translate(src: &str) -> Option<Vec<String>> {
    let (name, args) = parse(src)?;
    let f = |k: &str| field(&args, k).map(Val::text);
    // a window selector first focuses that window (then the dispatch acts on the active one)
    let mut pre: Vec<String> = f("window").map(|w| vec![format!("focuswindow {w}")]).unwrap_or_default();
    let main: Vec<String> = match name.as_str() {
        "focus" => {
            if let Some(ws) = f("workspace") {
                vec![format!("workspace {ws}")]
            } else if let Some(d) = f("direction") {
                vec![format!("movefocus {d}")]
            } else if f("window").is_some() {
                vec![]
            } else if let Some(m) = f("monitor") {
                vec![format!("focusmonitor {m}")]
            } else {
                vec![]
            }
        }
        "window.move" => {
            if let Some(ws) = f("workspace") {
                let silent = field(&args, "follow") == Some(&Val::Bool(false));
                vec![format!("{} {ws}", if silent { "movetoworkspacesilent" } else { "movetoworkspace" })]
            } else if let (Some(x), Some(y)) = (f("x"), f("y")) {
                vec![format!("movewindowpixel exact {x} {y}")]
            } else if let Some(d) = f("direction") {
                vec![format!("movewindow {d}")]
            } else {
                vec![]
            }
        }
        // a named window closed where it is (not focused first: that switched workspace)
        "window.close" | "window.kill" => {
            let verb = if name == "window.kill" { "killwindow" } else { "closewindow" };
            match pre.pop() {
                Some(focus) => vec![format!("{verb} {}", focus.trim_start_matches("focuswindow ").trim())],
                None => vec![if name == "window.kill" { "killwindow activewindow".into() } else { "killactive".into() }],
            }
        }
        // Hyprland 0.56: action toggle / enable (on) / disable (off)
        "window.float" => vec![match f("action").as_deref() {
            Some("enable" | "on") => "setfloating".into(),
            Some("disable" | "off") => "settiled".into(),
            _ => "togglefloating".into(),
        }],
        "window.resize" => match (f("x"), f("y")) {
            (Some(x), Some(y)) => {
                let rel = field(&args, "relative") == Some(&Val::Bool(true));
                vec![if rel { format!("resizewindowpixel {x} {y}") } else { format!("resizewindowpixel exact {x} {y}") }]
            }
            // no size: the mouse resize (a `mouse = true` bind, as bindm resizewindow)
            _ => vec!["resizewindow".into()],
        },
        "window.pin" => vec!["pin".into()],
        // the mode matters: "maximized" maximizes in the layout, as Hyprland's fullscreen 1; only
        // "fullscreen" covers the screen; fullscreen_state sets the layout's and the program's apart
        "window.fullscreen" => vec![if field(&args, "mode").map(|m| m.text()).as_deref() == Some("maximized") { "fullscreen 1".into() } else { "fullscreen".into() }],
        "window.fullscreen_state" => vec![format!("fullscreenstate {} {}", f("internal").unwrap_or_else(|| "-1".into()), f("client").unwrap_or_else(|| "-1".into()))],
        "window.cycle_next" => vec![if first(&args).is_some_and(|a| a.contains("prev")) { "cyclenext prev".into() } else { "cyclenext".into() }],
        "window.bring_to_top" => vec!["bringactivetotop".into()],
        "workspace.toggle_special" => vec![format!("togglespecialworkspace {}", first(&args).or_else(|| f("name")).unwrap_or_default()).trim_end().to_string()],
        "cursor.move" => vec![format!("movecursor {} {}", f("x").unwrap_or_default(), f("y").unwrap_or_default())],
        "exec_cmd" => {
            let cmd = first(&args)?;
            match f("workspace") {
                Some(ws) => vec![format!("exec [workspace {ws}] {cmd}")],
                None => vec![format!("exec {cmd}")],
            }
        }
        "global" => vec![format!("global {}", first(&args)?)],
        // the mouse move (a `mouse = true` bind, as bindm movewindow)
        "window.drag" => vec!["movewindow".into()],
        // a key sent to the window: send_shortcut({ mods = "CTRL", key = "Left" })
        "send_shortcut" => vec![format!("sendshortcut {}, {}", f("mods").unwrap_or_default(), f("key")?)],
        "submap" => vec![format!("submap {}", first(&args).unwrap_or_else(|| "reset".into()))],
        "layout" => vec![format!("layoutmsg {}", first(&args)?)],
        // accepted, nothing to do yet (one monitor, no DPMS headless, events are the shell's own)
        "dpms" => vec![format!("dpms {}", first(&args).or_else(|| field(&args, "action").map(Val::text)).unwrap_or_else(|| "toggle".into()))],
        "workspace.move" => match (f("workspace"), f("monitor")) {
            (Some(ws), Some(m)) => vec![format!("moveworkspacetomonitor {ws} {m}")],
            (None, Some(m)) => vec![format!("movecurrentworkspacetomonitor {m}")],
            _ => vec![],
        },
        "workspace.rename" | "event" => vec![],
        _ => return None,
    };
    pre.extend(main);
    Some(pre)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shells_calls() {
        // a named window closed where it is, not focused first
        assert_eq!(translate("hl.dsp.window.close({ window = \"address:0x5a\" })"), Some(vec!["closewindow address:0x5a".into()]));
        assert_eq!(translate("hl.dsp.window.close()"), Some(vec!["killactive".into()]));
        assert_eq!(translate("hl.dsp.window.float({ action = \"enable\" })"), Some(vec!["setfloating".into()]));
        assert_eq!(translate("hl.dsp.window.float({ action = \"disable\" })"), Some(vec!["settiled".into()]));
        assert_eq!(translate("hl.dsp.window.float({ action = \"toggle\" })"), Some(vec!["togglefloating".into()]));
        assert_eq!(translate("hl.dsp.window.resize({ x = 800, y = 600, relative = false })"), Some(vec!["resizewindowpixel exact 800 600".into()]));
        assert_eq!(translate("hl.dsp.window.resize({ x = 10, y = -5, relative = true })"), Some(vec!["resizewindowpixel 10 -5".into()]));
        assert_eq!(translate("hl.dsp.focus({ workspace = 3 })"), Some(vec!["workspace 3".into()]));
        assert_eq!(translate("hl.dsp.focus({ workspace = \"r+1\" })"), Some(vec!["workspace r+1".into()]));
        assert_eq!(translate("hl.dsp.window.move({ workspace = 4, follow = false, window = \"address:0x5a000100\" })"), Some(vec!["focuswindow address:0x5a000100".into(), "movetoworkspacesilent 4".into()]));
        assert_eq!(translate("hl.dsp.window.close()"), Some(vec!["killactive".into()]));
        assert_eq!(translate("hl.dsp.exec_cmd('zash -e \"a b\"', { workspace = \"2 silent\" })"), Some(vec!["exec [workspace 2 silent] zash -e \"a b\"".into()]));
        assert_eq!(translate("hl.dsp.global(\"quickshell:overviewToggle\")"), Some(vec!["global quickshell:overviewToggle".into()]));
        assert_eq!(translate("hl.dsp.cursor.move({ x = 9999, y = -5 })"), Some(vec!["movecursor 9999 -5".into()]));
        assert_eq!(translate("hl.dsp.focus({ window = \"class:^(zash)$\" })"), Some(vec!["focuswindow class:^(zash)$".into()]));
        assert_eq!(translate("hl.dsp.send_shortcut({ mods = \"CTRL\", key = \"Left\" })"), Some(vec!["sendshortcut CTRL, Left".into()]));
        assert_eq!(translate("hl.dsp.window.drag()"), Some(vec!["movewindow".into()]));
        assert_eq!(translate("hl.dsp.window.resize()"), Some(vec!["resizewindow".into()]));
        assert_eq!(translate("hl.dsp.nonsense()"), None);
    }
}
