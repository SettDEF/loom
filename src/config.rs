//! loom's settings, Neovim style: the defaults come from the user's Hyprland config (so loom looks
//! the same out of the box), then ~/.config/loom/init.lua runs over them:
//!
//! ```lua
//! loom.opt.rounding = 12               -- like vim.opt
//! loom.setup { rounding_power = 2.4 }  -- a table at once
//! if loom.hyprland.rounding > 10 then loom.opt.rounding = 10 end
//! ```
//!
//! The file is watched (inotify, no polling): a change is applied at once, an error keeps the last
//! good settings and is logged.

use std::{collections::HashMap, path::PathBuf};

/// What the windows look like.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// corner radius in logical pixels (Hyprland's decoration:rounding)
    pub rounding: f32,
    /// the corner curve: 2 = a circle, higher = squarer (Hyprland's decoration:rounding_power)
    pub rounding_power: f32,
    /// between two windows, and to the screen's edge (Hyprland's general:gaps_in, gaps_out)
    pub gaps_in: f32,
    pub gaps_out: f32,
    /// whose window animations: "loom" (its own, Hyprland-like) or "kwin" (KDE's effects, kwin_fx)
    pub animations: String,
    /// programs given loom's own X server (loom-x) instead of Xwayland, by name: loom.opt.own_x = { "xterm", "xeyes" }
    pub own_x: Vec<String>,
    /// the desktop drawn under the lock screen (Hyprland's misc:session_lock_xray): ii's lock is
    /// see-through and puts its wallpaper over the windows while locked
    pub lock_xray: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look { rounding: 0.0, rounding_power: 2.0, gaps_in: 5.0, gaps_out: 20.0, animations: "loom".into(), own_x: Vec::new(), lock_xray: false }
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn init_lua() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("loom/init.lua")
}

