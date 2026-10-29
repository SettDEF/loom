//! Hyprland's IPC, as Hyprland serves it: `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`
//! `.socket.sock` answers requests (hyprctl, the shell's Quickshell.Hyprland: `j/monitors`,
//! `j/workspaces`, `j/clients`, `dispatch …`, `keyword …`, `[[BATCH]]`), `.socket2.sock` streams
//! events (`openwindow>>…`, `workspacev2>>…`). Programs started in loom get its signature, so
//! `hyprctl` there talks to loom, never to a Hyprland running outside.

use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    time::Duration,
};

use serde_json::{json, Value};
use smithay::reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction};

use crate::state::Loom;

pub struct HyprIpc {
    pub signature: String,
    pub dir: PathBuf,
    /// the event socket's listeners
    pub listeners: Vec<UnixStream>,
}

impl Drop for HyprIpc {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Opens both sockets; requests and new listeners are taken in the event loop.
pub fn start(handle: &LoopHandle<'static, Loom>) -> Result<HyprIpc, Box<dyn std::error::Error>> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // after `loom restart` the same one: the shell's sockets come back at the same path
    let signature = crate::restart::signature().unwrap_or_else(|| format!("loom_{}_{secs}", std::process::id()));
    let dir = PathBuf::from(runtime).join("hypr").join(&signature);
    let _ = std::fs::remove_file(dir.join(".socket.sock"));
    let _ = std::fs::remove_file(dir.join(".socket2.sock"));
    // folders of loom runs that died without cleaning up (killed tests): their pid is gone
    if let Ok(rd) = std::fs::read_dir(dir.parent().unwrap_or(&dir)) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(pid) = n.strip_prefix("loom_").and_then(|r| r.split('_').next()).and_then(|p| p.parse::<u32>().ok()) {
                if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }
    std::fs::create_dir_all(&dir)?;
    let requests = UnixListener::bind(dir.join(".socket.sock"))?;
    requests.set_nonblocking(true)?;
    let events = UnixListener::bind(dir.join(".socket2.sock"))?;
    events.set_nonblocking(true)?;
    handle.insert_source(Generic::new(requests, Interest::READ, Mode::Level), |_, l, state: &mut Loom| {
        // each request read when it arrives (its own source): a slow client never stalls loom
        while let Ok((s, _)) = l.accept() {
            let _ = s.set_nonblocking(true);
            let Some(h) = state.loop_handle.clone() else { continue };
            let mut got: Vec<u8> = Vec::new();
            let _ = h.insert_source(Generic::new(s, Interest::READ, Mode::Level), move |_, s, state: &mut Loom| {
                // Safety: the stream is not dropped while the source lives
                let s = unsafe { s.get_mut() };
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            got.extend_from_slice(&buf[..n]);
                            if n < buf.len() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(PostAction::Continue),
                        Err(_) => return Ok(PostAction::Remove),
                    }
                }
                let reply = state.hypr_request(&String::from_utf8_lossy(&got));
                let _ = s.set_nonblocking(false);
                let _ = s.set_write_timeout(Some(Duration::from_millis(50)));
                let _ = s.write_all(reply.as_bytes());
                Ok(PostAction::Remove)
            });
        }
        Ok(PostAction::Continue)
    })?;
    handle.insert_source(Generic::new(events, Interest::READ, Mode::Level), |_, l, state: &mut Loom| {
        while let Ok((s, _)) = l.accept() {
            let _ = s.set_nonblocking(true);
            if let Some(h) = state.hypr.as_mut() {
                h.listeners.push(s);
            }
        }
        Ok(PostAction::Continue)
    })?;
    eprintln!("loom: HYPRLAND_INSTANCE_SIGNATURE={signature}");
    let listeners: Vec<UnixStream> = crate::restart::hypr_listeners();
    for s in &listeners {
        let _ = s.set_nonblocking(true);
    }
    Ok(HyprIpc { signature, dir, listeners })
}

impl Loom {
    /// An event to every listener of the event socket (a listener that went is dropped).
    pub fn hypr_event(&mut self, line: &str) {
        let Some(h) = self.hypr.as_mut() else { return };
        let msg = format!("{line}\n");
        // a listener too slow to take it keeps its place (it misses this one); a gone one is dropped
        h.listeners.retain_mut(|s| s.write_all(msg.as_bytes()).map_or_else(|e| e.kind() == std::io::ErrorKind::WouldBlock, |_| true));
    }

