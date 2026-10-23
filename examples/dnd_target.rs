//! dnd_target: a Wayland window that takes drops of text/uri-list. Prints `enter TYPES`,
//! `dropped DATA` or `leave`. With xdnd_source, tests loom carrying an X drag into Wayland.

use std::{
    io::Read,
    os::fd::AsFd,
    time::{Duration, Instant},
};

use wayland_client::{
    protocol::{wl_buffer, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct S {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    ddm: Option<wl_data_device_manager::WlDataDeviceManager>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    configured: Option<(u32, i32, i32)>,
    types: Vec<String>,
    offer: Option<wl_data_offer::WlDataOffer>,
    dropped: bool,
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

impl Dispatch<wl_data_offer::WlDataOffer, ()> for S {
    fn event(s: &mut Self, _: &wl_data_offer::WlDataOffer, e: wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Offer { mime_type } = e {
            s.types.push(mime_type);
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for S {
    fn event(s: &mut Self, _: &wl_data_device::WlDataDevice, e: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            wl_data_device::Event::Enter { serial, id: Some(offer), .. } => {
                println!("enter {}", s.types.join(" "));
                offer.accept(serial, Some("text/uri-list".into()));
                offer.set_actions(wl_data_device_manager::DndAction::Copy, wl_data_device_manager::DndAction::Copy);
                s.offer = Some(offer);
            }
            wl_data_device::Event::Leave => {
                if !s.dropped {
                    println!("leave");
                }
            }
            wl_data_device::Event::Drop => s.dropped = true,
            _ => {}
        }
    }

    wayland_client::event_created_child!(S, wl_data_device::WlDataDevice, [wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ())]);
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for S {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(wl_compositor::WlCompositor, wl_shm::WlShm, wl_seat::WlSeat, wl_surface::WlSurface, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, wl_data_device_manager::WlDataDeviceManager);

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
    ddm.get_data_device(&seat, &qh, ());
    let surface = comp.create_surface(&qh, ());
    let xs = wm.get_xdg_surface(&surface, &qh, ());
    let top = xs.get_toplevel(&qh, ());
    top.set_title("dnd_target".into());
    surface.commit();
    q.roundtrip(&mut s).expect("roundtrip");
    // a grey picture the size it is given, again at each new size
    let paint = |s: &S| {
        let (serial, w, h) = s.configured.expect("no configure");
        let (w, h) = (if w > 0 { w } else { 400 }, if h > 0 { h } else { 300 });
        xs.ack_configure(serial);
        let size = (w * h * 4) as usize;
        let fd = memfd(size);
        let px: Vec<u8> = (0..w * h).flat_map(|_| [0x70u8, 0x70, 0x70, 0xff]).collect();
        std::fs::write(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&fd)), &px).expect("write");
        let pool = shm.create_pool(fd.as_fd(), size as i32, &qh, ());
        let buf = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, &qh, ());
        surface.attach(Some(&buf), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
        serial
    };
    let mut acked = paint(&s);
    println!("shown");
    let end = Instant::now() + Duration::from_secs(14);
    while !s.done && Instant::now() < end {
        q.blocking_dispatch(&mut s).expect("dispatch");
        if s.configured.is_some_and(|c| c.0 != acked) {
            acked = paint(&s);
        }
        if s.dropped {
            let Some(offer) = s.offer.take() else { break };
            let (mut read, write) = std::io::pipe().expect("pipe");
            offer.receive("text/uri-list".into(), write.as_fd());
            drop(write);
            conn.flush().expect("flush");
            let mut data = String::new();
            let _ = read.read_to_string(&mut data);
            println!("dropped {data:?}");
            offer.finish();
            q.roundtrip(&mut s).expect("roundtrip");
            s.done = true;
        }
    }
}

fn memfd(size: usize) -> std::fs::File {
    let fd = unsafe { libc::memfd_create(c"dnd_target".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create");
    let f: std::fs::File = unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) };
    f.set_len(size as u64).expect("set_len");
    f
}
