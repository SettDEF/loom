//! The user's Hyprland keybinds, read from the Hyprland config itself (hyprland.conf and what it
//! sources: `$vars`, `submap = …` sections, `bind[flags] = MODS, KEY, [desc,] DISPATCHER, ARGS`),
//! so the same binds work in loom. Parsed once into tables by key: a key press is one hash
//! lookup and allocates nothing unless a bind matches (performance first, the user 2026-10-21).

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use smithay::input::keyboard::{keysyms, xkb, ModifiersState};

pub const SHIFT: u8 = 1;
pub const CTRL: u8 = 4;
pub const ALT: u8 = 8;
pub const SUPER: u8 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Sym(u32),
    /// `code:N`, the xkb keycode
    Code(u32),
    Catchall,
}

#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub release: bool,
    pub repeat: bool,
    pub non_consuming: bool,
    pub ignore_mods: bool,
    #[allow(dead_code)] // binds that work on the lock screen (with ext-session-lock)
    pub locked: bool,
}

#[derive(Debug, Clone)]
pub struct Bind {
    pub mods: u8,
    pub key: Key,
    pub flags: Flags,
    /// `DISPATCHER ARGS`, as `hyprctl dispatch` takes it
    pub action: String,
    pub description: String,
}

#[derive(Default)]
struct Table {
    by_key: HashMap<Key, Vec<usize>>,
    catchall: Vec<usize>,
}

#[derive(Default)]
pub struct Binds {
    pub binds: Vec<Bind>,
    tables: HashMap<String, Table>,
    pub submap: String,
    /// keys whose press a bind took: their release is taken too
    taken: HashSet<u32>,
    /// mouse and other binds loom does not handle yet (counted, said once)
    pub skipped: usize,
    /// the last key() matched a repeating bind (binde): loom repeats it while the key is held
    pub last_repeat: bool,
}

fn mods_of(s: &str) -> u8 {
    let mut m = 0;
    for part in s.split(|c: char| c == ' ' || c == '_' || c == '+').filter(|p| !p.is_empty()) {
        m |= match part.to_ascii_uppercase().as_str() {
            "SHIFT" => SHIFT,
            "CTRL" | "CONTROL" => CTRL,
            "ALT" | "MOD1" => ALT,
            "SUPER" | "WIN" | "LOGO" | "MOD4" | "META" => SUPER,
            _ => 0,
        };
    }
    m
}

/// The modifier a key is itself (its own bit is ignored when it is the key bound: `Super, Super_L`).
fn self_mod(sym: u32) -> u8 {
    match sym {
        keysyms::KEY_Super_L | keysyms::KEY_Super_R | keysyms::KEY_Meta_L | keysyms::KEY_Meta_R => SUPER,
        keysyms::KEY_Shift_L | keysyms::KEY_Shift_R => SHIFT,
        keysyms::KEY_Control_L | keysyms::KEY_Control_R => CTRL,
        keysyms::KEY_Alt_L | keysyms::KEY_Alt_R => ALT,
        _ => 0,
    }
}

/// A modifier key by its keycode (xkb code; evdev + 8), for `code:133` style binds.
fn self_mod_code(code: u32) -> u8 {
    match code {
        133 | 134 => SUPER,
        50 | 62 => SHIFT,
        37 | 105 => CTRL,
        64 | 108 => ALT,
        _ => 0,
    }
}

pub fn mask(m: &ModifiersState) -> u8 {
    (if m.shift { SHIFT } else { 0 }) | (if m.ctrl { CTRL } else { 0 }) | (if m.alt { ALT } else { 0 }) | (if m.logo { SUPER } else { 0 })
}

