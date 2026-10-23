//! The Wayland side: each X top-level window is an xdg_toplevel drawn from shm buffers; loom's pointer and keyboard come back as X input.

use std::{
    os::fd::{AsFd, FromRawFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat,
        wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::{
    wp::{
        fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
        linux_dmabuf::zv1::client::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1},
        viewporter::client::{wp_viewport, wp_viewporter},
    },
    xdg::shell::client::{xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base},
};

use crate::server::Server;

pub struct Wl {
    pub conn: Connection,
    pub qh: QueueHandle<Server>,
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    wm: xdg_wm_base::XdgWmBase,
    pub seat: Option<wl_seat::WlSeat>,
    /// GPU buffers straight to loom (zero-copy Present)
    dmabuf: Option<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
    pub data_control: Option<wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    /// X pixels per logical pixel: X draws at the screen's own pixels (sharp), loom shows them at logical size
    pub scale: f64,
}

/// One shm buffer; `busy` until loom releases it.
struct Buf {
    buffer: wl_buffer::WlBuffer,
    ptr: *mut u8,
    len: usize,
    size: (u32, u32),
    argb: bool,
    busy: Arc<AtomicBool>,
}

impl Drop for Buf {
    fn drop(&mut self) {
        self.buffer.destroy();
        unsafe { libc::munmap(self.ptr as *mut _, self.len) };
    }
}

/// An X top-level window's Wayland half: its surface, what it shows, the state of its frames.
pub struct Top {
    pub surface: wl_surface::WlSurface,
    pub xdg: xdg_surface::XdgSurface,
    /// a normal window, or a menu or tooltip (override-redirect) shown as a popup of one
    pub toplevel: Option<xdg_toplevel::XdgToplevel>,
    popup: Option<xdg_popup::XdgPopup>,
    pub img: tiny_skia::Pixmap,
    /// loom configured it at least once (before that nothing is shown or exposed)
    pub configured: bool,
    pub dirty: bool,
    frame_pending: bool,
    /// the size loom asked for in the last toplevel configure (0: the window's own)
    pending: (u32, u32),
    bufs: Vec<Buf>,
    /// a GPU pixmap shown as it is (its content is not in `img`)
    pub external: Option<u32>,
    viewport: Option<wp_viewport::WpViewport>,
    _fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
}

impl Drop for Top {
    // unmapped or destroyed: gone from loom
    fn drop(&mut self) {
        if let Some(v) = &self.viewport {
            v.destroy();
        }
        if let Some(f) = &self._fractional {
            f.destroy();
        }
        if let Some(t) = &self.toplevel {
            t.destroy();
        }
        if let Some(p) = &self.popup {
            p.destroy();
        }
        self.xdg.destroy();
        self.surface.destroy();
    }
}

pub fn connect(scale: f64) -> Result<(Wl, EventQueue<Server>), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init::<Server>(&conn)?;
    let qh = queue.handle();
    let compositor = globals.bind(&qh, 4..=6, ())?;
    let shm = globals.bind(&qh, 1..=1, ())?;
    let wm = globals.bind(&qh, 1..=6, ())?;
    let seat = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 1..=7, ()).ok();
    let viewporter = globals.bind(&qh, 1..=1, ()).ok();
    let fractional = globals.bind(&qh, 1..=1, ()).ok();
    let viewporter_ok = viewporter.is_some();
    let data_control = globals.bind(&qh, 1..=1, ()).ok();
    let dmabuf = globals.bind(&qh, 3..=3, ()).ok();
    Ok((
        Wl {
            conn,
            qh,
            compositor,
            shm,
            wm,
            seat,
            data_control,
            dmabuf,
            viewporter,
            fractional,
            // without a viewport loom would show X pixels as logical ones
            scale: if viewporter_ok { scale } else { 1.0 },
        },
        queue,
    ))
}