/// Hyprland's config as ordered `(category:key, value)` lines (nested `decoration { rounding = 18 }` blocks and
/// `decoration:rounding = 18` lines alike), following `source =`.
pub fn hyprland_lines(conf: &std::path::Path) -> Vec<(String, String)> {
    fn read(file: &std::path::Path, out: &mut Vec<(String, String)>, depth: u32) {
        let Ok(text) = std::fs::read_to_string(file) else { return };
        let dir = file.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        let mut stack: Vec<String> = Vec::new();
        for raw in text.lines() {
            let line = raw.split(" #").next().unwrap_or("").trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some(cat) = line.strip_suffix('{') {
                stack.push(cat.trim().to_string());
                continue;
            }
            if line == "}" {
                stack.pop();
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            if k == "source" && depth < 8 {
                let p = v.replace('~', &home().to_string_lossy());
                let p = if std::path::Path::new(&p).is_absolute() { PathBuf::from(p) } else { dir.join(p) };
                read(&p, out, depth + 1);
                continue;
            }
            let key = if stack.is_empty() { k.to_string() } else { format!("{}:{k}", stack.join(":")) };
            out.push((key, v.to_string()));
        }
    }
    let mut out = Vec::new();
    read(conf, &mut out, 0);
    // Hyprland 0.56 reads hyprland.lua instead when there is one: its settings win
    let lua = conf.with_file_name("hyprland.lua");
    if lua.is_file() {
        out.extend(crate::hypr_conf_lua::lines(&lua));
    }
    out
}

/// Hyprland's config as `category:key = value`, the last one winning.
pub fn hyprland_values(conf: &std::path::Path) -> HashMap<String, String> {
    hyprland_lines(conf).into_iter().collect()
}

pub fn hyprland_conf() -> PathBuf {
    home().join(".config/hypr/hyprland.conf")
}

/// The defaults: Hyprland's values where it sets them.
pub fn from_hyprland() -> Look {
    let v = hyprland_values(&home().join(".config/hypr/hyprland.conf"));
    let num = |k: &str, d: f32| v.get(k).and_then(|s| s.parse().ok()).unwrap_or(d);
    let d = Look::default();
    Look { rounding: num("decoration:rounding", d.rounding), rounding_power: num("decoration:rounding_power", d.rounding_power), gaps_in: num("general:gaps_in", d.gaps_in), gaps_out: num("general:gaps_out", d.gaps_out), animations: d.animations, own_x: d.own_x, lock_xray: v.get("misc:session_lock_xray").is_some_and(|s| matches!(s.trim(), "true" | "1" | "yes" | "on")) }
}

/// Key repeat as Hyprland's input:repeat_delay (ms) and repeat_rate (keys a second) say.
pub fn key_repeat() -> (i32, i32) {
    let v = hyprland_values(&home().join(".config/hypr/hyprland.conf"));
    let num = |k: &str, d: i32| v.get(k).and_then(|s| s.parse().ok()).unwrap_or(d);
    (num("input:repeat_delay", 600), num("input:repeat_rate", 25).max(1))
}

/// The keyboard as Hyprland's input:kb_model, kb_layout, kb_variant and kb_options say.
pub fn xkb_names() -> [String; 4] {
    let v = hyprland_values(&home().join(".config/hypr/hyprland.conf"));
    ["input:kb_model", "input:kb_layout", "input:kb_variant", "input:kb_options"].map(|k| v.get(k).map(|s| s.trim()).filter(|s| *s != "[[EMPTY]]").unwrap_or("").to_string())
}

/// Hyprland's defaults with init.lua run over them; the error, if the file has one.
pub fn load() -> (Look, Option<String>) {
    let base = from_hyprland();
    let path = init_lua();
    let Ok(src) = std::fs::read_to_string(&path) else { return (base, None) };
    match run_lua(&src, &base) {
        Ok(look) => (look, None),
        Err(e) => (base, Some(e)),
    }
}

fn run_lua(src: &str, base: &Look) -> Result<Look, String> {
    let lua = mlua::Lua::new();
    let e = |e: mlua::Error| e.to_string();
    let loom = lua.create_table().map_err(e)?;
    let opt = lua.create_table().map_err(e)?;
    opt.set("rounding", base.rounding).map_err(e)?;
    opt.set("rounding_power", base.rounding_power).map_err(e)?;
    opt.set("gaps_in", base.gaps_in).map_err(e)?;
    opt.set("gaps_out", base.gaps_out).map_err(e)?;
    opt.set("animations", base.animations.as_str()).map_err(e)?;
    opt.set("own_x", lua.create_sequence_from(base.own_x.iter().map(|s| s.as_str())).map_err(e)?).map_err(e)?;
    // what Hyprland's config says, to read (loom.hyprland.rounding)
    let hypr = lua.create_table().map_err(e)?;
    hypr.set("rounding", base.rounding).map_err(e)?;
    hypr.set("rounding_power", base.rounding_power).map_err(e)?;
    hypr.set("gaps_in", base.gaps_in).map_err(e)?;
    hypr.set("gaps_out", base.gaps_out).map_err(e)?;
    loom.set("hyprland", hypr).map_err(e)?;
    loom.set("opt", opt.clone()).map_err(e)?;
    // loom.setup { key = value, … }: the same as setting each loom.opt.key
    let o2 = opt.clone();
    let setup = lua
        .create_function(move |_, t: mlua::Table| {
            for pair in t.pairs::<String, mlua::Value>() {
                let (k, v) = pair?;
                o2.set(k, v)?;
            }
            Ok(())
        })
        .map_err(e)?;
    loom.set("setup", setup).map_err(e)?;
    lua.globals().set("loom", loom).map_err(e)?;
    lua.load(src).set_name("init.lua").exec().map_err(e)?;
    let get = |k: &str, d: f32| -> Result<f32, String> {
        match opt.get::<mlua::Value>(k).map_err(e)? {
            mlua::Value::Nil => Ok(d),
            mlua::Value::Integer(i) => Ok(i as f32),
            mlua::Value::Number(n) => Ok(n as f32),
            other => Err(format!("loom.opt.{k} must be a number, not {}", other.type_name())),
        }
    };
    Ok(Look {
        rounding: get("rounding", base.rounding)?.max(0.0),
        rounding_power: get("rounding_power", base.rounding_power)?.clamp(1.0, 10.0),
        gaps_in: get("gaps_in", base.gaps_in)?.max(0.0),
        gaps_out: get("gaps_out", base.gaps_out)?.max(0.0),
        lock_xray: base.lock_xray,
        animations: match opt.get::<mlua::Value>("animations").map_err(e)? {
            mlua::Value::String(s) if matches!(s.to_str().map(|x| x.to_string()).unwrap_or_default().as_str(), "loom" | "kwin") => s.to_str().map(|x| x.to_string()).unwrap_or_default(),
            mlua::Value::Nil => base.animations.clone(),
            other => return Err(format!("loom.opt.animations must be \"loom\" or \"kwin\", not {}", other.type_name())),
        },
        own_x: match opt.get::<mlua::Value>("own_x").map_err(e)? {
            mlua::Value::Table(t) => t.sequence_values::<String>().collect::<Result<Vec<_>, _>>().map_err(|_| "loom.opt.own_x must be a list of program names".to_string())?,
            mlua::Value::Nil => base.own_x.clone(),
            other => return Err(format!("loom.opt.own_x must be a list of program names, not {}", other.type_name())),
        },
    })
}

impl crate::state::Loom {
    /// Watches init.lua (its folder, so a file saved by renaming counts too): applied on change.
    pub fn watch_config(&mut self) {
        use inotify::{Inotify, WatchMask};
        let path = init_lua();
        let Some(dir) = path.parent().map(std::path::Path::to_path_buf) else { return };
        let _ = std::fs::create_dir_all(&dir);
        let Ok(ino) = Inotify::init() else { return };
        let mask = WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO | WatchMask::CREATE | WatchMask::DELETE;
        if ino.watches().add(&dir, mask).is_err() {
            return;
        }
        // ii-decor's files too (its title bars): the Hyprland folder and ii-decor.d
        let hypr = home().join(".config/hypr");
        let _ = ino.watches().add(&hypr, mask);
        let _ = ino.watches().add(hypr.join("ii-decor.d"), mask);
        // the rest of the Hyprland config: hyprland/, custom/, and lua/ with its folders (inotify is not recursive)
        let _ = ino.watches().add(hypr.join("hyprland"), mask);
        let _ = ino.watches().add(hypr.join("custom"), mask);
        let mut dirs = vec![hypr.join("lua")];
        while let Some(d) = dirs.pop() {
            let _ = ino.watches().add(&d, mask);
            dirs.extend(std::fs::read_dir(&d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
        let Some(h) = self.loop_handle.clone() else { return };
        let src = smithay::reexports::calloop::generic::Generic::new(ino, smithay::reexports::calloop::Interest::READ, smithay::reexports::calloop::Mode::Level);
        let _ = h.insert_source(src, |_, ino, state: &mut crate::state::Loom| {
            let mut buf = [0u8; 1024];
            // Safety: the inotify fd is not dropped while the source lives
            let ino = unsafe { ino.get_mut() };
            let names: Vec<String> = ino.read_events(&mut buf).map(|evs| evs.into_iter().filter_map(|e| e.name.map(|n| n.to_string_lossy().to_string())).collect()).unwrap_or_default();
            // a config file saved (not an editor's temp file or a backup). What the shell rewrites
            // often is read on its own: ii-decor's json (the title bars) and its Glass Lua files
            // (blur and rules); anything else is the whole config, read again once.
            let config = |n: &&String| !n.starts_with('.') && !n.contains(".bak") && (n.ends_with(".conf") || n.ends_with(".lua") || n.ends_with(".json"));
            let (mut decor, mut glass, mut all) = (false, false, false);
            for n in names.iter().filter(config) {
                match n.as_str() {
                    n if n.ends_with(".json") => decor = true,
                    n if n.starts_with("glass_") => glass = true,
                    _ => all = true,
                }
            }
            if all {
                state.schedule_reload();
            } else {
                if glass {
                    state.blur = crate::blur::Config::load();
                    state.win_rules = crate::winrules::Rules::load();
                    state.dirty = true;
                }
                if decor {
                    state.decor.borrow_mut().reload();
                    state.relayout();
                }
            }
            Ok(smithay::reexports::calloop::PostAction::Continue)
        });
    }

    /// A reload soon: the saves of one moment (an editor, the shell writing several files) become one.
    pub fn schedule_reload(&mut self) {
        if std::mem::replace(&mut self.reload_pending, true) {
            return;
        }
        let Some(h) = self.loop_handle.clone() else { return };
        let t = smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_millis(150));
        let _ = h.insert_source(t, |_, _, state: &mut crate::state::Loom| {
            state.reload_all();
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
    }

    /// The whole config read again, as Hyprland's reload: binds, looks, rules, animations, the
    /// title bars, input devices, the keyboard; values set by `keyword` stay. Tells the shell.
    pub fn reload_all(&mut self) {
        self.reload_pending = false;
        crate::hypr_conf_lua::forget();
        let submap = std::mem::take(&mut self.binds.submap);
        self.binds = crate::binds::Binds::load(&hyprland_conf());
        self.binds.submap = if submap.is_empty() { "global".into() } else { submap };
        self.reload_config();
        self.blur = crate::blur::Config::load();
        self.layer_cfg = crate::layer_anim::Config::load();
        self.gesture_cfg = crate::gestures::Config::load();
        self.win_rules = crate::winrules::Rules::load();
        self.frames_cfg = crate::frames::Config::load();
        self.shader_conf = screen_shader_conf();
        self.decor.borrow_mut().reload();
        let lines = self.input_lines();
        self.inputs.reapply(&lines);
        self.apply_xkb();
        let (delay, rate) = key_repeat();
        self.seat.get_keyboard().expect("keyboard").change_repeat_info(rate, delay);
        self.relayout();
        self.dirty = true;
        eprintln!("loom: config read again ({} binds)", self.binds.binds.len());
        self.hypr_event("configreloaded>>");
        self.greeter_snapshot();
    }

    pub fn reload_config(&mut self) {
        let (look, err) = load();
        match err {
            Some(e) => eprintln!("loom: init.lua: {e} (the last good settings stay)"),
            None => {
                if look != self.look {
                    eprintln!("loom: settings: rounding {} power {} gaps {}/{}", look.rounding, look.rounding_power, look.gaps_in, look.gaps_out);
                    let gaps = (look.gaps_in, look.gaps_out) != (self.look.gaps_in, self.look.gaps_out);
                    self.look = look;
                    self.dirty = true;
                    self.apply_animations_setting();
                    if gaps {
                        self.relayout();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_sets_options_both_ways() {
        let base = Look { rounding: 18.0, rounding_power: 2.0, gaps_in: 4.0, gaps_out: 5.0, animations: "loom".into(), own_x: Vec::new(), lock_xray: false };
        assert_eq!(run_lua("loom.opt.rounding = 12", &base).unwrap().rounding, 12.0);
        let l = run_lua("loom.setup { rounding = 6, rounding_power = 3 }", &base).unwrap();
        assert_eq!((l.rounding, l.rounding_power), (6.0, 3.0));
        assert_eq!(run_lua("if loom.hyprland.rounding > 10 then loom.opt.rounding = 10 end", &base).unwrap().rounding, 10.0);
        assert!(run_lua("loom.opt.rounding = 'big'", &base).is_err());
        assert!(run_lua("this is not lua", &base).is_err());
        assert_eq!(run_lua("", &base).unwrap(), base, "an empty file keeps Hyprland's values");
    }

    #[test]
    fn hyprland_blocks_and_lines() {
        let dir = std::env::temp_dir().join(format!("loom-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.conf"), "decoration {\n    rounding_power = 2.4\n    blur {\n        size = 8\n    }\n    rounding = 18 # comment\n}\n").unwrap();
        std::fs::write(dir.join("hyprland.conf"), "source = b.conf\ngeneral:gaps_in = 5\n").unwrap();
        let v = hyprland_values(&dir.join("hyprland.conf"));
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(v.get("decoration:rounding").map(String::as_str), Some("18"));
        assert_eq!(v.get("decoration:rounding_power").map(String::as_str), Some("2.4"));
        assert_eq!(v.get("decoration:blur:size").map(String::as_str), Some("8"));
        assert_eq!(v.get("general:gaps_in").map(String::as_str), Some("5"));
    }
}

#[cfg(test)]
mod anim_opt {
    #[test]
    fn animations_option() {
        let base = super::Look { rounding: 0.0, rounding_power: 2.0, gaps_in: 4.0, gaps_out: 5.0, animations: "loom".into(), own_x: Vec::new(), lock_xray: false };
        assert_eq!(super::run_lua("loom.opt.animations = 'kwin'", &base).unwrap().animations, "kwin");
        assert!(super::run_lua("loom.opt.animations = 'wobbly'", &base).is_err());
    }
}

/// Hyprland's xwayland:force_zero_scaling: X programs draw at the screen's own pixels (sharp; they
/// size themselves, e.g. with Xft.dpi) instead of at 1 and scaled up by loom (blurry).
pub fn xwayland_zero_scaling() -> bool {
    let v = hyprland_values(&home().join(".config/hypr/hyprland.conf"));
    v.get("xwayland:force_zero_scaling").is_some_and(|s| matches!(s.trim(), "true" | "1" | "yes" | "on"))
}

/// decoration:screen_shader as the config says ("" for none).
pub fn screen_shader_conf() -> String {
    hyprland_values(&hyprland_conf()).get("decoration:screen_shader").cloned().unwrap_or_default()
}
