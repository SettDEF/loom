//! dnd_source: a Wayland window that starts a drag of `file:///tmp/loom-dnd-test.wav` when pressed.
//! Prints `sent MIME`, `dropped`, `finished` or `cancelled`. With xdnd_target, tests loom's XDND:
//! `loom --headless --drag 200,300,600,300 -- sh -c 'dnd_source & sleep 1; DISPLAY= xdnd_target'`.

use std::{io::Write, os::fd::AsFd, time::{Duration, Instant}};

use wayland_client::{
    protocol::{wl_buffer, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const URI: &str = "file:///tmp/loom-dnd-test.wav\r\n";

/// DND_URI=… drags that instead (a real file, for a real program to open)
fn uri() -> String {
    std::env::var("DND_URI").map(|u| format!("{u}\r\n")).unwrap_or_else(|_| URI.into())
}

#[derive(Default)]
struct S {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    ddm: Option<wl_data_device_manager::WlDataDeviceManager>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    configured: Option<(u32, i32, i32)>,
    pressed: Option<u32>,
    done: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for S {
    fn event(s: &mut Self, r: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = e {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(r.bind(name, 4, qh, ())),
                "wl_shm" => s.shm = Some(r.bind(name, 1, qh, ())),
                "wl_seat" if s.seat.is_none() => s.seat = Some(r.bind(name, 5, qh, ())),
                "wl_data_device_manager" => s.ddm = Some(r.bind(name, 3, qh, ())),
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

impl Dispatch<wl_pointer::WlPointer, ()> for S {
    fn event(s: &mut Self, _: &wl_pointer::WlPointer, e: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } = e {
            s.pressed = Some(serial);
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for S {
    fn event(s: &mut Self, _: &wl_data_source::WlDataSource, e: wl_data_source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_data_source::Event::Send { mime_type, fd } => {
                let mut f = std::fs::File::from(fd);
                let _ = f.write_all(uri().as_bytes());
                println!("sent {mime_type}");
            }
            wl_data_source::Event::DndDropPerformed => println!("dropped"),
            wl_data_source::Event::DndFinished => {
                println!("finished");
                s.done = true;
            }
            wl_data_source::Event::Cancelled => {
                println!("cancelled");
                s.done = true;
            }
            _ => {}
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
ignore!(wl_compositor::WlCompositor, wl_shm::WlShm, wl_seat::WlSeat, wl_surface::WlSurface, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, wl_data_device_manager::WlDataDeviceManager);

impl Dispatch<wl_data_device::WlDataDevice, ()> for S {
    fn event(_: &mut Self, _: &wl_data_device::WlDataDevice, _: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}

    wayland_client::event_created_child!(S, wl_data_device::WlDataDevice, [wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ())]);
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for S {
    fn event(_: &mut Self, _: &wl_data_offer::WlDataOffer, _: wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

fn main() {
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let mut q = conn.new_event_queue();
    let qh = q.handle();
    conn.display().get_registry(&qh, ());
    let mut s = S::default();
    q.roundtrip(&mut s).expect("roundtrip");
    let (Some(comp), Some(shm), Some(seat), Some(ddm), Some(wm)) = (s.compositor.clone(), s.shm.clone(), s.seat.clone(), s.ddm.clone(), s.wm.clone()) else {
        println!("FAIL: missing globals");
        std::process::exit(1);
    };
    seat.get_pointer(&qh, ());
    let device = ddm.get_data_device(&seat, &qh, ());
    let surface = comp.create_surface(&qh, ());
    let xs = wm.get_xdg_surface(&surface, &qh, ());
    let top = xs.get_toplevel(&qh, ());
    top.set_title("dnd_source".into());
    surface.commit();
    q.roundtrip(&mut s).expect("roundtrip");
    // an orange picture the size it is given, again at each new size (a tile shrinks when another opens)
    let paint = |s: &S| {
        let (serial, w, h) = s.configured.expect("no configure");
        let (w, h) = (if w > 0 { w } else { 400 }, if h > 0 { h } else { 300 });
        xs.ack_configure(serial);
        let size = (w * h * 4) as usize;
        let fd = memfd(size);
        let px: Vec<u8> = (0..w * h).flat_map(|_| [0x30u8, 0x80, 0xe0, 0xff]).collect();
        std::fs::write(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&fd)), &px).expect("write");
        let pool = shm.create_pool(fd.as_fd(), size as i32, &qh, ());
        let buf = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, &qh, ());
        surface.attach(Some(&buf), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
        println!("shown {w}x{h}");
        serial
    };
    let mut acked = paint(&s);
    let end = Instant::now() + Duration::from_secs(12);
    let mut started = false;
    while !s.done && Instant::now() < end {
        q.blocking_dispatch(&mut s).expect("dispatch");
        if s.configured.is_some_and(|c| c.0 != acked) {
            acked = paint(&s);
        }
        if let (false, Some(serial)) = (started, s.pressed) {
            let src = ddm.create_data_source(&qh, ());
            src.offer("text/uri-list".into());
            src.offer("text/plain;charset=utf-8".into());
            src.set_actions(wl_data_device_manager::DndAction::Copy);
            device.start_drag(Some(&src), &surface, None, serial);
            started = true;
            println!("drag started");
        }
    }
}

fn memfd(size: usize) -> std::fs::File {
    let fd = unsafe { libc::memfd_create(c"dnd_source".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create");
    let f: std::fs::File = unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) };
    f.set_len(size as u64).expect("set_len");
    f
}