    /// One request, as hyprctl writes it: flags before a `/` (`j/monitors`), `[[BATCH]]a;b`.
    pub fn hypr_request(&mut self, req: &str) -> String {
        let req = req.trim();
        if let Some(batch) = req.strip_prefix("[[BATCH]]") {
            return batch.split(';').filter(|c| !c.trim().is_empty()).map(|c| self.hypr_request(c)).collect::<Vec<_>>().join("\n\n");
        }
        let (flags, cmd) = match req.split_once('/') {
            Some((f, c)) if !f.contains(' ') && f.len() <= 4 => (f, c),
            _ => ("", req),
        };
        let json = flags.contains('j');
        let (name, arg) = cmd.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((cmd, ""));
        let value = match name {
            "monitors" => Some(self.hypr_monitors()),
            "workspaces" => Some(Value::Array(self.workspace_ids().into_iter().map(|id| self.hypr_workspace(id)).collect())),
            "activeworkspace" => Some(self.hypr_workspace(self.active_ws)),
            "clients" => Some(Value::Array(self.managed.iter().map(|m| self.hypr_client(m)).collect())),
            "activewindow" => Some(self.focused.as_ref().and_then(|w| self.managed_of(w)).map(|m| self.hypr_client(m)).unwrap_or(json!({}))),
            "layers" => Some(self.hypr_layers()),
            // loom's own: who has the keyboard and the pointer, what grabs and drags are open (to
            // see why a window takes no input)
            "loomstate" => Some(self.loom_state()),
            "devices" => {
                let [model, layout, variant, options] = self.xkb_now();
                let (active, ..) = self.active_layout();
                let mods = self.seat.get_keyboard().expect("keyboard").modifier_state();
                Some(json!({ "mice": self.inputs.all.iter().filter(|d| d.pointer).enumerate().map(|(i, d)| json!({ "address": format!("0x{:x}", i + 1), "name": d.name, "defaultSpeed": 0.0, "scrollFactor": d.scroll })).collect::<Vec<_>>(), "keyboards": [{ "address": "0x0", "name": "loom-keyboard", "rules": "", "model": model, "layout": if layout.is_empty() { "us".into() } else { layout }, "variant": variant, "options": options, "active_keymap": active, "capsLock": mods.caps_lock, "numLock": mods.num_lock, "main": true }], "tablets": [], "touch": [], "switches": [] }))
            }
            "version" => Some(json!({ "branch": "loom", "commit": env!("CARGO_PKG_VERSION"), "version": "0.56.0", "dirty": false, "commit_message": "loom", "commit_date": "", "tag": "loom", "commits": "0", "buildAquamarine": "", "buildHyprlang": "", "buildHyprutils": "", "buildHyprcursor": "", "buildHyprgraphics": "", "flags": [] })),
            "status" => Some(json!({ "compositor": "loom", "version": env!("CARGO_PKG_VERSION") })),
            "globalshortcuts" => Some(Value::Array(self.proto.shortcuts.iter().map(|s| json!({ "name": format!("{}:{}", s.app_id, s.id), "description": s.description })).collect())),
            "binds" => Some(Value::Array(self.binds.list())),
            "instances" => Some(json!([])),
            "submap" => return if self.binds.submap == "global" { "default".into() } else { self.binds.submap.clone() },
            "getoption" => Some(self.hypr_option(arg)),
            // loom has no plugins: ii-decor's title bars are built in
            "plugin" => match arg.split_whitespace().next().unwrap_or("list") {
                "list" => return if json { "[]".into() } else { "no plugins loaded".into() },
                _ => return "loom has no plugins: its title bars (ii-decor) are built in".into(),
            },
            "iidecor" => match arg.split_whitespace().next().unwrap_or("status") {
                "reload" => {
                    self.decor.borrow_mut().reload();
                    self.relayout();
                    return "ok".into();
                }
                // always JSON, as the plugin prints it (the shell parses it without -j)
                "status" => return json!({ "plugin": "loom (built in)", "loaded": true }).to_string(),
                _ => return "ok".into(),
            },
            "cursorpos" => Some(json!({ "x": self.pointer_at.x as i32, "y": self.pointer_at.y as i32 })),
            "splash" => return "loom".into(),
            "dispatch" => return self.hypr_dispatch(arg),
            "keyword" => {
                // binds added or taken away at run time; other options accepted
                let (k, v) = arg.split_once(' ').map(|(a, b)| (a.trim(), b.trim())).unwrap_or((arg, ""));
                if k == "unbind" {
                    self.binds.remove(v);
                } else if let Some(flags) = k.strip_prefix("bind") {
                    let sub = self.binds.submap.clone();
                    self.binds.add(flags, v, &sub);
                } else {
                    self.set_keyword(k, v);
                }
                return "ok".into();
            }
            "eval" => return self.hypr_eval(arg),
            "switchxkblayout" => return self.switch_layout(arg),
            "reload" => {
                self.reload_all();
                return "ok".into();
            }
            "setcursor" | "notify" | "dismissnotify" => return "ok".into(),
            _ => None,
        };
        match value {
            Some(v) if json => serde_json::to_string_pretty(&v).unwrap_or_default(),
            Some(v) => plain(&v),
            None => format!("unknown request: {name}"),
        }
    }