fn key_of(s: &str) -> Option<Key> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("catchall") {
        return Some(Key::Catchall);
    }
    if let Some(n) = s.strip_prefix("code:") {
        return n.trim().parse().ok().map(Key::Code);
    }
    if s.starts_with("mouse") {
        return None;
    }
    // raw (unshifted) syms are lower case for letters
    let name = if s.chars().count() == 1 { s.to_lowercase() } else { s.to_string() };
    let sym = xkb::keysym_from_name(&name, xkb::KEYSYM_NO_FLAGS);
    let sym = if sym.raw() == keysyms::KEY_NoSymbol { xkb::keysym_from_name(&name, xkb::KEYSYM_CASE_INSENSITIVE) } else { sym };
    (sym.raw() != keysyms::KEY_NoSymbol).then_some(Key::Sym(sym.raw()))
}

fn expand(s: &str, vars: &HashMap<String, String>) -> String {
    let mut out = s.replace("$HOME", &std::env::var("HOME").unwrap_or_default());
    // the longest names first, so $qsConfigDir is not read as $qsConfig + Dir
    let mut names: Vec<&String> = vars.keys().collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    for n in names {
        if out.contains('$') {
            out = out.replace(&format!("${n}"), &vars[n]);
        }
    }
    out
}

impl Binds {
    /// The binds of `conf` and everything it sources; none when it is not there.
    pub fn load(conf: &Path) -> Binds {
        let mut b = Binds { submap: "global".into(), ..Default::default() };
        let mut vars = HashMap::new();
        let mut submap = "global".to_string();
        b.read(conf, &mut vars, &mut submap, 0);
        b.submap = "global".into();
        b
    }

    fn read(&mut self, file: &Path, vars: &mut HashMap<String, String>, submap: &mut String, depth: u32) {
        let Ok(text) = std::fs::read_to_string(file) else { return };
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        for raw in text.lines() {
            // a `#` starts a comment; `##` is a literal one
            let line = match raw.find(" #").or_else(|| raw.starts_with('#').then_some(0)) {
                Some(i) => &raw[..i],
                None => raw,
            };
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            if let Some(name) = k.strip_prefix('$') {
                let val = expand(v, vars);
                vars.insert(name.to_string(), val);
            } else if k == "source" && depth < 8 {
                let p = expand(v, vars).replace('~', &std::env::var("HOME").unwrap_or_default());
                let p = if Path::new(&p).is_absolute() { PathBuf::from(p) } else { dir.join(p) };
                self.read(&p, vars, submap, depth + 1);
            } else if k == "submap" {
                *submap = v.to_string();
            } else if let Some(flags) = k.strip_prefix("bind") {
                self.add(flags, &expand(v, vars), submap);
            }
        }
    }

    /// One `bind[flags] = …` line's right side (also `hyprctl keyword bind …`).
    pub fn add(&mut self, flags: &str, rest: &str, submap: &str) {
        let f = Flags {
            release: flags.contains('r'),
            repeat: flags.contains('e'),
            non_consuming: flags.contains('n'),
            ignore_mods: flags.contains('i'),
            locked: flags.contains('l'),
        };
        let with_desc = flags.contains('d');
        if flags.contains('m') {
            self.skipped += 1;
            return;
        }
        let parts: Vec<&str> = rest.splitn(if with_desc { 5 } else { 4 }, ',').map(str::trim).collect();
        let (mods, key, desc, disp, args) = match (with_desc, parts.as_slice()) {
            (true, [m, k, d, disp, a @ ..]) => (*m, *k, *d, *disp, a.first().copied().unwrap_or("")),
            (false, [m, k, disp, a @ ..]) => (*m, *k, "", *disp, a.first().copied().unwrap_or("")),
            _ => return,
        };
        let Some(key) = key_of(key) else {
            self.skipped += 1;
            return;
        };
        let action = if args.is_empty() { disp.to_string() } else { format!("{disp} {args}") };
        let i = self.binds.len();
        self.binds.push(Bind { mods: mods_of(mods), key, flags: f, action, description: desc.to_string() });
        let t = self.tables.entry(submap.to_string()).or_default();
        if key == Key::Catchall {
            t.catchall.push(i);
        } else {
            t.by_key.entry(key).or_default().push(i);
        }
    }

