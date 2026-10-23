//! Hyprland's Lua config (hyprland.lua, 0.56+) read for its settings: run sandboxed (no io, os or
//! processes), `hl.config` and `hl.device` kept as `category:key` lines; any other `hl.` call does nothing.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use mlua::{HookTriggers, Lua, LuaOptions, StdLib, Table, Value, Variadic, VmState};

type Lines = Vec<(String, String)>;

thread_local! {
    /// the last read, reused for 2 s (getoption and the shaders ask often)
    static CACHE: RefCell<Option<(PathBuf, Instant, Lines)>> = const { RefCell::new(None) };
}

/// The next read runs the file again (a reload: it was just saved).
pub fn forget() {
    CACHE.with(|c| *c.borrow_mut() = None);
}

/// The settings `main` sets, in order; none when it does not run (said once a read).
pub fn lines(main: &Path) -> Lines {
    if let Some(l) = CACHE.with(|c| c.borrow().as_ref().filter(|(p, at, _)| p == main && at.elapsed() < Duration::from_secs(2)).map(|(_, _, l)| l.clone())) {
        return l;
    }
    let l = run(main).unwrap_or_else(|e| {
        eprintln!("loom: {}: {e}", main.display());
        Vec::new()
    });
    CACHE.with(|c| *c.borrow_mut() = Some((main.to_path_buf(), Instant::now(), l.clone())));
    l
}

/// `{ general = { col = { active_border = "…" } } }` as `general:col.active_border`; a list as its
/// values with spaces (a gradient), as the conf writes them.
fn flat(prefix: &str, v: &Value, out: &mut Lines) {
    let text = |v: &Value| match v {
        Value::Boolean(b) => Some(b.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.to_string_lossy()),
        _ => None,
    };
    match v {
        Value::Table(t) if t.raw_len() > 0 => out.push((prefix.to_string(), t.clone().sequence_values::<Value>().flatten().filter_map(|v| text(&v)).collect::<Vec<_>>().join(" "))),
        Value::Table(t) => {
            for (k, v) in t.clone().pairs::<String, Value>().flatten() {
                let key = match prefix {
                    "" => k,
                    p if p == "col" || p.ends_with(":col") => format!("{p}.{k}"),
                    p => format!("{p}:{k}"),
                };
                flat(&key, &v, out);
            }
        }
        v => out.extend(text(v).map(|t| (prefix.to_string(), t))),
    }
}

