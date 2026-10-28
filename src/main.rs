//! loom: a Wayland compositor that speaks Hyprland's and KDE's protocols (see PLAN.md).
//!
//!   loom --headless [--size 1280x720] [--snapshot out.png] [--wait 2000] [--socket NAME] [--click X,Y] [--rclick X,Y] [--drag X,Y,X2,Y2] [--keys TEXT] [--input-at ms] [--record DIR] [--binds CONF] [--gpu] [-- CMD …]

mod anim;
mod headless;
mod hypr_ipc;
mod idle;
mod hypr_lua;
mod hypr_conf_lua;
mod image_copy;
mod input_cfg;
mod hypr_proto;
mod binds;
mod blur;
mod capture;
mod config;
mod decor;
mod decoration;
mod focus;
mod frames;
mod kde_output;
mod shader;
mod xdnd;
mod gestures;
mod gpu;
mod foreign;
mod input;
mod kwin_fx;
mod layer_anim;
mod layouts;
mod lock;
mod pointer;
mod protocols;
mod screencopy;
mod nested;
mod clipboard;
mod outputs;
mod own_x;
mod render;
mod restart;
mod scale;
mod rounding;
mod state;
mod tablet;
mod touch;
mod tty;
mod winrules;
mod wm;
mod xwayland;
mod xscreens;
mod greeter;
mod curtain;
mod nightlight;

use std::{path::PathBuf, time::Duration};

fn main() {
    restart::take_env();
    // `loom restart`: the running loom (this session's) replaced by the installed binary
    if std::env::args().nth(1).as_deref() == Some("restart") {
        std::process::exit(ask_restart());
    }
    let mut args = std::env::args().skip(1);
    let mut opts = headless::Options { size: (1280, 720), snapshot: None, wait: Duration::from_millis(2000), socket: None, run: Vec::new(), script: Vec::new(), script_at: Duration::from_millis(1200), record: None, binds: None, gpu: false, scale: 1.0, more: Vec::new() };
    let mut headless = false;
    let mut nested = false;
    let mut tty = false;
    let mut session = false;
    let mut for_minutes: Option<u64> = None;
    let mut greeter_dir: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--headless" => headless = true,
            // the login screen (greeter.rs): DIR is the user's snapshot, the greeter QML runs
            "--greeter" => greeter_dir = args.next().map(PathBuf::from),
            // a session started from the login screen: its last pictures over the start (curtain.rs)
            // Safety: single-threaded still (argument parsing)
            "--curtain" => unsafe { std::env::set_var("LOOM_CURTAIN", args.next().unwrap_or_default()) },
            "--nested" => nested = true,
            "--tty" => tty = true,
            "--session" => session = true,
            "--for" => for_minutes = args.next().and_then(|v| v.parse().ok()),
            "--size" => {
                // WxH, or several screens side by side: 1600x1000,1920x1080
                let v = args.next().unwrap_or_default();
                let sizes: Vec<(i32, i32)> = v.split(',').filter_map(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))).collect();
                if let Some((first, rest)) = sizes.split_first() {
                    opts.size = *first;
                    opts.more = rest.to_vec();
                }
            }
            "--snapshot" => opts.snapshot = args.next().map(PathBuf::from),
            "--wait" => opts.wait = Duration::from_millis(args.next().and_then(|v| v.parse().ok()).unwrap_or(2000)),
            "--socket" => opts.socket = args.next(),
            "--binds" => opts.binds = args.next().map(PathBuf::from),
            "--gpu" => opts.gpu = true,
            "--scale" => opts.scale = args.next().and_then(|v| v.parse().ok()).unwrap_or(1.0),
            "--record" => {
                opts.record = args.next().map(PathBuf::from);
                if let Some(d) = &opts.record {
                    let _ = std::fs::create_dir_all(d);
                }
            }
            "--keys" => opts.script.extend(input::parse_keys(&args.next().unwrap_or_default())),
            "--rclick" => {
                let v = args.next().unwrap_or_default();
                if let Some((x, y)) = v.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                    opts.script.push(input::Action::RightClick(x, y));
                }
            }
            "--click" => {
                let v = args.next().unwrap_or_default();
                if let Some((x, y)) = v.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))) {
                    opts.script.push(input::Action::Click(x, y));
                }
            }
            "--sdrag" => {
                let v: Vec<f64> = args.next().unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() >= 4 {
                    opts.script.push(input::Action::SuperDrag { from: (v[0], v[1]), to: (v[2], v[3]), button: v.get(4).map(|b| *b as u32).unwrap_or(272) });
                }
            }
            "--scroll" => {
                let arg = args.next().unwrap_or_default();
                let v: Vec<&str> = arg.split(',').collect();
                if let [x, y, dy, rest @ ..] = &v[..] {
                    if let (Ok(x), Ok(y), Ok(dy)) = (x.parse(), y.parse(), dy.parse()) {
                        opts.script.extend([input::Action::Wait, input::Action::Scroll { at: (x, y), dy, wheel: rest.first() == Some(&"wheel") }, input::Action::Wait]);
                    }
                }
            }
            "--drag" => {
                // pressed, held a moment (the program starts its drag), moved over 0.5 s, let go
                let v: Vec<f64> = args.next().unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if let [x, y, x2, y2] = v[..] {
                    use input::Action::*;
                    opts.script.extend([Move(x, y), Button(true), Wait, Wait, Move(x + 8.0, y), Wait, Wait]);
                    opts.script.extend((1..=20).map(|i| Move(x + (x2 - x) * i as f64 / 20.0, y + (y2 - y) * i as f64 / 20.0)));
                    opts.script.extend([Wait, Wait, Wait, Wait, Button(false)]);
                }
            }
            "--sscroll" => {
                let s = args.next().unwrap_or_default();
                let p: Vec<&str> = s.split(',').collect();
                if let [x, y, d] = p.as_slice() {
                    if let (Ok(x), Ok(y)) = (x.parse(), y.parse()) {
                        opts.script.push(input::Action::SuperScroll { at: (x, y), down: *d == "down" });
                    }
                }
            }
            "--input-at" => opts.script_at = Duration::from_millis(args.next().and_then(|v| v.parse().ok()).unwrap_or(1200)),
            "--" => {
                opts.run = args.by_ref().collect();
            }
            "-h" | "--help" => {
                println!("loom --headless [--size WxH] [--snapshot out.png] [--wait ms] [--socket NAME] [--click X,Y] [--rclick X,Y] [--drag X,Y,X2,Y2] [--keys TEXT] [--input-at ms] [--record DIR] [--binds CONF] [--gpu] [-- CMD …]");
                println!("loom --tty [--for MIN] [--session] [-- CMD …]   (--session: the login session, the Hyprland config's exec-once programs started)");
                println!("loom --nested [-- CMD …]");
                println!("loom --greeter DIR (with --headless/--tty/--nested): the login screen from the snapshot in DIR");
                return;
            }
            other => {
                eprintln!("loom: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if let Some(d) = &greeter_dir {
        greeter::enter(d, &mut opts.run);
    }
    if tty {
        if let Err(e) = tty::run(tty::Options { socket: opts.socket, run: opts.run, for_minutes, session }) {
            eprintln!("loom: {e}");
            std::process::exit(1);
        }
        return;
    }
    if nested {
        if let Err(e) = nested::run(opts.socket, opts.run) {
            eprintln!("loom: {e}");
            std::process::exit(1);
        }
        return;
    }
    if !headless {
        eprintln!("loom: --headless (tests) or --nested (a window on this desktop); the TTY backend is milestone 4");
        std::process::exit(2);
    }
    if let Err(e) = headless::run(opts) {
        eprintln!("loom: {e}");
        std::process::exit(1);
    }
}

