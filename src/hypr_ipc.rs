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
    let signature = format!("loom_{}_{secs}", std::process::id());
    let dir = PathBuf::from(runtime).join("hypr").join(&signature);
    std::fs::create_dir_all(&dir)?;
    let requests = UnixListener::bind(dir.join(".socket.sock"))?;
    requests.set_nonblocking(true)?;
    let events = UnixListener::bind(dir.join(".socket2.sock"))?;
    events.set_nonblocking(true)?;
    handle.insert_source(Generic::new(requests, Interest::READ, Mode::Level), |_, l, state: &mut Loom| {
        while let Ok((mut s, _)) = l.accept() {
            let _ = s.set_nonblocking(false);
            let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let reply = state.hypr_request(&req);
            let _ = s.write_all(reply.as_bytes());
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
    Ok(HyprIpc { signature, dir, listeners: Vec::new() })
}

impl Loom {
    /// An event to every listener of the event socket (a listener that went is dropped).
    pub fn hypr_event(&mut self, line: &str) {
        let Some(h) = self.hypr.as_mut() else { return };
        let msg = format!("{line}\n");
        h.listeners.retain_mut(|s| s.write_all(msg.as_bytes()).is_ok());
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
            "layers" => Some(json!({})),
            "devices" => Some(json!({ "mice": [], "keyboards": [{ "address": "0x0", "name": "loom-keyboard", "rules": "", "model": "", "layout": "us", "variant": "", "options": "", "active_keymap": "English (US)", "capsLock": false, "numLock": false, "main": true }], "tablets": [], "touch": [], "switches": [] })),
            "version" => Some(json!({ "branch": "loom", "commit": env!("CARGO_PKG_VERSION"), "version": "0.56.0", "dirty": false, "commit_message": "loom", "commit_date": "", "tag": "loom", "commits": "0", "buildAquamarine": "", "buildHyprlang": "", "buildHyprutils": "", "buildHyprcursor": "", "buildHyprgraphics": "", "flags": [] })),
            "status" => Some(json!({ "compositor": "loom", "version": env!("CARGO_PKG_VERSION") })),
            "binds" | "globalshortcuts" | "instances" => Some(json!([])),
            "getoption" => Some(json!({ "option": arg, "int": 0, "float": 0.0, "str": "", "set": false })),
            "cursorpos" => Some(json!({ "x": self.pointer_at.x as i32, "y": self.pointer_at.y as i32 })),
            "splash" => return "loom".into(),
            "dispatch" => return self.hypr_dispatch(arg),
            "keyword" | "reload" | "setcursor" | "switchxkblayout" | "notify" | "dismissnotify" | "eval" => return "ok".into(),
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
        let (d, a) = arg.split_once(' ').map(|(x, y)| (x, y.trim())).unwrap_or((arg, ""));
        let ws_of = |s: &Loom, a: &str| -> Option<i32> {
            let a = a.trim();
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
            "movetoworkspace" | "movetoworkspacesilent" => match ws_of(self, a.split(',').next().unwrap_or(a)) {
                Some(n) => {
                    self.move_to_workspace(n);
                    if d == "movetoworkspace" {
                        self.switch_workspace(n);
                    }
                }
                None => return format!("invalid workspace {a}"),
            },
            "exec" => self.spawn(a),
            "killactive" => {
                if let Some(t) = self.focused.as_ref().and_then(|w| w.toplevel().cloned()) {
                    t.send_close();
                }
            }
            "togglefloating" => {
                if let Some(w) = self.focused.clone() {
                    if let Some(m) = self.managed.iter_mut().find(|m| m.window == w) {
                        m.floating = !m.floating;
                    }
                    self.relayout();
                }
            }
            "focuswindow" => {
                let want = a.trim_start_matches("address:").trim_start_matches("0x");
                if let Some(m) = self.managed.iter().find(|m| format!("{:x}", m.addr) == want).cloned() {
                    self.switch_workspace(m.ws);
                    self.focus_window(&m.window);
                }
            }
            "exit" => self.signal.stop(),
            _ => return format!("Invalid dispatcher {d}"),
        }
        "ok".into()
    }

    /// A program started in loom: its display and Hyprland signature are loom's.
    pub fn spawn(&self, cmd: &str) {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd).env_remove("DISPLAY");
        if let Some(name) = &self.socket_name {
            c.env("WAYLAND_DISPLAY", name);
        }
        if let Some(h) = &self.hypr {
            c.env("HYPRLAND_INSTANCE_SIGNATURE", &h.signature);
        }
        let _ = c.spawn();
    }

    fn hypr_monitors(&self) -> Value {
        let Some(o) = &self.output else { return json!([]) };
        let mode = o.current_mode();
        let (w, h) = mode.map(|m| (m.size.w, m.size.h)).unwrap_or((0, 0));
        let props = o.physical_properties();
        let zone = layer_map_for(o);
        json!([{
            "id": 0, "name": o.name(), "description": format!("{} {}", props.make, props.model),
            "make": props.make, "model": props.model, "serial": "",
            "width": w, "height": h, "physicalWidth": props.size.w, "physicalHeight": props.size.h,
            "refreshRate": mode.map(|m| m.refresh as f64 / 1000.0).unwrap_or(60.0),
            "x": 0, "y": 0,
            "activeWorkspace": { "id": self.active_ws, "name": self.active_ws.to_string() },
            "specialWorkspace": { "id": 0, "name": "" },
            "reserved": zone, "scale": o.current_scale().fractional_scale(), "transform": 0,
            "focused": true, "dpmsStatus": true, "vrr": false, "solitary": "0", "solitaryBlockedBy": [],
            "activelyTearing": false, "tearingBlockedBy": [], "directScanoutTo": "0", "directScanoutBlockedBy": [],
            "disabled": false, "currentFormat": "XRGB8888", "mirrorOf": "none", "availableModes": [format!("{w}x{h}@60.00Hz")],
        }])
    }

    fn hypr_workspace(&self, id: i32) -> Value {
        let here: Vec<&crate::wm::Managed> = self.managed.iter().filter(|m| m.ws == id).collect();
        let last = here.last();
        json!({
            "id": id, "name": id.to_string(),
            "monitor": self.output.as_ref().map(|o| o.name()).unwrap_or_default(), "monitorID": 0,
            "windows": here.len(), "hasfullscreen": false,
            "lastwindow": last.map(|m| format!("0x{:x}", m.addr)).unwrap_or_else(|| "0x0".into()),
            "lastwindowtitle": last.map(|m| self.title_of(&m.window)).unwrap_or_default(),
            "ispersistent": false,
        })
    }

    fn hypr_client(&self, m: &crate::wm::Managed) -> Value {
        let loc = self.space.element_location(&m.window).unwrap_or_default();
        let size = m.window.geometry().size;
        let class = self.class_of(&m.window);
        let title = self.title_of(&m.window);
        json!({
            "address": format!("0x{:x}", m.addr), "mapped": true, "hidden": m.ws != self.active_ws,
            "at": [loc.x, loc.y], "size": [size.w, size.h],
            "workspace": { "id": m.ws, "name": m.ws.to_string() },
            "floating": m.floating, "pseudo": false, "monitor": 0,
            "class": class, "title": title, "initialClass": class, "initialTitle": title,
            "pid": m.pid, "xwayland": false, "pinned": false, "fullscreen": 0, "fullscreenClient": 0,
            "grouped": [], "tags": [], "swallowing": "0x0",
            "focusHistoryID": if self.focused.as_ref() == Some(&m.window) { 0 } else { 1 },
            "inhibitingIdle": false, "xdgTag": "", "xdgDescription": "", "contentType": "none",
        })
    }
}

/// How much of each edge the layers keep (Hyprland's `reserved`: left, top, right, bottom).
fn layer_map_for(o: &smithay::output::Output) -> Value {
    let map = smithay::desktop::layer_map_for_output(o);
    let full = o.current_mode().map(|m| m.size.to_logical(1)).unwrap_or_default();
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
