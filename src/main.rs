//! loom: a Wayland compositor that speaks Hyprland's and KDE's protocols (see PLAN.md).
//!
//!   loom --headless [--size 1280x720] [--snapshot out.png] [--wait 2000] [--socket NAME] [--click X,Y] [--keys TEXT] [--input-at ms] [--record DIR] [--binds CONF] [--gpu] [-- CMD …]

mod anim;
mod headless;
mod hypr_ipc;
mod hypr_lua;
mod hypr_proto;
mod binds;
mod capture;
mod focus;
mod gpu;
mod foreign;
mod input;
mod layouts;
mod pointer;
mod nested;
mod render;
mod state;
mod wm;
mod xwayland;

use std::{path::PathBuf, time::Duration};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut opts = headless::Options { size: (1280, 720), snapshot: None, wait: Duration::from_millis(2000), socket: None, run: Vec::new(), script: Vec::new(), script_at: Duration::from_millis(1200), record: None, binds: None, gpu: false };
    let mut headless = false;
    let mut nested = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--headless" => headless = true,
            "--nested" => nested = true,
            "--size" => {
                let v = args.next().unwrap_or_default();
                if let Some((w, h)) = v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) {
                    opts.size = (w, h);
                }
            }
            "--snapshot" => opts.snapshot = args.next().map(PathBuf::from),
            "--wait" => opts.wait = Duration::from_millis(args.next().and_then(|v| v.parse().ok()).unwrap_or(2000)),
            "--socket" => opts.socket = args.next(),
            "--binds" => opts.binds = args.next().map(PathBuf::from),
            "--gpu" => opts.gpu = true,
            "--record" => {
                opts.record = args.next().map(PathBuf::from);
                if let Some(d) = &opts.record {
                    let _ = std::fs::create_dir_all(d);
                }
            }
            "--keys" => opts.script.extend(input::parse_keys(&args.next().unwrap_or_default())),
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
                println!("loom --headless [--size WxH] [--snapshot out.png] [--wait ms] [--socket NAME] [--click X,Y] [--keys TEXT] [--input-at ms] [--record DIR] [--binds CONF] [--gpu] [-- CMD …]");
                return;
            }
            other => {
                eprintln!("loom: unknown argument {other}");
                std::process::exit(2);
            }
        }
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
