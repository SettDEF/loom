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
}

impl Default for Look {
    fn default() -> Self {
        Look { rounding: 0.0, rounding_power: 2.0 }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn init_lua() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("loom/init.lua")
}

/// Hyprland's config as `category:key = value` (nested `decoration { rounding = 18 }` blocks and
/// `decoration:rounding = 18` lines alike), following `source =`.
pub fn hyprland_values(conf: &std::path::Path) -> HashMap<String, String> {
    fn read(file: &std::path::Path, out: &mut HashMap<String, String>, depth: u32) {
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
            out.insert(key, v.to_string());
        }
    }
    let mut out = HashMap::new();
    read(conf, &mut out, 0);
    out
}

/// The defaults: Hyprland's values where it sets them.
pub fn from_hyprland() -> Look {
    let v = hyprland_values(&home().join(".config/hypr/hyprland.conf"));
    let num = |k: &str, d: f32| v.get(k).and_then(|s| s.parse().ok()).unwrap_or(d);
    let d = Look::default();
    Look { rounding: num("decoration:rounding", d.rounding), rounding_power: num("decoration:rounding_power", d.rounding_power) }
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
    // what Hyprland's config says, to read (loom.hyprland.rounding)
    let hypr = lua.create_table().map_err(e)?;
    hypr.set("rounding", base.rounding).map_err(e)?;
    hypr.set("rounding_power", base.rounding_power).map_err(e)?;
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
    Ok(Look { rounding: get("rounding", base.rounding)?.max(0.0), rounding_power: get("rounding_power", base.rounding_power)?.clamp(1.0, 10.0) })
}

impl crate::state::Loom {
    /// Watches init.lua (its folder, so a file saved by renaming counts too): applied on change.
    pub fn watch_config(&mut self) {
        use inotify::{Inotify, WatchMask};
        let path = init_lua();
        let Some(dir) = path.parent().map(std::path::Path::to_path_buf) else { return };
        let _ = std::fs::create_dir_all(&dir);
        let Ok(ino) = Inotify::init() else { return };
        if ino.watches().add(&dir, WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO | WatchMask::CREATE | WatchMask::DELETE).is_err() {
            return;
        }
        let Some(h) = self.loop_handle.clone() else { return };
        let src = smithay::reexports::calloop::generic::Generic::new(ino, smithay::reexports::calloop::Interest::READ, smithay::reexports::calloop::Mode::Level);
        let _ = h.insert_source(src, |_, ino, state: &mut crate::state::Loom| {
            let mut buf = [0u8; 1024];
            // Safety: the inotify fd is not dropped while the source lives
            let ino = unsafe { ino.get_mut() };
            let touched = ino.read_events(&mut buf).map(|evs| evs.into_iter().any(|e| e.name.is_some_and(|n| n == "init.lua"))).unwrap_or(false);
            if touched {
                state.reload_config();
            }
            Ok(smithay::reexports::calloop::PostAction::Continue)
        });
    }

    pub fn reload_config(&mut self) {
        let (look, err) = load();
        match err {
            Some(e) => eprintln!("loom: init.lua: {e} (the last good settings stay)"),
            None => {
                if look != self.look {
                    eprintln!("loom: settings: rounding {} power {}", look.rounding, look.rounding_power);
                    self.look = look;
                    self.dirty = true;
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
        let base = Look { rounding: 18.0, rounding_power: 2.0 };
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
