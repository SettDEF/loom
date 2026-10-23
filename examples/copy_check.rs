//! copy_check: copies the screen through ext-image-copy-capture into a GPU buffer (gbm, a modifier
//! loom offers), reads it back and prints `ready WxH MODIFIER avg R G B` (or `failed …`).

use std::os::fd::AsFd;

use smithay::reexports::gbm::{self, BufferObjectFlags, Format, Modifier};
use wayland_client::{
    protocol::{wl_buffer, wl_output, wl_registry},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::{
    ext::{
        image_capture_source::v1::client::{ext_image_capture_source_v1 as source, ext_output_image_capture_source_manager_v1 as osm},
        image_copy_capture::v1::client::{ext_image_copy_capture_frame_v1 as frame, ext_image_copy_capture_manager_v1 as mgr, ext_image_copy_capture_session_v1 as session},
    },
    wp::linux_dmabuf::zv1::client::{zwp_linux_buffer_params_v1 as params, zwp_linux_dmabuf_v1 as dmabuf},
};

#[derive(Default)]
struct S {
    output: Option<wl_output::WlOutput>,
    mgr: Option<mgr::ExtImageCopyCaptureManagerV1>,
    osm: Option<osm::ExtOutputImageCaptureSourceManagerV1>,
    dmabuf: Option<dmabuf::ZwpLinuxDmabufV1>,
    size: (u32, u32),
    formats: Vec<(u32, Vec<u64>)>,
    device: bool,
    done: bool,
    result: Option<Result<(), u32>>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for S {
    fn event(s: &mut Self, r: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, .. } = e {
            match interface.as_str() {
                "wl_output" if s.output.is_none() => s.output = Some(r.bind(name, 4, qh, ())),
                "ext_image_copy_capture_manager_v1" => s.mgr = Some(r.bind(name, 1, qh, ())),
                "ext_output_image_capture_source_manager_v1" => s.osm = Some(r.bind(name, 1, qh, ())),
                "zwp_linux_dmabuf_v1" => s.dmabuf = Some(r.bind(name, 3, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<session::ExtImageCopyCaptureSessionV1, ()> for S {
    fn event(s: &mut Self, _: &session::ExtImageCopyCaptureSessionV1, e: session::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            session::Event::BufferSize { width, height } => s.size = (width, height),
            session::Event::DmabufDevice { .. } => s.device = true,
            session::Event::DmabufFormat { format, modifiers } => s.formats.push((format, modifiers.chunks_exact(8).map(|c| u64::from_ne_bytes(c.try_into().unwrap())).collect())),
            session::Event::Done => s.done = true,
            session::Event::Stopped => s.result = Some(Err(99)),
            _ => {}
        }
    }
}

impl Dispatch<frame::ExtImageCopyCaptureFrameV1, ()> for S {
    fn event(s: &mut Self, _: &frame::ExtImageCopyCaptureFrameV1, e: frame::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            frame::Event::Ready => s.result = Some(Ok(())),
            frame::Event::Failed { reason } => s.result = Some(Err(reason.into())),
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
ignore!(wl_output::WlOutput, mgr::ExtImageCopyCaptureManagerV1, osm::ExtOutputImageCaptureSourceManagerV1, source::ExtImageCaptureSourceV1, dmabuf::ZwpLinuxDmabufV1, params::ZwpLinuxBufferParamsV1, wl_buffer::WlBuffer);

fn main() {
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let mut q = conn.new_event_queue();
    let qh = q.handle();
    conn.display().get_registry(&qh, ());
    let mut s = S::default();
    q.roundtrip(&mut s).expect("roundtrip");
    let (Some(out), Some(m), Some(o), Some(d)) = (s.output.clone(), s.mgr.clone(), s.osm.clone(), s.dmabuf.clone()) else {
        println!("failed: missing globals");
        std::process::exit(1);
    };
    let src = o.create_source(&out, &qh, ());
    let sess = m.create_session(&src, mgr::Options::empty(), &qh, ());
    while !s.done && s.result.is_none() {
        q.blocking_dispatch(&mut s).expect("dispatch");
    }
    let Some((fourcc, mods)) = s.formats.iter().find(|(f, _)| *f == Format::Argb8888 as u32).cloned().filter(|_| s.device) else {
        println!("failed: no GPU buffers offered");
        std::process::exit(1);
    };
    let node = std::fs::OpenOptions::new().read(true).write(true).open("/dev/dri/renderD128").expect("render node");
    let gbm = gbm::Device::new(node).expect("gbm");
    let (w, h) = s.size;
    let bo = gbm.create_buffer_object_with_modifiers2::<()>(w, h, Format::Argb8888, mods.iter().map(|m| Modifier::from(*m)), BufferObjectFlags::RENDERING).expect("gbm bo");
    let modifier: u64 = bo.modifier().into();
    eprintln!("bo modifier {modifier:#x} of {} offered, {} planes", mods.len(), bo.plane_count());
    let p = d.create_params(&qh, ());
    // every plane (AMD's compressed modifiers keep their metadata in more than one)
    for i in 0..bo.plane_count() as i32 {
        let fd = bo.fd_for_plane(i).expect("fd");
        p.add(fd.as_fd(), i as u32, bo.offset(i), bo.stride_for_plane(i), (modifier >> 32) as u32, modifier as u32);
    }
    let buffer = p.create_immed(w as i32, h as i32, fourcc, params::Flags::empty(), &qh, ());
    let f = sess.create_frame(&qh, ());
    f.attach_buffer(&buffer);
    f.damage_buffer(0, 0, w as i32, h as i32);
    f.capture();
    while s.result.is_none() {
        q.blocking_dispatch(&mut s).expect("dispatch");
    }
    if let Some(Err(r)) = s.result {
        println!("failed: reason {r}");
        std::process::exit(1);
    }
    let avg = bo
        .map(0, 0, w, h, |mb| {
            let (stride, px) = (mb.stride() as usize, mb.buffer());
            let mut sum = [0u64; 3];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let i = y * stride + x * 4;
                    sum[0] += px[i + 2] as u64;
                    sum[1] += px[i + 1] as u64;
                    sum[2] += px[i] as u64;
                }
            }
            sum.map(|v| v / (w as u64 * h as u64))
        })
        .expect("map");
    println!("ready {w}x{h} modifier {modifier:#x} avg {} {} {}", avg[0], avg[1], avg[2]);
}