    /// `hyprctl keyword unbind MODS, KEY`
    pub fn remove(&mut self, rest: &str) {
        let mut p = rest.splitn(2, ',').map(str::trim);
        let (Some(m), Some(k)) = (p.next(), p.next()) else { return };
        let (mods, Some(key)) = (mods_of(m), key_of(k)) else { return };
        if let Some(t) = self.tables.get_mut(&self.submap) {
            if let Some(v) = t.by_key.get_mut(&key) {
                v.retain(|&i| self.binds[i].mods != mods);
            }
        }
    }

    /// A key event: the actions to run, and whether the program loses the key. Hot path: no
    /// allocation unless a bind matches.
    pub fn key(&mut self, code: u32, sym: u32, mods: u8, pressed: bool, repeat: bool) -> (Option<Vec<String>>, bool) {
        let Some(t) = self.tables.get(&self.submap) else { return (None, false) };
        let mut out: Option<Vec<String>> = None;
        let mut consume = false;
        let mut matched = false;
        self.last_repeat = false;
        let mut rep = false;
        for key in [Key::Sym(sym), Key::Code(code)] {
            let own = match key {
                Key::Sym(s) => self_mod(s),
                Key::Code(c) => self_mod_code(c),
                Key::Catchall => 0,
            };
            let Some(list) = t.by_key.get(&key) else { continue };
            for &i in list {
                let b = &self.binds[i];
                if b.flags.release == pressed || (repeat && !b.flags.repeat) {
                    continue;
                }
                if !b.flags.ignore_mods && (b.mods & !own) != (mods & !own) {
                    continue;
                }
                matched = true;
                consume |= !b.flags.non_consuming;
                rep |= b.flags.repeat && pressed;
                out.get_or_insert_with(Vec::new).push(b.action.clone());
            }
        }
        self.last_repeat = rep;
        // a submap's catchall takes what nothing else did
        if !matched && pressed {
            for &i in &t.catchall {
                let b = &self.binds[i];
                if !b.flags.ignore_mods && b.mods != mods {
                    continue;
                }
                consume |= !b.flags.non_consuming;
                out.get_or_insert_with(Vec::new).push(b.action.clone());
            }
        }
        // the release of a key a bind took is taken too
        if pressed {
            if consume {
                self.taken.insert(code);
            }
        } else if self.taken.remove(&code) {
            consume = true;
        }
        (out, consume)
    }