    /// `dispatch NAME ARG`: the dispatchers the shell and scripts use.
    pub fn hypr_dispatch(&mut self, arg: &str) -> String {
        // Hyprland 0.56's Lua form (the shell's HyprDispatch.qml): read, carried out as classic dispatches
        if arg.trim_start().starts_with("hl.dsp.") {
            let Some(steps) = crate::hypr_lua::translate(arg) else { return format!("Invalid dispatcher {arg}") };
            for s in steps {
                let r = self.hypr_dispatch(&s);
                if r != "ok" {
                    return r;
                }
            }
            return "ok".into();
        }
        let (d, a) = arg.split_once(' ').map(|(x, y)| (x, y.trim())).unwrap_or((arg, ""));
        let ws_of = |s: &Loom, a: &str| -> Option<i32> {
            let a = a.trim();
            if a == "special" || a.starts_with("special:") {
                return Some(crate::wm::SPECIAL);
            }
            if let Some(r) = a.strip_prefix("e").or_else(|| a.strip_prefix("r")).filter(|r| r.starts_with('+') || r.starts_with('-')) {
                return r.parse::<i32>().ok().map(|n| (s.active_ws + n).max(1));
            }
            if a.starts_with('+') || a.starts_with('-') {
                return a.parse::<i32>().ok().map(|n| (s.active_ws + n).max(1));
            }
            a.parse().ok()
        };
        match d {
            "workspace" => match ws_of(self, a) {
                Some(n) => self.switch_workspace(n),
                None => return format!("invalid workspace {a}"),
            },
            // `N` or `N, address:0x…` (the shell names the window it means; without, the focused one)
            // test input for a headless loom (scripts/zen-input-probe.sh): `click X,Y`, `rclick X,Y`,
            // `move X,Y`, `down`, `up`, `drag X,Y,X2,Y2`, `keys TEXT` (as --keys). Refused on a real session.
            "loomtest" => {
                if !self.headless {
                    return "loomtest: only in a headless loom".into();
                }
                let (what, rest) = a.split_once(' ').map(|(w, r)| (w, r.trim())).unwrap_or((a, ""));
                let nums: Vec<f64> = rest.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                use crate::input::Action as A;
                match (what, nums.as_slice()) {
                    ("click", [x, y]) => self.test_input.push_back(A::Click(*x, *y)),
                    ("rclick", [x, y]) => self.test_input.push_back(A::RightClick(*x, *y)),
                    ("move", [x, y]) => self.test_input.push_back(A::Move(*x, *y)),
                    ("down", []) => self.test_input.push_back(A::Button(true)),
                    ("up", []) => self.test_input.push_back(A::Button(false)),
                    ("drag", [x, y, x2, y2]) => {
                        self.test_input.push_back(A::Move(*x, *y));
                        self.test_input.push_back(A::Button(true));
                        for i in 1..=8 {
                            let f = i as f64 / 8.0;
                            self.test_input.push_back(A::Move(x + (x2 - x) * f, y + (y2 - y) * f));
                        }
                        self.test_input.push_back(A::Button(false));
                    }
                    ("keys", _) => self.test_input.extend(crate::input::parse_keys(rest)),
                    _ => return format!("loomtest: {a}?"),
                }
            }
            "movetoworkspace" | "movetoworkspacesilent" => match ws_of(self, a.split(',').next().unwrap_or(a).trim()) {
                Some(n) => {
                    let named = a.split_once(',').map(|(_, s)| s.trim()).filter(|s| !s.is_empty());
                    let w = match named {
                        Some(sel) => match self.find_window(sel) {
                            Some(m) => Some(m.window),
                            None => return format!("no such window: {sel}"),
                        },
                        None => self.focused.clone(),
                    };
                    if let Some(w) = w {
                        self.move_window_to_workspace(&w, n);
                    }
                    if d == "movetoworkspace" {
                        self.switch_workspace(n);
                    }
                }
                None => return format!("invalid workspace {a}"),
            },
            "exec" => {
                // `[workspace N silent] CMD`: its first window goes there
                if let Some(rest) = a.strip_prefix('[') {
                    let (rule, cmd) = rest.split_once(']').unwrap_or((rest, ""));
                    let mut w = rule.split_whitespace();
                    if w.next() == Some("workspace") {
                        if let Some(n) = w.next().and_then(|n| n.parse::<i32>().ok()) {
                            let silent = w.next() == Some("silent");
                            if let Some(pid) = self.spawn(cmd.trim()) {
                                self.exec_rules.push((pid, n, silent));
                            }
                        }
                    } else {
                        self.spawn(cmd.trim());
                    }
                } else {
                    self.spawn(a);
                }
            }
            "killactive" => {
                if let Some(w) = self.focused.clone() {
                    close_window(&w);
                }
            }
            // a window by address:, class:, title:, pid: (or the focused one): asked to close; killwindow kills its program
            "closewindow" | "killwindow" => {
                let w = match a.trim() {
                    "" | "activewindow" => self.focused.clone().and_then(|w| self.managed_of(&w).cloned()),
                    sel => self.find_window(sel),
                };
                let Some(m) = w else { return format!("No such window found: {a}") };
                if d == "killwindow" && m.pid > 0 {
                    // SAFETY: a signal to a program loom shows
                    unsafe { libc::kill(m.pid, libc::SIGKILL) };
                } else {
                    close_window(&m.window);
                }
            }
            "togglefloating" => {
                if let Some(w) = self.focused.clone() {
                    if let Some(m) = self.managed.iter_mut().find(|m| m.window == w) {
                        m.floating = !m.floating;
                        // pinned is for floating ones: a tile pinned went along to every workspace
                        m.pinned &= m.floating;
                    }
                    self.relayout();
                }
            }
            "focuswindow" => match self.find_window(a) {
                Some(m) => {
                    self.switch_workspace(m.ws);
                    self.focus_window(&m.window);
                }
                None => return format!("No such window found: {a}"),
            },
            "movewindowpixel" => {
                // `exact X Y`: a floating window to that place
                let p: Vec<i32> = a.split_whitespace().filter_map(|v| v.parse().ok()).collect();
                if let (Some(w), [x, y]) = (self.focused.clone(), p.as_slice()) {
                    if self.managed_of(&w).is_some_and(|m| m.floating) {
                        self.space.map_element(w.clone(), (*x, *y), true);
                        if let Some(xs) = w.x11_surface() {
                            let _ = xs.configure(self.x_moved(xs, (*x, *y).into()));
                        }
                        self.dirty = true;
                    }
                }
            }
            "movecursor" => {
                let p: Vec<f64> = a.split_whitespace().filter_map(|v| v.parse().ok()).collect();
                if let [x, y] = p.as_slice() {
                    let at = self.clamp_to_outputs((*x, *y).into());
                    let t = self.now_ms();
                    self.pointer_move(at, t);
                }
            }
            // the AwesomeWM layouts' messages (suit, next, mwfact, nmaster, ncol, swapmaster, reset)
            "layoutmsg" => {
                // "ws:NAME msg" for a workspace that is not the focused one (the bar's workspace menu)
                let (ws, a) = match a.strip_prefix("ws:").and_then(|r| r.split_once(' ')) {
                    Some((w, rest)) => (w.to_string(), rest.trim()),
                    None => (self.active_ws.to_string(), a),
                };
                match self.layouts.message(&ws, a) {
                    Ok(swap) => {
                        if swap {
                            self.swap_master();
                        }
                        self.relayout();
                    }
                    Err(e) => return e,
                }
            }
            "global" => {
                if !self.global_shortcut(a, true) {
                    return format!("no global shortcut {a}");
                }
            }
            "submap" => {
                let name = if a.is_empty() || a == "reset" { "global" } else { a };
                self.binds.submap = name.to_string();
                self.hypr_event(&format!("submap>>{}", if name == "global" { "" } else { name }));
            }
            "movefocus" => self.move_focus(a),
            "cyclenext" => self.cycle_focus(!a.contains("prev")),
            "bringactivetotop" | "alterzorder" => {
                if let Some(w) = self.focused.clone() {
                    self.space.raise_element(&w, true);
                    self.dirty = true;
                }
            }
            // "fullscreen 1" (Hyprland's maximize, the overview menu's and Super+D) and fullscreenstate maximize in the
            // layout; "fullscreen" / "fullscreen 0" cover the screen
            "fullscreen" if a.trim() == "1" => { if let Some(w) = self.focused.clone() { self.toggle_maximize(&w); self.relayout() } }
            "fullscreenstate" => { if let Some(w) = self.focused.clone() { self.toggle_maximize(&w); self.relayout() } }
            "fullscreen" => self.toggle_fullscreen(),
            "movewindow" => self.move_window(a),
            "pin" => self.toggle_pin(),
            "resizeactive" => self.resize_active(a),
            // resizewindowpixel SIZE[, WINDOW]: as resizeactive, on that window (focused first)
            "resizewindowpixel" => {
                let (size, sel) = a.split_once(',').map(|(s, w)| (s.trim(), w.trim())).unwrap_or((a, ""));
                if !sel.is_empty() && sel != "activewindow" {
                    let Some(m) = self.find_window(sel) else { return format!("No such window found: {sel}") };
                    self.focus_window(&m.window);
                }
                self.resize_active(size);
            }
            // setfloating / settiled [WINDOW]: floating or tiled, whatever it was
            "setfloating" | "settiled" => {
                let sel = a.trim().trim_start_matches(',').trim();
                let w = match sel {
                    "" | "activewindow" => self.focused.clone(),
                    sel => self.find_window(sel).map(|m| m.window),
                };
                let Some(w) = w else { return format!("No such window found: {sel}") };
                let float = d == "setfloating";
                if let Some(m) = self.managed.iter_mut().find(|m| m.window == w) {
                    m.floating = float;
                    m.pinned &= float;
                }
                self.relayout();
            }
            "togglespecialworkspace" => self.toggle_special(),
            "sendshortcut" => self.send_shortcut(a),
            "exit" => self.signal.stop(),
            "loomdebug" => match a.trim() {
                "cursor" => {
                    self.debug_cursor = !self.debug_cursor;
                    return format!("cursor log {}", if self.debug_cursor { "on" } else { "off" });
                }
                "clicks" => {
                    self.debug_clicks = !self.debug_clicks;
                    return format!("click log {}", if self.debug_clicks { "on" } else { "off" });
                }
                // a screen unplugged and plugged back, without a real one (headless, nested only)
                u if u.starts_with("unplug ") || u.starts_with("replug ") => {
                    if self.tty.is_some() {
                        return "unplug/replug: headless and nested only".into();
                    }
                    let name = u[7..].trim().to_string();
                    if u.starts_with("unplug ") {
                        let Some(o) = self.output_named(&name) else { return format!("no screen {name}") };
                        let at = self.space.output_geometry(&o).map(|g| g.loc).unwrap_or_default();
                        self.remove_output(&name);
                        if let Some(g) = self.output_globals.remove(&name) {
                            self.dh.remove_global::<Loom>(g);
                        }
                        self.parked.push((o, at));
                        return format!("{name} unplugged");
                    }
                    let Some(i) = self.parked.iter().position(|(o, _)| o.name() == name) else { return format!("{name} is not unplugged") };
                    let (o, at) = self.parked.remove(i);
                    let g = o.create_global::<Loom>(&self.dh);
                    self.output_globals.insert(name.clone(), g);
                    self.add_output(o, at);
                    return format!("{name} back");
                }
                _ => return "loomdebug cursor | clicks | unplug NAME | replug NAME".into(),
            },
            // loom's own: the new binary in place, the session kept (restart.rs)
            "loomrestart" => {
                // X programs (Bitwig, Wine) lose their window in a restart, and some kept running without
                // one (a second Bitwig then had no window): they are closed first
                let xs: Vec<String> = self.managed.iter().filter(|m| m.window.x11_surface().is_some()).map(|m| self.class_of(&m.window)).collect();
                if !xs.is_empty() {
                    // asked to close as their close button would (Bitwig can ask to save); the restart
                    // follows when the last one is gone (x_gone)
                    for m in &self.managed {
                        if let Some(x) = m.window.x11_surface() {
                            let _ = x.close();
                        }
                    }
                    self.restart_pending = true;
                    if a.trim() == "force" {
                        // force: what has not closed after 3 s is ended, then the restart
                        let pids: Vec<i32> = self.managed.iter().filter(|m| m.window.x11_surface().is_some() && m.pid > 0).map(|m| m.pid).collect();
                        if let Some(h) = self.loop_handle.clone() {
                            let _ = h.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_secs(3)), move |_, _, s: &mut Loom| {
                                for p in &pids {
                                    // Safety: plain kill(2) on a pid that ran an X window here
                                    unsafe { libc::kill(*p, libc::SIGTERM) };
                                }
                                s.restart = true;
                                s.signal.stop();
                                smithay::reexports::calloop::timer::TimeoutAction::Drop
                            });
                        }
                        return format!("closing X programs ({}), ending what is left in 3 s, then restarting", xs.join(", "));
                    }
                    return format!("closing X programs first ({}); loom restarts when they are gone (loom restart --force: ends them after 3 s)", xs.join(", "));
                }
                self.restart = true;
                self.signal.stop();
            }
            "dpms" => return self.dpms(a),
            "focusmonitor" => match self.monitor_arg(a) {
                Some(o) => {
                    self.focus_output(&o);
                    self.pointer_to_middle(&o);
                    self.focus_next(self.active_ws);
                }
                None => return format!("no monitor {a}"),
            },
            "moveworkspacetomonitor" => {
                let (w, m) = a.split_once(' ').unwrap_or((a, ""));
                match (ws_of(self, w), self.monitor_arg(m)) {
                    (Some(n), Some(o)) => self.workspace_to_monitor(n, &o),
                    _ => return format!("moveworkspacetomonitor {a}: which workspace / monitor?"),
                }
            }
            "movecurrentworkspacetomonitor" => match self.monitor_arg(a) {
                Some(o) => {
                    let n = self.active_ws;
                    self.workspace_to_monitor(n, &o);
                }
                None => return format!("no monitor {a}"),
            },
            "swapactiveworkspaces" => {
                let (x, y) = a.split_once(' ').unwrap_or((a, ""));
                match (self.monitor_arg(x), self.monitor_arg(y)) {
                    (Some(p), Some(q)) => self.swap_active_workspaces(&p, &q),
                    _ => return format!("swapactiveworkspaces {a}: which monitors?"),
                }
            }
            _ => return format!("Invalid dispatcher {d}"),
        }
        "ok".into()
    }

    /// A program started in loom: its display and Hyprland signature are loom's.
    pub fn spawn(&self, cmd: &str) -> Option<u32> {
        let mut c = std::process::Command::new("sh");
        // one of init.lua's own_x programs: loom's own X server, waited for
        let own = self.own_x_for(cmd);
        let cmd = match own {
            Some(n) => crate::own_x::wait_for(n, cmd),
            None => cmd.to_string(),
        };
        c.arg("-c").arg(&cmd).env_remove("DISPLAY");
        if let Some(name) = &self.socket_name {
            c.env("WAYLAND_DISPLAY", name);
        }
        if let Some(h) = &self.hypr {
            c.env("HYPRLAND_INSTANCE_SIGNATURE", &h.signature);
        }
        if let Some(n) = own.or(self.x_display) {
            c.env("DISPLAY", format!(":{n}"));
        }
        // the cursor KDE's settings chose
        c.env("XCURSOR_THEME", crate::tty::cursor_theme()).env("XCURSOR_SIZE", (crate::tty::cursor_base().round() as u32).to_string());
        let ch = c.spawn().ok()?;
        let pid = ch.id();
        self.keep_child(ch);
        Some(pid)
    }

    /// A started program kept until it ends, then waited for (no zombies left behind).
    fn keep_child(&self, ch: std::process::Child) {
        use std::sync::{atomic::{AtomicBool, Ordering}, Mutex};
        static CHILDREN: Mutex<Vec<std::process::Child>> = Mutex::new(Vec::new());
        static REAPING: AtomicBool = AtomicBool::new(false);
        fn reap() {
            if let Ok(mut v) = CHILDREN.lock() {
                v.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
            }
        }
        reap();
        if let Ok(mut v) = CHILDREN.lock() {
            v.push(ch);
        }
        // a timer collects them every few seconds
        if let Some(h) = &self.loop_handle {
            if !REAPING.swap(true, Ordering::Relaxed) {
                use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
                let d = std::time::Duration::from_secs(3);
                let _ = h.insert_source(Timer::from_duration(d), move |_, _, _| {
                    reap();
                    TimeoutAction::ToDuration(d)
                });
            }
        }
    }

    fn hypr_monitors(&self) -> Value {
        Value::Array(self.outputs.iter().enumerate().map(|(i, o)| {
            let mode = o.current_mode();
            let (w, h) = mode.map(|m| (m.size.w, m.size.h)).unwrap_or((0, 0));
            let props = o.physical_properties();
            let zone = layer_map_for(o);
            let at = self.space.output_geometry(o).map(|g| g.loc).unwrap_or_default();
            let ws = self.shown_on.get(&o.name()).copied().unwrap_or(0);
            let hz = mode.map(|m| m.refresh as f64 / 1000.0).unwrap_or(60.0);
            json!({
                "id": i, "name": o.name(), "description": format!("{} {}", props.make, props.model),
                "make": props.make, "model": props.model, "serial": "",
                "width": w, "height": h, "physicalWidth": props.size.w, "physicalHeight": props.size.h,
                "refreshRate": hz,
                "x": at.x, "y": at.y,
                "activeWorkspace": { "id": ws, "name": ws.to_string() },
                "specialWorkspace": { "id": 0, "name": "" },
                "reserved": zone, "scale": o.current_scale().fractional_scale(), "transform": 0,
                "focused": self.output.as_ref() == Some(o), "dpmsStatus": !self.screen_off, "vrr": self.vrr_on(&o.name()), "solitary": "0", "solitaryBlockedBy": [],
                "activelyTearing": false, "tearingBlockedBy": [], "directScanoutTo": "0", "directScanoutBlockedBy": [],
                "disabled": false, "currentFormat": "XRGB8888", "mirrorOf": "none", "availableModes": [format!("{w}x{h}@{hz:.2}Hz")],
            })
        }).collect())
    }

    /// Hyprland's form: by monitor, levels 0 (background) to 3 (overlay), each layer's box
    fn hypr_layers(&self) -> Value {
        use smithay::wayland::shell::wlr_layer::Layer as L;
        let mut all = serde_json::Map::new();
        for o in &self.outputs {
            let map = smithay::desktop::layer_map_for_output(o);
            let at = self.space.output_geometry(o).map(|g| g.loc).unwrap_or_default();
            let level = |l: L| -> Value {
                Value::Array(map.layers_on(l).map(|s| {
                    let g = map.layer_geometry(s).unwrap_or_default();
                    json!({ "address": format!("0x{:x}", smithay::reexports::wayland_server::Resource::id(s.wl_surface()).protocol_id()), "x": g.loc.x + at.x, "y": g.loc.y + at.y, "w": g.size.w, "h": g.size.h, "namespace": s.namespace() })
                }).collect())
            };
            all.insert(o.name(), json!({ "levels": { "0": level(L::Background), "1": level(L::Bottom), "2": level(L::Top), "3": level(L::Overlay) } }));
        }
        Value::Object(all)
    }

    fn hypr_workspace(&self, id: i32) -> Value {
        let here: Vec<&crate::wm::Managed> = self.managed.iter().filter(|m| m.ws == id).collect();
        let last = here.last();
        json!({
            "id": id, "name": if id == crate::wm::SPECIAL { "special:special".to_string() } else { id.to_string() },
            "monitor": self.ws_on.get(&id).cloned().or_else(|| self.output.as_ref().map(|o| o.name())).unwrap_or_default(),
            "monitorID": self.ws_on.get(&id).map(|n| self.output_id(n)).unwrap_or(0),
            "windows": here.len(), "hasfullscreen": false,
            "lastwindow": last.map(|m| format!("0x{:x}", m.addr)).unwrap_or_else(|| "0x0".into()),
            "lastwindowtitle": last.map(|m| self.title_of(&m.window)).unwrap_or_default(),
            "ispersistent": false,
        })
    }

    fn hypr_client(&self, m: &crate::wm::Managed) -> Value {
        // a hidden workspace's window: where it was last shown
        let loc = self.space.element_location(&m.window).unwrap_or(m.last_at);
        let size = m.window.geometry().size;
        let class = self.class_of(&m.window);
        let title = self.title_of(&m.window);
        json!({
            "address": format!("0x{:x}", m.addr), "mapped": true, "hidden": !self.is_shown(m.ws),
            "at": [loc.x, loc.y], "size": [size.w, size.h],
            "workspace": { "id": m.ws, "name": if m.ws == crate::wm::SPECIAL { "special:special".to_string() } else { m.ws.to_string() } },
            "floating": m.floating, "pseudo": false, "monitor": self.ws_on.get(&m.ws).map(|n| self.output_id(n)).unwrap_or(0),
            "class": class, "title": title, "initialClass": class, "initialTitle": title,
            "pid": m.pid, "xwayland": m.window.x11_surface().is_some(), "pinned": false, "fullscreen": if self.is_full(&m.window) { 2 } else { 0 }, "fullscreenClient": if self.is_full(&m.window) { 2 } else { 0 },
            "grouped": [], "tags": [], "swallowing": "0x0",
            "focusHistoryID": if self.focused.as_ref() == Some(&m.window) { 0 } else { 1 },
            "inhibitingIdle": false, "xdgTag": "", "xdgDescription": "", "contentType": "none",
        })
    }
}