fn run(main: &Path) -> mlua::Result<Lines> {
    let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8, LuaOptions::default())?;
    // a config that loops forever is stopped
    let steps = Rc::new(RefCell::new(0u32));
    let s = steps.clone();
    lua.set_hook(HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
        *s.borrow_mut() += 1;
        if *s.borrow() > 2_000 { Err(mlua::Error::runtime("runs too long")) } else { Ok(VmState::Continue) }
    });
    let out: Rc<RefCell<Lines>> = Rc::default();
    let g = lua.globals();

    // anything: every field of it and every call to it give it back
    let meta = lua.create_table()?;
    meta.set("__index", lua.create_function(|_, (t, _): (Table, Value)| Ok(t))?)?;
    meta.set("__call", lua.create_function(|_, (t, _): (Table, Variadic<Value>)| Ok(t))?)?;
    let hl = lua.create_table()?;
    hl.set_metatable(Some(meta));
    let o = out.clone();
    hl.raw_set("config", lua.create_function(move |_, t: Value| Ok(flat("", &t, &mut o.borrow_mut())))?)?;
    let o = out.clone();
    hl.raw_set(
        "device",
        lua.create_function(move |_, t: Table| {
            let mut o = o.borrow_mut();
            o.push(("device:name".into(), t.get::<String>("name").unwrap_or_default()));
            for (k, v) in t.pairs::<String, Value>().flatten().filter(|(k, _)| k != "name") {
                flat(&format!("device:{k}"), &v, &mut o);
            }
            Ok(())
        })?,
    )?;
    // the submap hl.define_submap is filling ("global" outside one)
    let submap: Rc<RefCell<String>> = Rc::new(RefCell::new("global".into()));
    let sm = submap.clone();
    hl.raw_set(
        "define_submap",
        lua.create_function(move |_, (name, f): (String, mlua::Function)| {
            *sm.borrow_mut() = name;
            let r = f.call::<()>(());
            *sm.borrow_mut() = "global".into();
            r
        })?,
    )?;
    // hl.bind("SUPER + F", hl.dsp.exec_cmd("…"), { description = "…" }) as a conf bind line, keyed
    // `bind<flags>@<submap>` (`bindd` = "SUPER, F, …, exec, …"); a bind running a Lua function is left out
    let sm = submap.clone();
    let o = out.clone();
    hl.raw_set(
        "bind",
        lua.create_function(move |_, (keys, d, opts): (String, Value, Option<Table>)| {
            let Value::Table(d) = d else { return Ok(()) };
            let Ok(src) = d.get::<String>("__loom_dsp") else { return Ok(()) };
            let Some(steps) = crate::hypr_lua::translate(&src).filter(|s| s.len() == 1) else { return Ok(()) };
            let (disp, args) = steps[0].split_once(' ').unwrap_or((steps[0].as_str(), ""));
            let mut parts: Vec<&str> = keys.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
            let Some(key) = parts.pop() else { return Ok(()) };
            let on = |k: &str| opts.as_ref().is_some_and(|t| t.get::<bool>(k).unwrap_or(false));
            let mut flags: String = [("release", 'r'), ("repeating", 'e'), ("locked", 'l'), ("non_consuming", 'n'), ("ignore_mods", 'i'), ("transparent", 't'), ("mouse", 'm')].iter().filter(|(k, _)| on(k)).map(|(_, c)| *c).collect();
            let desc = opts.as_ref().and_then(|t| t.get::<String>("description").ok()).map(|s| s.replace(',', " "));
            let line = match &desc {
                Some(dsc) => {
                    flags.push('d');
                    format!("{}, {key}, {dsc}, {disp}, {args}", parts.join(" "))
                }
                None => format!("{}, {key}, {disp}, {args}", parts.join(" ")),
            };
            o.borrow_mut().push((format!("bind{flags}@{}", sm.borrow()), line));
            Ok(())
        })?,
    )?;
    g.set("hl", hl)?;
    // hl.dsp.window.close({ … }) gives back its own source, for the bind above to read
    lua.load(
        r##"
        local function ser(v)
            local t = type(v)
            if t == "string" then return string.format("%q", v) end
            if t == "number" or t == "boolean" then return tostring(v) end
            if t == "table" then
                local p = {}
                for k, x in pairs(v) do p[#p + 1] = (type(k) == "string" and (k .. " = ") or "") .. ser(x) end
                return "{ " .. table.concat(p, ", ") .. " }"
            end
            return "nil"
        end
        local function proxy(path)
            return setmetatable({}, {
                __index = function(_, k) return proxy(path .. "." .. k) end,
                __call = function(_, ...)
                    local a = {}
                    for i = 1, select("#", ...) do a[i] = ser((select(i, ...))) end
                    return { __loom_dsp = path .. "(" .. table.concat(a, ", ") .. ")" }
                end,
            })
        end
        rawset(hl, "dsp", proxy("hl.dsp"))
        "##,
    )
    .exec()?;
    g.set("print", lua.create_function(|_, _: Variadic<Value>| Ok(()))?)?;
    // os: the environment to read, the time; nothing that runs or writes
    let os = lua.create_table()?;
    os.set("getenv", lua.create_function(|_, k: String| Ok(std::env::var(k).ok()))?)?;
    os.set("time", lua.create_function(|_, _: Variadic<Value>| Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)))?)?;
    g.set("os", os)?;

    // require("lua.custom.env") is <config dir>/lua/custom/env.lua, run once
    let root = main.parent().map(Path::to_path_buf).unwrap_or_default();
    let loaded = lua.create_table()?;
    g.set(
        "require",
        lua.create_function(move |lua, name: String| {
            if let Ok(v) = loaded.get::<Value>(name.as_str()) {
                if !v.is_nil() {
                    return Ok(v);
                }
            }
            let rel = name.replace('.', "/");
            let file = [root.join(format!("{rel}.lua")), root.join(&rel).join("init.lua")].into_iter().find(|p| p.is_file());
            let Some(file) = file else { return Err(mlua::Error::runtime(format!("module {name} not found"))) };
            let src = std::fs::read_to_string(&file).map_err(mlua::Error::external)?;
            let v: Value = lua.load(src).set_name(format!("@{}", file.display())).call(())?;
            let v = if v.is_nil() { Value::Boolean(true) } else { v };
            loaded.set(name.as_str(), v.clone())?;
            Ok(v)
        })?,
    )?;

    let src = std::fs::read_to_string(main).map_err(mlua::Error::external)?;
    lua.load(src).set_name(format!("@{}", main.display())).exec()?;
    drop(steps);
    let lines = out.borrow().clone();
    Ok(lines)
}

#[cfg(test)]
mod tests {
    #[test]
    fn keeps_config_and_device_calls() {
        let dir = std::env::temp_dir().join(format!("loom-lua-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lua")).unwrap();
        std::fs::write(dir.join("lua/extra.lua"), "hl.device({ name = \"pad\", natural_scroll = true, sensitivity = -0.5 })\n").unwrap();
        std::fs::write(
            dir.join("hyprland.lua"),
            "hl.exec_cmd('touch /tmp/should-not-run')\nhl.bind('SUPER + Q', hl.dsp.window.close())\nhl.on('hyprland.start', function() end)\n\
             hl.config({ general = { col = { active_border = 'rgba(00000000)' }, gaps_in = 4 }, input = { touchpad = { scroll_factor = 1.5 } } })\n\
             hl.bind('SUPER + CTRL + F', hl.dsp.exec_cmd('qs -c ii ipc call firewall toggle'), { description = 'Firewall panel' })\n\
             hl.bind('SUPER + mouse:272', hl.dsp.global('quickshell:windowDrag'), { transparent = true })\n\
             hl.bind('SUPER + X', function() end)\n\
             require('lua.extra')\nif os.execute then error('os.execute is there') end\n",
        )
        .unwrap();
        let l = super::run(&dir.join("hyprland.lua")).unwrap();
        let has = |k: &str, v: &str| l.iter().any(|(a, b)| a == k && b == v);
        assert!(has("general:col.active_border", "rgba(00000000)") && has("general:gaps_in", "4") && has("input:touchpad:scroll_factor", "1.5"), "{l:?}");
        assert!(has("bindd@global", "SUPER CTRL, F, Firewall panel, exec, qs -c ii ipc call firewall toggle") && has("bindt@global", "SUPER, mouse:272, global, quickshell:windowDrag"), "{l:?}");
        assert!(!l.iter().any(|(k, v)| k.starts_with("bind") && v.contains(", X,")), "a Lua function bind is left out");
        assert!(has("device:name", "pad") && has("device:natural_scroll", "true") && has("device:sensitivity", "-0.5"), "{l:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