impl Wl {
    fn surface(&self, win: u32, w: u32, h: u32) -> Top {
        let surface = self.compositor.create_surface(&self.qh, win);
        let xdg = self.wm.get_xdg_surface(&surface, &self.qh, win);
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, &self.qh, ()));
        let fractional = self.fractional.as_ref().map(|f| f.get_fractional_scale(&surface, &self.qh, ()));
        Top { surface, xdg, toplevel: None, popup: None, img: new_img(w, h), configured: false, dirty: false, frame_pending: false, pending: (0, 0), bufs: Vec::new(), external: None, viewport, _fractional: fractional }
    }

    /// A new toplevel for X window `win`, at its size, titled later from its properties.
    pub fn top(&self, win: u32, w: u32, h: u32) -> Top {
        let mut t = self.surface(win, w, h);
        t.toplevel = Some(t.xdg.get_toplevel(&self.qh, win));
        t.surface.commit();
        t
    }

    /// A menu or tooltip at (x, y) X pixels from `parent`'s corner, exactly where the program put it.
    pub fn popup(&self, win: u32, parent: &Top, x: i32, y: i32, w: u32, h: u32) -> Top {
        let mut t = self.surface(win, w, h);
        let k = self.scale;
        let p = self.wm.create_positioner(&self.qh, ());
        let (lw, lh) = (((w as f64 / k).round() as i32).max(1), ((h as f64 / k).round() as i32).max(1));
        p.set_size(lw, lh);
        p.set_anchor_rect((x as f64 / k).round() as i32, (y as f64 / k).round() as i32, 1, 1);
        p.set_anchor(xdg_positioner::Anchor::TopLeft);
        p.set_gravity(xdg_positioner::Gravity::BottomRight);
        // kept on the screen, as X window managers do not, but a Wayland screen edge would cut
        p.set_constraint_adjustment(xdg_positioner::ConstraintAdjustment::SlideX | xdg_positioner::ConstraintAdjustment::SlideY);
        t.popup = Some(t.xdg.get_popup(Some(&parent.xdg), &p, &self.qh, win));
        p.destroy();
        t.surface.commit();
        t
    }

    /// Shows what changed: the picture copied into a free buffer, one frame at a time.
    pub fn present(&self, top: &mut Top, shape: Option<&crate::ext::Shape>, alpha: bool) {
        let argb = alpha || shape.is_some();
        if !top.configured || !top.dirty || top.frame_pending {
            return;
        }
        let (w, h) = (top.img.width(), top.img.height());
        top.bufs
            .retain(|b| (b.size == (w, h) && b.argb == argb) || b.busy.load(Ordering::Acquire));
        let i = match top
            .bufs
            .iter()
            .position(|b| b.size == (w, h) && b.argb == argb && !b.busy.load(Ordering::Acquire))
        {
            Some(i) => i,
            None => match self.buffer(w, h, argb) {
                Some(b) => {
                    top.bufs.push(b);
                    top.bufs.len() - 1
                }
                None => return,
            },
        };
        let b = &top.bufs[i];
        // RGBA (opaque) to XRGB8888: bytes B, G, R, X; a shaped window clear outside its shape (ARGB, premultiplied)
        let dst = unsafe { std::slice::from_raw_parts_mut(b.ptr, b.len) };
        // the shape by index when it is the picture's size (the usual case), else per pixel
        let same = shape.is_some_and(|sh| (sh.w, sh.h) == (w, h));
        for (i, (d, s)) in dst.chunks_exact_mut(4).zip(top.img.data().chunks_exact(4)).enumerate() {
            let inside = match shape {
                None => true,
                Some(sh) if same => sh.inside[i],
                Some(sh) => sh.at(i as u32 % w, i as u32 / w),
            };
            if inside {
                // premultiplied already, as Wayland's ARGB8888 wants
                d.copy_from_slice(&[s[2], s[1], s[0], if alpha { s[3] } else { 255 }]);
            } else {
                d.copy_from_slice(&[0, 0, 0, 0]);
            }
        }
        b.busy.store(true, Ordering::Release);
        if let Some(v) = &top.viewport {
            v.set_destination(
                ((w as f64 / self.scale).round() as i32).max(1),
                ((h as f64 / self.scale).round() as i32).max(1),
            );
        }
        top.surface.attach(Some(&b.buffer), 0, 0);
        top.surface.damage_buffer(0, 0, w as i32, h as i32);
        top.surface.frame(&self.qh, top.surface.clone());
        top.surface.commit();
        top.dirty = false;
        top.frame_pending = true;
    }

    fn buffer(&self, w: u32, h: u32, argb: bool) -> Option<Buf> {
        let len = (w * h * 4) as usize;
        let fd = unsafe { libc::memfd_create(c"loom-x".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(std::os::fd::AsRawFd::as_raw_fd(&fd), len as i64) } < 0 {
            return None;
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return None;
        }
        let pool = self.shm.create_pool(fd.as_fd(), len as i32, &self.qh, ());
        let busy = Arc::new(AtomicBool::new(false));
        let buffer = pool.create_buffer(
            0,
            w as i32,
            h as i32,
            (w * 4) as i32,
            if argb { wl_shm::Format::Argb8888 } else { wl_shm::Format::Xrgb8888 },
            &self.qh,
            busy.clone(),
        );
        pool.destroy();
        Some(Buf {
            buffer,
            ptr: ptr as *mut u8,
            len,
            size: (w, h),
            argb,
            busy,
        })
    }
}