/// Asks the loom of this session (HYPRLAND_INSTANCE_SIGNATURE) to restart in place.
fn ask_restart() -> i32 {
    use std::io::{Read, Write};
    let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") else {
        eprintln!("loom restart: no XDG_RUNTIME_DIR");
        return 1;
    };
    // the signature in the environment can be stale (a terminal server started under Hyprland):
    // else the running loom's own folder, loom_<pid>_… with that pid alive
    let sock = |sig: &str| format!("{rt}/hypr/{sig}/.socket.sock");
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().filter(|s| s.starts_with("loom_") && std::path::Path::new(&sock(s)).exists()).or_else(|| {
        std::fs::read_dir(format!("{rt}/hypr")).ok()?.flatten().map(|e| e.file_name().to_string_lossy().to_string()).find(|n| {
            n.strip_prefix("loom_").and_then(|r| r.split('_').next()).and_then(|p| p.parse::<u32>().ok()).is_some_and(|pid| {
                std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "loom") && std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.split(|b| *b == 0).any(|a| a == b"--tty"))
            }) && std::path::Path::new(&sock(n)).exists()
        })
    });
    let Some(sig) = sig else {
        eprintln!("loom restart: no running loom session found");
        return 1;
    };
    match std::os::unix::net::UnixStream::connect(sock(&sig)) {
        Ok(mut s) => {
            let force = std::env::args().any(|a| a == "--force");
            let _ = s.write_all(if force { b"dispatch loomrestart force" as &[u8] } else { b"dispatch loomrestart" });
            let mut r = String::new();
            let _ = s.read_to_string(&mut r);
            if !r.trim().is_empty() && r.trim() != "ok" {
                println!("loom restart: {}", r.trim());
            }
            if r.starts_with("not restarted") {
                eprintln!("loom restart: {}", r.trim());
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("loom restart: {e}");
            1
        }
    }
}
