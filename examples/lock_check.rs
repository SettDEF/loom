//! lock_check: locks the session (ext-session-lock), shows a red lock surface, then unlocks.
//! Prints `locked`, `configured WxH` and `unlocked`, or what failed. Run inside loom:
//! `loom --headless --gpu --snapshot red.png --wait 3000 -- lock_check` (the shot is red).

use std::{os::fd::AsFd, time::Duration};

use wayland_client::{
    protocol::{wl_buffer, wl_compositor, wl_output, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::ext::session_lock::v1::client::{ext_session_lock_manager_v1 as mgr, ext_session_lock_surface_v1 as lsurf, ext_session_lock_v1 as lock};

#[derive(Default)]
struct S {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    output: Option<wl_output::WlOutput>,
    manager: Option<mgr::ExtSessionLockManagerV1>,
    locked: bool,
    finished: bool,
    configure: Option<(u32, u32, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for S {
    fn event(s: &mut Self, r: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = e {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(r.bind(name, 4, qh, ())),
                "wl_shm" => s.shm = Some(r.bind(name, 1, qh, ())),
                "wl_output" if s.output.is_none() => s.output = Some(r.bind(name, 1, qh, ())),
                "ext_session_lock_manager_v1" => s.manager = Some(r.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<lock::ExtSessionLockV1, ()> for S {
    fn event(s: &mut Self, _: &lock::ExtSessionLockV1, e: lock::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            lock::Event::Locked => s.locked = true,
            lock::Event::Finished => s.finished = true,
            _ => {}
        }
    }
}

impl Dispatch<lsurf::ExtSessionLockSurfaceV1, ()> for S {
    fn event(s: &mut Self, _: &lsurf::ExtSessionLockSurfaceV1, e: lsurf::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let lsurf::Event::Configure { serial, width, height } = e {
            s.configure = Some((serial, width, height));
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
ignore!(wl_compositor::WlCompositor, wl_shm::WlShm, wl_output::WlOutput, mgr::ExtSessionLockManagerV1, wl_surface::WlSurface, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer);

fn main() {
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let mut q = conn.new_event_queue();
    let qh = q.handle();
    conn.display().get_registry(&qh, ());
    let mut s = S::default();
    q.roundtrip(&mut s).expect("roundtrip");
    let (Some(comp), Some(shm), Some(out), Some(m)) = (s.compositor.clone(), s.shm.clone(), s.output.clone(), s.manager.clone()) else {
        println!("FAIL: no ext_session_lock_manager_v1 (or compositor, shm, output)");
        std::process::exit(1);
    };
    let l = m.lock(&qh, ());
    q.roundtrip(&mut s).expect("roundtrip");
    if !s.locked {
        println!("FAIL: not locked (finished: {})", s.finished);
        std::process::exit(1);
    }
    println!("locked");
    let surface = comp.create_surface(&qh, ());
    let ls = l.get_lock_surface(&surface, &out, &qh, ());
    q.roundtrip(&mut s).expect("roundtrip");
    let Some((serial, w, h)) = s.configure else {
        println!("FAIL: the lock surface got no configure");
        std::process::exit(1);
    };
    println!("configured {w}x{h}");
    ls.ack_configure(serial);
    // a red picture the size it was given
    let size = (w * h * 4) as usize;
    let fd = rustix_memfd(size);
    let px: Vec<u8> = (0..w * h).flat_map(|_| [0x30u8, 0x30, 0xe0, 0xff]).collect();
    std::fs::write(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&fd)), &px).expect("write");
    let pool = shm.create_pool(fd.as_fd(), size as i32, &qh, ());
    let buf = pool.create_buffer(0, w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Xrgb8888, &qh, ());
    surface.attach(Some(&buf), 0, 0);
    surface.damage_buffer(0, 0, w as i32, h as i32);
    surface.commit();
    q.roundtrip(&mut s).expect("roundtrip");
    std::thread::sleep(Duration::from_millis(std::env::var("LOCK_HOLD_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500)));
    ls.destroy();
    l.unlock_and_destroy();
    q.roundtrip(&mut s).expect("roundtrip");
    println!("unlocked");
}

fn rustix_memfd(size: usize) -> std::fs::File {
    let name = c"lock_check";
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create");
    let f: std::fs::File = unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) };
    f.set_len(size as u64).expect("set_len");
    f
}
