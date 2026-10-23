//! loom-x: loom's own X server in Rust, a Wayland client of loom; every X top-level window is a Wayland window. Usage: loom-x [:N] [WxH] [--scale F]

mod clip;
mod colors;
mod draw;
mod ext;
mod fonts;
mod glx;
mod gpu;
mod keys;
mod render;
mod server;
mod shm;
mod wl;
mod xkb;
mod xi;

fn main() {
    let mut display = 20;
    // the X screen in the output's own pixels, and how many of them a logical pixel has
    let mut size = (1600u16, 1000u16);
    let mut scale = 1.0;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if let Some(n) = a.strip_prefix(':').and_then(|n| n.parse().ok()) {
            display = n;
        } else if a == "--scale" {
            scale = args
                .next()
                .and_then(|v| v.parse().ok())
                .filter(|v: &f64| *v >= 0.5)
                .unwrap_or(1.0);
        } else if let Some((w, h)) = a
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        {
            size = (w, h);
        } else {
            eprintln!("usage: loom-x [:N] [WxH] [--scale F]");
            std::process::exit(2);
        }
    }
    if let Err(e) = server::run(display, size, scale) {
        eprintln!("loom-x: {e}");
        std::process::exit(1);
    }
}