/// How much of each edge the layers keep (Hyprland's `reserved`: left, top, right, bottom).
fn layer_map_for(o: &smithay::output::Output) -> Value {
    let map = smithay::desktop::layer_map_for_output(o);
    // the screen in logical units (the zone is logical; its pixels are not)
    let scale = o.current_scale().fractional_scale();
    let full = o.current_mode().map(|m| o.current_transform().transform_size(m.size).to_f64().to_logical(scale).to_i32_round::<i32>()).unwrap_or_default();
    let z = map.non_exclusive_zone();
    json!([z.loc.x, z.loc.y, full.w - z.loc.x - z.size.w, full.h - z.loc.y - z.size.h])
}

/// hyprctl's text form of a JSON answer, roughly: one `key: value` per line.
fn plain(v: &Value) -> String {
    match v {
        Value::Array(a) => a.iter().map(plain).collect::<Vec<_>>().join("\n\n"),
        Value::Object(o) => o.iter().map(|(k, v)| format!("\t{k}: {}", match v { Value::String(s) => s.clone(), other => other.to_string() })).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

impl Loom {
    /// `getoption NAME`: what it is now, typed as Hyprland answers it (int, float or str): a value
    /// set by `keyword`, else loom's own setting, else the Hyprland config.
    fn hypr_option(&self, name: &str) -> Value {
        let name = name.trim();
        let live = match name {
            "decoration:rounding" => Some(self.look.rounding.to_string()),
            "decoration:rounding_power" => Some(self.look.rounding_power.to_string()),
            "general:gaps_in" => Some(self.look.gaps_in.to_string()),
            "general:gaps_out" => Some(self.look.gaps_out.to_string()),
            _ => None,
        };
        let v = self.keywords.get(name).cloned().or(live).or_else(|| crate::config::hyprland_values(&crate::config::hyprland_conf()).get(name).cloned());
        let Some(v) = v else { return json!({ "option": name, "int": 0, "set": false }) };
        let t = v.trim();
        let mut o = json!({ "option": name, "set": true });
        match (t.parse::<i64>(), t.parse::<f64>()) {
            (Ok(i), _) => o["int"] = json!(i),
            (_, Ok(f)) if f.fract() == 0.0 => o["int"] = json!(f as i64),
            (_, Ok(f)) => o["float"] = json!(f),
            _ => match t {
                "true" | "yes" | "on" => o["int"] = json!(1),
                "false" | "no" | "off" => o["int"] = json!(0),
                _ => o["str"] = json!(t),
            },
        }
        o
    }

    /// `keyword NAME VALUE`: kept (getoption answers it), and the ones loom acts on applied at once
    /// (GameMode turns blur, animations, rounding and gaps off and on this way).
    pub fn set_keyword(&mut self, k: &str, v: &str) {
        let was = self.keywords.insert(k.to_string(), v.to_string());
        let on = !matches!(v.trim(), "0" | "false" | "no" | "off");
        let num = |d: f32| v.trim().parse::<f32>().unwrap_or(d);
        match k {
            "decoration:rounding" => self.look.rounding = num(self.look.rounding).max(0.0),
            "general:gaps_in" => self.look.gaps_in = num(self.look.gaps_in).max(0.0),
            "general:gaps_out" => self.look.gaps_out = num(self.look.gaps_out).max(0.0),
            "decoration:blur:enabled" => self.blur.settings.enabled = on,
            "animations:enabled" => self.layer_cfg.enabled = on,
            "input:kb_model" | "input:kb_layout" | "input:kb_variant" | "input:kb_options" => {
                // a layout that does not load is not kept, so devices names the one in use
                if !self.apply_xkb() {
                    match was {
                        Some(w) => self.keywords.insert(k.to_string(), w),
                        None => self.keywords.remove(k),
                    };
                }
                return;
            }
            k if k.starts_with("input:") || k.starts_with("device:") => {
                let lines = self.input_lines();
                self.inputs.reapply(&lines);
                return;
            }
            // drawn on the next frame (no layout change)
            "decoration:screen_shader" => {
                self.dirty = true;
                return;
            }
            _ => return,
        }
        self.relayout();
    }

    /// The config's input and device lines, the ones set by `keyword` after them.
    pub fn input_lines(&self) -> Vec<(String, String)> {
        let mut l = crate::config::hyprland_lines(&crate::config::hyprland_conf());
        l.extend(self.keywords.iter().filter(|(k, _)| k.starts_with("input:") || k.starts_with("device:")).map(|(k, v)| (k.clone(), v.clone())));
        l
    }

    /// The keyboard's model, layouts, variants and options: set by `keyword`, else the config.
    fn xkb_now(&self) -> [String; 4] {
        let conf = crate::config::xkb_names();
        let keys = ["input:kb_model", "input:kb_layout", "input:kb_variant", "input:kb_options"];
        std::array::from_fn(|i| self.keywords.get(keys[i]).map(|s| s.trim().to_string()).filter(|s| s != "[[EMPTY]]").unwrap_or_else(|| conf[i].clone()))
    }

    /// The keymap made again from the kb_ options (a bad one is refused and the old one kept).
    pub fn apply_xkb(&mut self) -> bool {
        let [model, layout, variant, options] = self.xkb_now();
        let cfg = smithay::input::keyboard::XkbConfig { model: &model, layout: &layout, variant: &variant, options: Some(options).filter(|o| !o.is_empty()), ..Default::default() };
        let kb = self.seat.get_keyboard().expect("keyboard");
        if kb.set_xkb_config(self, cfg).is_err() {
            eprintln!("loom: keyboard layout {layout:?} ({variant:?}) does not load; kept the old one");
            return false;
        }
        self.layout_changed();
        true
    }

    /// The active layout's name (`English (US)`), and its index and how many there are.
    fn active_layout(&mut self) -> (String, u32, u32) {
        let kb = self.seat.get_keyboard().expect("keyboard");
        kb.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            let l = xkb.active_layout();
            (xkb.layout_name(l).to_string(), l.0, xkb.layouts().count() as u32)
        })
    }

    fn layout_changed(&mut self) {
        let (name, ..) = self.active_layout();
        self.hypr_event(&format!("activelayout>>loom-keyboard,{name}"));
    }

    /// `switchxkblayout DEVICE next|prev|N` (there is one keyboard, so any device is it).
    fn switch_layout(&mut self, arg: &str) -> String {
        let how = arg.split_whitespace().nth(1).unwrap_or("next");
        let (_, at, n) = self.active_layout();
        let to = match how {
            "next" => (at + 1) % n.max(1),
            "prev" => (at + n.max(1) - 1) % n.max(1),
            i => match i.parse::<u32>() {
                Ok(i) if i < n => i,
                _ => return "error: no such layout".into(),
            },
        };
        let kb = self.seat.get_keyboard().expect("keyboard");
        kb.with_xkb_state(self, |mut ctx| ctx.set_layout(smithay::input::keyboard::Layout(to)));
        self.layout_changed();
        "ok".into()
    }

    /// The screen shader's file (decoration:screen_shader, live or from the config); "" for none.
    pub fn screen_shader(&self) -> String {
        let v = self.keywords.get("decoration:screen_shader").cloned().unwrap_or_else(|| self.shader_conf.clone());
        let v = v.trim().trim_matches('"').to_string();
        if v == "[[EMPTY]]" { String::new() } else { v }
    }

    /// `hyprctl eval`: `hl.config({ section = { key = value } })` sets those options live, as
    /// `keyword` does (the shell sends its screen shader so).
    pub fn hypr_eval(&mut self, src: &str) -> String {
        let Some((name, args)) = crate::hypr_lua::call(src.trim(), "hl.") else { return "ok".into() };
        // hl.device({ name = "…", scroll_factor = 2 }): that device's settings
        if name == "device" {
            if let Some(crate::hypr_lua::Val::Table(t)) = args.first() {
                let dev = t.get("name").map(|n| n.text().replace(' ', "-")).unwrap_or_default();
                for (k, v) in t.iter().filter(|(k, _)| *k != "name" && !dev.is_empty()) {
                    self.set_keyword(&format!("device:{dev}:{k}"), &v.text());
                }
            }
            return "ok".into();
        }
        if name != "config" {
            return "ok".into();
        }
        fn flat(prefix: &str, v: &crate::hypr_lua::Val, out: &mut Vec<(String, String)>) {
            match v {
                crate::hypr_lua::Val::Table(t) => {
                    for (k, v) in t {
                        flat(&if prefix.is_empty() { k.clone() } else { format!("{prefix}:{k}") }, v, out);
                    }
                }
                v => out.push((prefix.to_string(), v.text())),
            }
        }
        let mut set = Vec::new();
        for a in &args {
            flat("", a, &mut set);
        }
        for (k, v) in set {
            self.set_keyword(&k, &v);
        }
        "ok".into()
    }
}