pub fn new_img(w: u32, h: u32) -> tiny_skia::Pixmap {
    tiny_skia::Pixmap::new(w.max(1), h.max(1)).expect("a window picture")
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Server {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(Server: ignore wl_compositor::WlCompositor);
delegate_noop!(Server: ignore wl_shm::WlShm);
delegate_noop!(Server: ignore wl_shm_pool::WlShmPool);

impl Dispatch<wl_surface::WlSurface, u32> for Server {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, Arc<AtomicBool>> for Server {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        e: wl_buffer::Event,
        busy: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = e {
            busy.store(false, Ordering::Release);
        }
    }
}

// a frame shown: the next one may go
impl Dispatch<wl_callback::WlCallback, wl_surface::WlSurface> for Server {
    fn event(
        s: &mut Self,
        _: &wl_callback::WlCallback,
        e: wl_callback::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = e {
            if let Some(t) = s.top_of_surface(surface) {
                t.frame_pending = false;
            }
            // Present completions for that window go out with its frame
            if let Some(w) = surface.data::<u32>().copied() {
                s.present_frame_done(w);
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Server {
    fn event(
        _: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, u32> for Server {
    fn event(
        s: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        win: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            let Some(pending) = s
                .wins
                .get(win)
                .and_then(|w| w.top.as_ref())
                .map(|t| t.pending)
            else {
                return;
            };
            s.top_configured(*win, pending);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, u32> for Server {
    fn event(
        s: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        e: xdg_toplevel::Event,
        win: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                if let Some(t) = s.wins.get_mut(win).and_then(|w| w.top.as_mut()) {
                    t.pending = (width.max(0) as u32, height.max(0) as u32);
                }
            }
            xdg_toplevel::Event::Close => s.close_request(*win),
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Server {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        e: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(c),
        } = e
        {
            if c.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if c.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Server {
    fn event(
        s: &mut Self,
        _: &wl_pointer::WlPointer,
        e: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                let win = surface.data::<u32>().copied();
                let k = s.wl.scale;
                s.pointer_enter(win, surface_x * k, surface_y * k);
            }
            wl_pointer::Event::Leave { .. } => s.pointer_enter(None, 0.0, 0.0),
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => s.pointer_motion(surface_x * s.wl.scale, surface_y * s.wl.scale),
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(st),
                ..
            } => {
                // evdev BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, side buttons: X buttons 1, 3, 2, 8, 9
                let b = match button {
                    0x110 => 1,
                    0x111 => 3,
                    0x112 => 2,
                    0x113 => 8,
                    0x114 => 9,
                    _ => return,
                };
                s.pointer_button(b, st == wl_pointer::ButtonState::Pressed);
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(a),
                value,
                ..
            } => {
                // a wheel step is a press and release of buttons 4/5 (vertical), 6/7 (horizontal)
                let b = match (a, value > 0.0) {
                    (wl_pointer::Axis::VerticalScroll, false) => 4,
                    (wl_pointer::Axis::VerticalScroll, true) => 5,
                    (wl_pointer::Axis::HorizontalScroll, false) => 6,
                    (_, true) => 7,
                    _ => return,
                };
                s.pointer_button(b, true);
                s.pointer_button(b, false);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Server {
    fn event(
        s: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd,
                size,
            } => s.keymap_fd(fd, size as usize),
            wl_keyboard::Event::Enter { surface, keys, .. } => {
                let pressed: Vec<u32> = keys
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                s.keyboard_enter(surface.data::<u32>().copied(), &pressed);
            }
            wl_keyboard::Event::Leave { .. } => s.keyboard_enter(None, &[]),
            wl_keyboard::Event::RepeatInfo { rate, delay } => s.repeat = (delay.max(0) as u32, rate.max(0) as u32),
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(st),
                ..
            } => s.key(key + 8, st == wl_keyboard::KeyState::Pressed),
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                ..
            } => s.modifiers(mods_depressed | mods_latched | mods_locked),
            _ => {}
        }
    }
}

delegate_noop!(Server: ignore wp_viewporter::WpViewporter);
delegate_noop!(Server: ignore wp_viewport::WpViewport);
delegate_noop!(Server: ignore wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for Server {
    fn event(
        s: &mut Self,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        e: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = e {
            s.scale_changed(scale as f64 / 120.0);
        }
    }
}

delegate_noop!(Server: ignore xdg_positioner::XdgPositioner);

impl Dispatch<xdg_popup::XdgPopup, u32> for Server {
    fn event(s: &mut Self, _: &xdg_popup::XdgPopup, e: xdg_popup::Event, win: &u32, _: &Connection, _: &QueueHandle<Self>) {
        // loom closed it (a click elsewhere): the program is told its menu went
        if let xdg_popup::Event::PopupDone = e {
            s.popup_done(*win);
        }
    }
}

/// The pixmap a GPU wl_buffer shows (its release frees the pixmap for the program again).
pub struct DmaRelease(pub u32);

impl Wl {
    /// A wl_buffer on a client's GPU buffer (one plane, linear), without copying it.
    pub fn dmabuf_buffer(&self, fd: std::os::fd::BorrowedFd, offset: u32, stride: u32, w: u32, h: u32, alpha: bool, pixmap: u32) -> Option<wl_buffer::WlBuffer> {
        let d = self.dmabuf.as_ref()?;
        let params = d.create_params(&self.qh, ());
        params.add(fd, 0, offset, stride, 0, 0);
        // XR24 / AR24
        let fourcc = if alpha { 0x34325241 } else { 0x34325258 };
        let b = params.create_immed(w as i32, h as i32, fourcc, zwp_linux_buffer_params_v1::Flags::empty(), &self.qh, DmaRelease(pixmap));
        params.destroy();
        Some(b)
    }

    /// Shows a GPU buffer in a top-level window as it is (zero-copy), at its logical size.
    pub fn show_external(&self, top: &mut Top, buffer: &wl_buffer::WlBuffer, w: u32, h: u32, pixmap: u32) {
        if let Some(v) = &top.viewport {
            v.set_destination(((w as f64 / self.scale).round() as i32).max(1), ((h as f64 / self.scale).round() as i32).max(1));
        }
        top.surface.attach(Some(buffer), 0, 0);
        top.surface.damage_buffer(0, 0, w as i32, h as i32);
        top.surface.frame(&self.qh, top.surface.clone());
        top.surface.commit();
        top.frame_pending = true;
        top.dirty = false;
        top.external = Some(pixmap);
    }
}

delegate_noop!(Server: ignore zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1);
delegate_noop!(Server: ignore zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1);

impl Dispatch<wl_buffer::WlBuffer, DmaRelease> for Server {
    fn event(s: &mut Self, _: &wl_buffer::WlBuffer, e: wl_buffer::Event, d: &DmaRelease, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = e {
            s.dmabuf_released(d.0);
        }
    }
}
