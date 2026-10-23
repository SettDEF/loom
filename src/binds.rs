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
    /// `mouse:N`, the evdev button (272 = left)
    Mouse(u32),
    /// `mouse_up` 0, `mouse_down` 1, `mouse_left` 2, `mouse_right` 3
    Scroll(u8),
}

#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub release: bool,
    pub repeat: bool,
    pub non_consuming: bool,
    pub ignore_mods: bool,
    /// `t`: a catchall that fires even when another bind takes the key (the shell's "Super was
    /// used for something else" signal, so the launcher does not open on its release)
    pub transparent: bool,
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
    /// the config's `env = NAME,VALUE` lines, in order (set for what loom starts on a console)
    pub env: Vec<(String, String)>,
    /// `exec-once` and `exec` lines, in order: started by `loom --tty --session`
    pub exec_once: Vec<String>,
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
    if let Some(n) = s.strip_prefix("mouse:") {
        return n.trim().parse().ok().map(Key::Mouse);
    }
    match s {
        "mouse_up" => return Some(Key::Scroll(0)),
        "mouse_down" => return Some(Key::Scroll(1)),
        "mouse_left" => return Some(Key::Scroll(2)),
        "mouse_right" => return Some(Key::Scroll(3)),
        _ => {}
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
        // a Lua config (hyprland.lua next to it) is what Hyprland 0.56 binds: its binds replace the
        // conf's, which go stale once the Lua is edited (Super+Return differed, and would open two)
        let lua = conf.with_file_name("hyprland.lua");
        let lua_binds: Vec<(String, String)> = if lua.is_file() { crate::hypr_conf_lua::lines(&lua).into_iter().filter(|(k, _)| k.starts_with("bind") && k.contains('@')).collect() } else { Vec::new() };
        if !lua_binds.is_empty() {
            b.binds.clear();
            b.tables.clear();
            b.skipped = 0;
            for (k, v) in lua_binds {
                let (flags, submap) = k["bind".len()..].split_once('@').unwrap_or(("", "global"));
                b.add(flags, &v, submap);
            }
        }
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
            } else if k == "env" {
                if let Some((name, val)) = expand(v, vars).split_once(',') {
                    self.env.push((name.trim().to_string(), val.trim().to_string()));
                }
            } else if k == "exec-once" || k == "exec" {
                self.exec_once.push(expand(v, vars));
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
            transparent: flags.contains('t'),
            locked: flags.contains('l'),
        };
        let with_desc = flags.contains('d');
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
        // bindm: a drag while the button is held (move or resize the window under the pointer)
        let action = if flags.contains('m') { format!("__drag {disp}") } else { action };
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
                _ => 0,
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
        // a submap's catchall takes what nothing else did; a transparent one any key but a modifier
        let modifier = self_mod(sym) != 0 || self_mod_code(code) != 0;
        if pressed && !modifier {
            for &i in &t.catchall {
                let b = &self.binds[i];
                if matched && !b.flags.transparent {
                    continue;
                }
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
        // a modifier (Shift, Ctrl, Alt, Super, AltGr) always reaches the program too, as in Hyprland:
        // the launcher's Super-release bind took Super's release, and the window kept Super held, so
        // its clicks came as Super+click (Zen's links did nothing) and keys as Super+key (Bitwig)
        if (0xffe1..=0xffee).contains(&sym) || sym == 0xfe03 {
            consume = false;
            self.taken.remove(&code);
        }
        (out, consume)
    }

    /// Hyprland's `binds` answer, roughly.
    pub fn list(&self) -> Vec<serde_json::Value> {
        self.binds.iter().map(|b| serde_json::json!({ "modmask": b.mods, "key": match b.key { Key::Sym(s) => xkb::keysym_get_name(xkb::Keysym::new(s)), Key::Code(c) => format!("code:{c}"), Key::Catchall => "catchall".into(), Key::Mouse(n) => format!("mouse:{n}"), Key::Scroll(d) => ["mouse_up", "mouse_down", "mouse_left", "mouse_right"][d as usize % 4].into() }, "release": b.flags.release, "repeat": b.flags.repeat, "non_consuming": b.flags.non_consuming, "description": b.description, "dispatcher": b.action.split_once(' ').map_or(b.action.as_str(), |x| x.0), "arg": b.action.split_once(' ').map_or("", |x| x.1) })).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conf(text: &str) -> Binds {
        // one folder per call: the tests run at once
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("loom-binds-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sub.conf"), "bind = Super, Q, killactive,\n").unwrap();
        std::fs::write(dir.join("hyprland.conf"), text).unwrap();
        let b = Binds::load(&dir.join("hyprland.conf"));
        std::fs::remove_dir_all(&dir).ok();
        b
    }

    #[test]
    #[test]
    fn super_used_for_something_else_tells_the_shell() {
        // the shell's own binds: Super alone opens the launcher on release, unless a key came between
        let mut b = conf("bindrd = Super, code:133, Search, global, quickshell:searchReleaseTap\nbinditn = Super, catchall, global, quickshell:searchToggleReleaseInterrupt\nbind = Super, 1, workspace, 1\n");
        let sup = xkb::keysym_from_name("Super_L", 0).raw();
        let one = xkb::keysym_from_name("1", 0).raw();
        let int = "global quickshell:searchToggleReleaseInterrupt".to_string();
        assert!(!b.key(133, sup, SUPER, true, false).0.unwrap_or_default().contains(&int), "Super itself is no other key");
        assert_eq!(b.key(10, one, SUPER, true, false).0, Some(vec!["workspace 1".into(), int.clone()]), "Super+1: the workspace and the interrupt");
        assert_eq!(b.key(133, sup, 0, false, false).0, Some(vec!["global quickshell:searchReleaseTap".into()]));
    }

    fn reads_exec_once_in_order() {
        let b = conf("$qs = ii\nexec-once = start-shell.sh $qs\n# exec-once = commented\nexec-once = hypridle # the idle daemon\nexec = true\n");
        assert_eq!(b.exec_once, vec!["start-shell.sh ii", "hypridle", "true"]);
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
        self.activity(pressed);
        let kb = self.seat.get_keyboard().expect("keyboard");
        // the keyboard stuck on a panel that no longer wants it (or is gone): back to the window
        // first, so the key reaches it (the browser got no keys while the desktop layer kept it)
        if pressed && !self.locked() {
            if let Some(f) = kb.current_focus().and_then(|f| f.surface()) {
                let layer = self.layer_output_of(&f).is_some();
                let stale = !smithay::reexports::wayland_server::Resource::is_alive(&f) || (layer && self.layer_keyboard(&f) == smithay::wayland::shell::wlr_layer::KeyboardInteractivity::None);
                if stale {
                    eprintln!("loom: the keyboard was left on {}; back to the window", if layer { self.layer_ns(&f) } else { "a gone surface".into() });
                    match self.focused.clone() {
                        Some(w) => self.focus_window(&w),
                        None => kb.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial()),
                    }
                }
            }
        }
        kb.input::<(), _>(self, code, st, smithay::utils::SERIAL_COUNTER.next_serial(), time, |data, mods, h| {
            let sym = h.raw_latin_sym_or_raw_current_sym().map_or(0, |s| s.raw());
            // on the real screen, before any bind: Ctrl+Alt+F<n> switches the console, Ctrl+Alt+Backspace quits
            if pressed && data.tty.is_some() {
                let m = h.modified_sym().raw();
                if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&m) {
                    if let Some(t) = data.tty.as_mut() {
                        t.vt_switch = Some((m - keysyms::KEY_XF86Switch_VT_1 + 1) as i32);
                    }
                    return smithay::input::keyboard::FilterResult::Intercept(());
                }
                // not while locked: quitting would leave the session open
                if mods.ctrl && mods.alt && sym == keysyms::KEY_BackSpace && !data.locked() {
                    eprintln!("loom: Ctrl+Alt+Backspace, quitting");
                    data.signal.stop();
                    return smithay::input::keyboard::FilterResult::Intercept(());
                }
            }
            // a VM or remote desktop holding a shortcuts inhibitor: its keys too
            if smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat::keyboard_shortcuts_inhibited(&data.seat) {
                return smithay::input::keyboard::FilterResult::Forward;
            }
            // locked: every key to the lock screen, no bind
            if data.locked() {
                return smithay::input::keyboard::FilterResult::Forward;
            }
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
        let held = crate::hypr_proto::Held::Key(code.raw());
        if !self.bind_actions.is_empty() {
            let acts = std::mem::take(&mut self.bind_actions);
            if pressed {
                self.hold_globals(held, &acts);
            }
            for a in acts {
                let r = self.hypr_dispatch(&a);
                if r != "ok" {
                    eprintln!("loom: bind `{a}`: {r}");
                }
            }
        }
        if !pressed {
            self.release_globals(held);
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
        let (delay, rate) = crate::config::key_repeat();
        let every = std::time::Duration::from_millis(1000 / rate as u64);
        let _ = h.insert_source(Timer::from_duration(std::time::Duration::from_millis(delay as u64)), move |_, _, s: &mut crate::state::Loom| {
            if s.repeat_gen != this {
                return TimeoutAction::Drop;
            }
            let Some((_, acts)) = s.repeat.clone() else { return TimeoutAction::Drop };
            for a in &acts {
                s.hypr_dispatch(a);
            }
            TimeoutAction::ToDuration(every)
        });
    }
}

impl Binds {
    /// A mouse button or scroll bind: the actions, and whether the program loses the event.
    pub fn mouse(&mut self, key: Key, mods: u8, pressed: bool) -> (Option<Vec<String>>, bool) {
        let Some(t) = self.tables.get(&self.submap) else { return (None, false) };
        let Some(list) = t.by_key.get(&key) else { return (None, false) };
        let mut out: Option<Vec<String>> = None;
        let mut consume = false;
        for &i in list {
            let b = &self.binds[i];
            // a drag starts on the press; other binds fire on press unless they are release binds
            let on_press = b.action.starts_with("__drag") || !b.flags.release;
            if on_press != pressed || (!b.flags.ignore_mods && b.mods != mods) {
                continue;
            }
            consume |= !b.flags.non_consuming;
            out.get_or_insert_with(Vec::new).push(b.action.clone());
        }
        (out, consume)
    }
}

#[cfg(test)]
mod env_tests {
    #[test]
    fn env_lines_read_with_vars() {
        let dir = std::env::temp_dir().join(format!("loom-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hyprland.conf"), "env = QT_QPA_PLATFORMTHEME,qt6ct\n$c = Vimix\nenv = XCURSOR_THEME,$c # comment\n").unwrap();
        let b = super::Binds::load(&dir.join("hyprland.conf"));
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(b.env, vec![("QT_QPA_PLATFORMTHEME".to_string(), "qt6ct".to_string()), ("XCURSOR_THEME".to_string(), "Vimix".to_string())]);
    }
}