/// A window asked to close (Wayland: its toplevel's close; X: WM_DELETE_WINDOW).
fn close_window(w: &smithay::desktop::Window) {
    match (w.toplevel(), w.x11_surface()) {
        (Some(t), _) => t.send_close(),
        (_, Some(x)) => {
            let _ = x.close();
        }
        _ => {}
    }
}

impl Loom {
    /// What a surface is, for loomstate: a window (its class and address), a layer, a popup.
    fn what_is(&self, s: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> String {
        if let Some(m) = self.managed.iter().find(|m| smithay::wayland::seat::WaylandFocus::wl_surface(&m.window).is_some_and(|w| *w == *s)) {
            return format!("window {} 0x{:x} ws {}", self.class_of(&m.window), m.addr, m.ws);
        }
        if self.layer_output_of(s).is_some() {
            return format!("layer {}", self.layer_ns(s));
        }
        format!("surface {:?}", smithay::reexports::wayland_server::Resource::id(s))
    }

    pub fn loom_state(&self) -> Value {
        let kb = self.seat.get_keyboard().and_then(|k| k.current_focus()).and_then(|f| f.surface()).map(|s| self.what_is(&s));
        let ptr = self.seat.get_pointer().and_then(|p| p.current_focus()).map(|s| self.what_is(&s));
        let under = self.surface_at(self.pointer_at).map(|(s, _)| self.what_is(&s));
        json!({
            "focused": self.focused.as_ref().and_then(|w| self.managed_of(w)).map(|m| format!("{} 0x{:x}", self.class_of(&m.window), m.addr)),
            "focus_on_map": self.focus_on_map.is_some(),
            "keyboard": kb,
            "pointer_focus": ptr,
            "under_pointer": under,
            "pointer_at": [self.pointer_at.x as i32, self.pointer_at.y as i32],
            "pointer_grabbed": self.seat.get_pointer().is_some_and(|p| p.is_grabbed()),
            "keyboard_grabbed": self.seat.get_keyboard().is_some_and(|k| k.is_grabbed()),
            "drag": self.drag.as_ref().map(|d| format!("{} button {}", if d.resize { "resize" } else { "move" }, d.button)),
            "menus_open": self.popup_grab.len(),
            "focus_grabs": self.proto.grabs.len(),
            "panels_wanting_keys": self.layer_kb_wanted.len(),
            "active_ws": self.active_ws,
            "shown": self.shown_on,
            "locked": self.locked(),
            "cursor": self.cursor_info(),
        })
    }
}

impl Loom {
    /// The cursor image now (for loomstate): the theme's shape, or a program's surface with its
    /// buffer size and scale, the size loom works out for it, and whether it is an X program's.
    fn cursor_info(&self) -> Value {
        use smithay::input::pointer::CursorImageStatus;
        match &self.cursor_status {
            CursorImageStatus::Hidden => json!("hidden"),
            CursorImageStatus::Named(n) => json!(format!("named {}", n.name())),
            CursorImageStatus::Surface(s) => {
                let from_x = smithay::reexports::wayland_server::Resource::client(s).is_some_and(|c| self.x.client.as_ref().is_some_and(|x| x.id() == c.id()));
                let (buf, bscale, logical) = smithay::backend::renderer::utils::with_renderer_surface_state(s, |st| (st.buffer_size().map(|b| [b.w, b.h]), st.buffer_scale(), st.surface_size().map(|z| [z.w, z.h]))).unwrap_or((None, 0, None));
                json!({ "from_x": from_x, "buffer": buf, "buffer_scale": bscale, "logical": logical, "x_scale": self.x_client_scale(), "client": smithay::reexports::wayland_server::Resource::client(s).map(|c| format!("{:?}", c.id())) })
            }
        }
    }
}