    /// Hyprland's `binds` answer, roughly.
    pub fn list(&self) -> Vec<serde_json::Value> {
        self.binds.iter().map(|b| serde_json::json!({ "modmask": b.mods, "key": match b.key { Key::Sym(s) => xkb::keysym_get_name(xkb::Keysym::new(s)), Key::Code(c) => format!("code:{c}"), Key::Catchall => "catchall".into() }, "release": b.flags.release, "repeat": b.flags.repeat, "non_consuming": b.flags.non_consuming, "description": b.description, "dispatcher": b.action.split_once(' ').map_or(b.action.as_str(), |x| x.0), "arg": b.action.split_once(' ').map_or("", |x| x.1) })).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conf(text: &str) -> Binds {
        let dir = std::env::temp_dir().join(format!("loom-binds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sub.conf"), "bind = Super, Q, killactive,\n").unwrap();
        std::fs::write(dir.join("hyprland.conf"), text).unwrap();
        let b = Binds::load(&dir.join("hyprland.conf"));
        std::fs::remove_dir_all(&dir).ok();
        b
    }

    #[test]
    fn reads_vars_sources_flags_and_submaps() {
        let mut b = conf("$term = zash\nsource = sub.conf\nbind = Super, Return, exec, $term # a comment\nbindd = Super Shift, 2, Move, movetoworkspace, 2\nbindrd = Super, code:133, Search, global, quickshell:search\nbind = Super, Escape, submap, vim\nsubmap = vim\nbind = , h, movefocus, l\nbind = , catchall, exec, true\nsubmap = global\n");
        let ret = xkb::keysym_from_name("Return", 0).raw();
        assert_eq!(b.key(36, ret, SUPER, true, false), (Some(vec!["exec zash".into()]), true));
        assert_eq!(b.key(36, ret, SUPER, false, false), (None, true), "its release is taken too");
        let q = xkb::keysym_from_name("q", 0).raw();
        assert_eq!(b.key(24, q, SUPER, true, false).0, Some(vec!["killactive".into()]), "from the sourced file");
        let two = xkb::keysym_from_name("2", 0).raw();
        assert_eq!(b.key(11, two, SUPER | SHIFT, true, false).0, Some(vec!["movetoworkspace 2".into()]));
        assert_eq!(b.key(11, two, SUPER, true, false).0, None, "mods must match");
        // Super alone, released: its own modifier bit does not count
        let sup = xkb::keysym_from_name("Super_L", 0).raw();
        assert_eq!(b.key(133, sup, 0, false, false).0, Some(vec!["global quickshell:search".into()]));
        b.submap = "vim".into();
        let h = xkb::keysym_from_name("h", 0).raw();
        assert_eq!(b.key(43, h, 0, true, false).0, Some(vec!["movefocus l".into()]));
        let x = xkb::keysym_from_name("x", 0).raw();
        assert_eq!(b.key(53, x, 0, true, false).0, Some(vec!["exec true".into()]), "the submap's catchall");
    }
}

impl crate::state::Loom {
    /// Every key goes through here: the binds first (one lookup), then the focused program.
    pub fn key_input(&mut self, code: smithay::input::keyboard::Keycode, st: smithay::backend::input::KeyState, time: u32) {
        let pressed = st == smithay::backend::input::KeyState::Pressed;
        let kb = self.seat.get_keyboard().expect("keyboard");
        kb.input::<(), _>(self, code, st, smithay::utils::SERIAL_COUNTER.next_serial(), time, |data, mods, h| {
            let sym = h.raw_latin_sym_or_raw_current_sym().map_or(0, |s| s.raw());
            let (acts, consume) = data.binds.key(h.raw_code().raw(), sym, mask(mods), pressed, false);
            if let Some(a) = acts {
                if data.binds.last_repeat {
                    data.start_repeat(h.raw_code().raw(), a.clone());
                }
                data.bind_actions.extend(a);
            }
            if !pressed && data.repeat.as_ref().is_some_and(|r| r.0 == h.raw_code().raw()) {
                data.repeat = None;
            }
            if consume {
                smithay::input::keyboard::FilterResult::Intercept(())
            } else {
                smithay::input::keyboard::FilterResult::Forward
            }
        });
        if !self.bind_actions.is_empty() {
            for a in std::mem::take(&mut self.bind_actions) {
                let r = self.hypr_dispatch(&a);
                if r != "ok" {
                    eprintln!("loom: bind `{a}`: {r}");
                }
            }
        }
    }
}

impl crate::state::Loom {
    /// A held repeating bind (binde, vim mode's h j k l): run again after the keyboard's repeat
    /// delay, then at its rate, until the key is let go.
    pub fn start_repeat(&mut self, code: u32, actions: Vec<String>) {
        self.repeat_gen += 1;
        let this = self.repeat_gen;
        self.repeat = Some((code, actions));
        let Some(h) = self.loop_handle.clone() else { return };
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        let _ = h.insert_source(Timer::from_duration(std::time::Duration::from_millis(600)), move |_, _, s: &mut crate::state::Loom| {
            if s.repeat_gen != this {
                return TimeoutAction::Drop;
            }
            let Some((_, acts)) = s.repeat.clone() else { return TimeoutAction::Drop };
            for a in &acts {
                s.hypr_dispatch(a);
            }
            TimeoutAction::ToDuration(std::time::Duration::from_millis(40))
        });
    }
}
