//! nodraw_check: a window that answers its configure but never draws (zett after a restart), or
//! draws only after `--draw-after SECONDS`. loom must not keep a tile empty for it: past its wait
//! the other windows take the room, and a late first picture gets its tile back.

use std::{
    os::fd::AsFd,
    time::{Duration, Instant},
};

use wayland_client::{
    protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct S {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    // the latest configure: serial, size
    configured: Option<(u32, i32, i32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for S {
    fn event(s: &mut Self, r: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = e {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(r.bind(name, 4, qh, ())),
                "wl_shm" => s.shm = Some(r.bind(name, 1, qh, ())),
                "xdg_wm_base" => s.wm = Some(r.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for S {
    fn event(_: &mut Self, wm: &xdg_wm_base::XdgWmBase, e: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for S {
    fn event(s: &mut Self, _: &xdg_surface::XdgSurface, e: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = e {
            let (w, h) = s.configured.map(|c| (c.1, c.2)).unwrap_or((0, 0));
            s.configured = Some((serial, w, h));
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for S {
    fn event(s: &mut Self, _: &xdg_toplevel::XdgToplevel, e: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = e {
            let serial = s.configured.map(|c| c.0).unwrap_or(0);
            s.configured = Some((serial, width, height));
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for S {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(wl_compositor::WlCompositor, wl_shm::WlShm, wl_surface::WlSurface, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer);

fn main() {
    let draw_after = std::env::args().skip_while(|a| a != "--draw-after").nth(1).and_then(|v| v.parse::<u64>().ok());
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let mut q = conn.new_event_queue();
    let qh = q.handle();
    conn.display().get_registry(&qh, ());
    let mut s = S::default();
    q.roundtrip(&mut s).expect("roundtrip");
    let (Some(comp), Some(shm), Some(wm)) = (s.compositor.clone(), s.shm.clone(), s.wm.clone()) else {
        println!("FAIL: missing globals");
        std::process::exit(1);
    };
    let surface = comp.create_surface(&qh, ());
    let xs = wm.get_xdg_surface(&surface, &qh, ());
    let top = xs.get_toplevel(&qh, ());
    top.set_title("nodraw".into());
    surface.commit();
    q.roundtrip(&mut s).expect("roundtrip");
    let mut acked = 0;
    let start = Instant::now();
    let mut drawn = false;
    loop {
        // configures answered, nothing attached (until --draw-after)
        if let Some((serial, w, h)) = s.configured {
            if serial != acked {
                xs.ack_configure(serial);
                acked = serial;
                if drawn {
                    draw(&shm, &surface, &qh, w, h);
                } else {
                    surface.commit();
                }
            }
        }
        if !drawn && draw_after.is_some_and(|d| start.elapsed() >= Duration::from_secs(d)) {
            let (_, w, h) = s.configured.unwrap_or((0, 400, 300));
            draw(&shm, &surface, &qh, w, h);
            drawn = true;
            println!("drawn");
        }
        conn.flush().expect("flush");
        let _ = q.dispatch_pending(&mut s);
        if let Some(guard) = conn.prepare_read() {
            let _ = guard.read();
        }
        let _ = q.dispatch_pending(&mut s);
        std::thread::sleep(Duration::from_millis(50));
        if start.elapsed() > Duration::from_secs(30) {
            break;
        }
    }
}

/// A grey picture the size asked (400x300 when none).
fn draw(shm: &wl_shm::WlShm, surface: &wl_surface::WlSurface, qh: &QueueHandle<S>, w: i32, h: i32) {
    let (w, h) = (if w > 0 { w } else { 400 }, if h > 0 { h } else { 300 });
    let size = (w * h * 4) as usize;
    let fd = unsafe { libc::memfd_create(c"nodraw_check".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create");
    let f: std::fs::File = unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) };
    f.set_len(size as u64).expect("set_len");
    std::fs::write(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&f)), vec![0x70u8; size]).expect("write");
    let pool = shm.create_pool(f.as_fd(), size as i32, qh, ());
    let buf = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, qh, ());
    surface.attach(Some(&buf), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    surface.commit();
}
