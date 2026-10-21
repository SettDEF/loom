//! zeal: a Wayland compositor that speaks Hyprland's and KDE's protocols (see PLAN.md).
//!
//!   zeal --headless [--size 1280x720] [--snapshot out.png] [--wait 2000] [--socket NAME] [-- CMD …]

mod headless;
mod state;

use std::{path::PathBuf, time::Duration};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut opts = headless::Options { size: (1280, 720), snapshot: None, wait: Duration::from_millis(2000), socket: None, run: Vec::new() };
    let mut headless = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--headless" => headless = true,
            "--size" => {
                let v = args.next().unwrap_or_default();
                if let Some((w, h)) = v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) {
                    opts.size = (w, h);
                }
            }
            "--snapshot" => opts.snapshot = args.next().map(PathBuf::from),
            "--wait" => opts.wait = Duration::from_millis(args.next().and_then(|v| v.parse().ok()).unwrap_or(2000)),
            "--socket" => opts.socket = args.next(),
            "--" => {
                opts.run = args.by_ref().collect();
            }
            "-h" | "--help" => {
                println!("zeal --headless [--size WxH] [--snapshot out.png] [--wait ms] [--socket NAME] [-- CMD …]");
                return;
            }
            other => {
                eprintln!("zeal: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if !headless {
        eprintln!("zeal: only --headless so far (milestone 0); the TTY backend is milestone 4");
        std::process::exit(2);
    }
    if let Err(e) = headless::run(opts) {
        eprintln!("zeal: {e}");
        std::process::exit(1);
    }
}
