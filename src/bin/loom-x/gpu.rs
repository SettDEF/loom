//! GPU programs (EGL, Vulkan): DRI3 (the render node, buffers as file descriptors), SYNC fences (xshmfence futexes), Present (buffers shown in windows, paced by loom's frames).

use std::{
    collections::HashMap,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use x11rb_protocol::protocol::Request;

use crate::server::{Rep, Server};

pub const PRESENT_MAJOR: u8 = 137;
/// DRM_FORMAT_MOD_LINEAR: the only layout offered, so buffers can be read by mapping them
const MOD_LINEAR: u64 = 0;

/// A client's GPU buffer behind a pixmap: its plane, mapped to read.
pub struct DmaBuf {
    pub fd: OwnedFd,
    /// loom's wl_buffer on it, made the first time it is shown
    pub wl: Option<wayland_client::protocol::wl_buffer::WlBuffer>,
    ptr: *mut u8,
    len: usize,
    pub stride: u32,
    pub offset: u32,
}

impl DmaBuf {
    /// The buffer; mapped for CPU reads when the driver allows it (zero-copy needs no mapping).
    fn new(fd: OwnedFd, stride: u32, offset: u32, _height: u32) -> Option<DmaBuf> {
        // a dma-buf's size is where seeking to its end lands
        let size = unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_END) };
        unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET) };
        let len = if size > 0 { size as usize } else { 0 };
        let p = if len > 0 { unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, fd.as_raw_fd(), 0) } } else { libc::MAP_FAILED };
        let (ptr, len) = if p == libc::MAP_FAILED { (std::ptr::null_mut(), 0) } else { (p as *mut u8, len) };
        Some(DmaBuf { fd, wl: None, ptr, len, stride, offset })
    }

    /// The buffer's bytes, after the GPU is done with it (DMA_BUF_IOCTL_SYNC waits for its fences); empty when not mapped.
    pub fn with<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T {
        if self.ptr.is_null() {
            return f(&[]);
        }
        // struct dma_buf_sync { u64 flags }: start (0) | read (1), end (4) | read (1)
        const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x40086200;
        let start: u64 = 1;
        let end: u64 = 4 | 1;
        unsafe { libc::ioctl(self.fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &start) };
        let r = f(unsafe { std::slice::from_raw_parts(self.ptr, self.len) });
        unsafe { libc::ioctl(self.fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &end) };
        r
    }
}

impl Drop for DmaBuf {
    fn drop(&mut self) {
        if let Some(b) = self.wl.take() {
            b.destroy();
        }
        if !self.ptr.is_null() {
            unsafe { libc::munmap(self.ptr as *mut _, self.len) };
        }
    }
}

/// A SYNC fence: in shared memory (xshmfence, from DRI3) or just a flag.
pub struct Fence {
    shm: Option<(*mut i32, usize)>,
    triggered: bool,
}

impl Fence {
    fn trigger(&mut self) {
        self.triggered = true;
        if let Some((p, _)) = self.shm {
            // xshmfence: 1 = triggered; whoever waits sleeps on the futex
            unsafe {
                std::sync::atomic::AtomicI32::from_ptr(p).store(1, std::sync::atomic::Ordering::SeqCst);
                libc::syscall(libc::SYS_futex, p, libc::FUTEX_WAKE, i32::MAX, 0, 0, 0);
            }
        }
    }
    fn reset(&mut self) {
        self.triggered = false;
        if let Some((p, _)) = self.shm {
            unsafe { std::sync::atomic::AtomicI32::from_ptr(p).store(0, std::sync::atomic::Ordering::SeqCst) };
        }
    }
}

impl Drop for Fence {
    fn drop(&mut self) {
        if let Some((p, len)) = self.shm {
            unsafe { libc::munmap(p as *mut _, len) };
        }
    }
}

/// A Present completion waiting for loom's next frame of that window.
pub struct Pending {
    pub client: usize,
    eid: u32,
    window: u32,
    serial: u32,
    kind: u8,
}

#[derive(Default)]
pub struct Gpu {
    pub fences: HashMap<u32, Fence>,
    /// Present event selections: (client, event id, window, mask)
    pub selected: Vec<(usize, u32, u32, u32)>,
    pub pending: HashMap<u32, Vec<Pending>>,
    /// frames shown (Present's MSC)
    pub msc: u64,
    /// pixmaps loom shows zero-copy, freed for the program when loom releases them: (window, serial, idle fence)
    pub idle_wait: HashMap<u32, (u32, u32, u32)>,
}

fn ust() -> u64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1000
}

impl Server {
    pub fn handle_gpu<'a>(&mut self, idx: usize, seq: u16, major: u8, req: Request<'a>) -> Result<(), Request<'a>> {
        match req {
            // ---- DRI3 ----
            Request::Dri3QueryVersion(_) => self.reply(idx, Rep::new(seq, 0).u32(1).u32(2)),
            Request::Dri3Open(_) => {
                let fd = unsafe { libc::open(c"/dev/dri/renderD128".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
                if fd < 0 {
                    return Ok(self.error(idx, 10, 0, major, 1));
                }
                self.reply(idx, Rep::new(seq, 1).pad(24));
                self.pass_fd(idx, unsafe { OwnedFd::from_raw_fd(fd) });
            }
            Request::Dri3GetSupportedModifiers(_) => self.reply(idx, Rep::new(seq, 0).u32(1).u32(1).pad(16).u32(MOD_LINEAR as u32).u32((MOD_LINEAR >> 32) as u32).u32(MOD_LINEAR as u32).u32((MOD_LINEAR >> 32) as u32)),
            Request::Dri3PixmapFromBuffer(r) => {
                let buf = DmaBuf::new(r.pixmap_fd, r.stride as u32, 0, r.height as u32);
                self.gpu_pixmap(idx, major, r.pixmap, r.width as u32, r.height as u32, r.depth, buf);
            }
            Request::Dri3PixmapFromBuffers(r) => {
                let fd = r.buffers.into_iter().next();
                let buf = fd.and_then(|fd| DmaBuf::new(fd, r.stride0, r.offset0, r.height as u32));
                self.gpu_pixmap(idx, major, r.pixmap, r.width as u32, r.height as u32, r.depth, buf);
            }
            Request::Dri3FenceFromFD(r) => {
                let len = 4096;
                let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, r.fence_fd.as_raw_fd(), 0) };
                let shm = (p != libc::MAP_FAILED).then_some((p as *mut i32, len));
                let mut f = Fence { shm, triggered: false };
                if r.initially_triggered {
                    f.trigger();
                }
                self.gpu.fences.insert(r.fence, f);
            }
            Request::Dri3BufferFromPixmap(_) | Request::Dri3BuffersFromPixmap(_) | Request::Dri3FDFromFence(_) | Request::Dri3ImportSyncobj(_) => self.error(idx, 1, 0, major, 0),
            Request::Dri3FreeSyncobj(_) | Request::Dri3SetDRMDeviceInUse(_) => {}
            // ---- SYNC (fences; counters and alarms accepted) ----
            Request::SyncInitialize(_) => self.reply(idx, Rep::new(seq, 0).u8(3).u8(1)),
            Request::SyncCreateFence(r) => {
                self.gpu.fences.insert(r.fence, Fence { shm: None, triggered: r.initially_triggered });
            }
            Request::SyncTriggerFence(r) => {
                if let Some(f) = self.gpu.fences.get_mut(&r.fence) {
                    f.trigger();
                }
            }
            Request::SyncResetFence(r) => {
                if let Some(f) = self.gpu.fences.get_mut(&r.fence) {
                    f.reset();
                }
            }
            Request::SyncDestroyFence(r) => {
                self.gpu.fences.remove(&r.fence);
            }
            Request::SyncQueryFence(r) => {
                let t = self.gpu.fences.get(&r.fence).is_some_and(|f| f.triggered);
                self.reply(idx, Rep::new(seq, 0).u8(t as u8));
            }
            // every fence loom-x knows is triggered by the time anyone waits on it
            Request::SyncAwaitFence(_) | Request::SyncAwait(_) => {}
            Request::SyncListSystemCounters(_) => self.reply(idx, Rep::new(seq, 0).u32(0)),
            Request::SyncCreateCounter(_) | Request::SyncDestroyCounter(_) | Request::SyncSetCounter(_) | Request::SyncChangeCounter(_) | Request::SyncCreateAlarm(_) | Request::SyncChangeAlarm(_) | Request::SyncDestroyAlarm(_) | Request::SyncSetPriority(_) => {}
            Request::SyncQueryCounter(_) => self.reply(idx, Rep::new(seq, 0).u32(0).u32(0)),
            Request::SyncGetPriority(_) => self.reply(idx, Rep::new(seq, 0).u32(0)),
            // ---- Present ----
            Request::PresentQueryVersion(_) => self.reply(idx, Rep::new(seq, 0).u32(1).u32(2)),
            Request::PresentQueryCapabilities(_) => self.reply(idx, Rep::new(seq, 0).u32(0)),
            Request::PresentSelectInput(r) => {
                let (eid, w, mask) = (r.eid, r.window, u32::from(r.event_mask));
                self.gpu.selected.retain(|(c, e, ..)| !(*c == idx && *e == eid));
                if mask != 0 {
                    self.gpu.selected.push((idx, eid, w, mask));
                }
            }
            Request::PresentPixmap(r) => {
                let t0 = std::time::Instant::now();
                // a GPU buffer the size of the window: shown as it is; idle once loom lets go of it
                if r.x_off == 0 && r.y_off == 0 && self.present_zero_copy(r.window, r.pixmap) {
                    if std::env::var_os("LOOM_X_TRACE").is_some() {
                        eprintln!("loom-x: {} ms present pixmap {:#x} serial {} zero-copy", self.now(), r.pixmap, r.serial);
                    }
                    self.gpu.idle_wait.insert(r.pixmap, (r.window, r.serial, r.idle_fence));
                    self.present_complete_later(idx, r.window, r.serial, 0);
                    return Ok(());
                }
                self.present(r.window, r.pixmap, r.x_off as i32, r.y_off as i32);
                if std::env::var_os("LOOM_X_TRACE").is_some() {
                    eprintln!("loom-x: {} ms present pixmap {:#x} serial {} copy {} us", self.now(), r.pixmap, r.serial, t0.elapsed().as_micros());
                }
                // the pixmap was copied: free again at once
                if let Some(f) = self.gpu.fences.get_mut(&r.idle_fence) {
                    f.trigger();
                }
                self.present_event_idle(r.window, r.serial, r.pixmap, r.idle_fence);
                self.present_complete_later(idx, r.window, r.serial, 0);
            }
            Request::PresentNotifyMSC(r) => self.present_complete_later(idx, r.window, r.serial, 1),
            Request::PresentPixmapSynced(_) => self.error(idx, 1, 0, major, 0),
            other => return Err(other),
        }
        Ok(())
    }

    fn gpu_pixmap(&mut self, idx: usize, major: u8, pid: u32, w: u32, h: u32, depth: u8, buf: Option<DmaBuf>) {
        let Some(buf) = buf else { return self.error(idx, 11, pid, major, 2) };
        self.dri3_pixmap(pid, w, h, depth, buf);
    }

    /// Sends CompleteNotify when loom shows the window's next frame (vsync pacing), at once if it is not shown.
    fn present_complete_later(&mut self, client: usize, window: u32, serial: u32, kind: u8) {
        let eids: Vec<u32> = self.gpu.selected.iter().filter(|(c, _, w, m)| *c == client && *w == window && m & 2 != 0).map(|(_, e, ..)| *e).collect();
        let top = self.top_of(window);
        for eid in eids {
            let p = Pending { client, eid, window, serial, kind };
            match top.filter(|t| self.is_shown(*t)) {
                Some(t) => self.gpu.pending.entry(t).or_default().push(p),
                None => self.present_complete(p),
            }
        }
    }

    fn present_complete(&mut self, p: Pending) {
        if std::env::var_os("LOOM_X_TRACE").is_some() {
            eprintln!("loom-x: {} ms present complete window {:#x} serial {} kind {}", self.now(), p.window, p.serial, p.kind);
        }
        self.gpu.msc += 1;
        let mut ev = vec![35, PRESENT_MAJOR, 0, 0];
        ev.extend_from_slice(&2u32.to_le_bytes());
        ev.extend_from_slice(&1u16.to_le_bytes());
        // kind (pixmap / notify-msc), mode: copy
        ev.extend_from_slice(&[p.kind, 0]);
        ev.extend_from_slice(&p.eid.to_le_bytes());
        ev.extend_from_slice(&p.window.to_le_bytes());
        ev.extend_from_slice(&p.serial.to_le_bytes());
        ev.extend_from_slice(&ust().to_le_bytes());
        ev.extend_from_slice(&self.gpu.msc.to_le_bytes());
        self.send_generic(p.client, ev);
    }

    /// loom showed a frame of top-level `t`: its waiting completions go out.
    pub fn present_frame_done(&mut self, t: u32) {
        if let Some(list) = self.gpu.pending.remove(&t) {
            for p in list {
                self.present_complete(p);
            }
        }
    }

    fn present_event_idle(&mut self, window: u32, serial: u32, pixmap: u32, fence: u32) {
        let to: Vec<(usize, u32)> = self.gpu.selected.iter().filter(|(_, _, w, m)| *w == window && m & 4 != 0).map(|(c, e, ..)| (*c, *e)).collect();
        for (c, eid) in to {
            let mut ev = vec![35, PRESENT_MAJOR, 0, 0, 0, 0, 0, 0];
            ev.extend_from_slice(&2u16.to_le_bytes());
            ev.extend_from_slice(&[0, 0]);
            for v in [eid, window, serial, pixmap, fence] {
                ev.extend_from_slice(&v.to_le_bytes());
            }
            self.send_generic(c, ev);
        }
    }

    /// Present's ConfigureNotify (programs size their buffers by it) when a window changes size.
    pub fn present_configure(&mut self, window: u32, x: i32, y: i32, w: u32, h: u32) {
        let to: Vec<(usize, u32)> = self.gpu.selected.iter().filter(|(_, _, win, m)| *win == window && m & 1 != 0).map(|(c, e, ..)| (*c, *e)).collect();
        for (c, eid) in to {
            let mut ev = vec![35, PRESENT_MAJOR, 0, 0];
            ev.extend_from_slice(&2u32.to_le_bytes());
            ev.extend_from_slice(&0u16.to_le_bytes());
            ev.extend_from_slice(&[0, 0]);
            ev.extend_from_slice(&eid.to_le_bytes());
            ev.extend_from_slice(&window.to_le_bytes());
            for v in [x as i16 as u16, y as i16 as u16, w as u16, h as u16, 0, 0, w as u16, h as u16] {
                ev.extend_from_slice(&v.to_le_bytes());
            }
            ev.extend_from_slice(&0u32.to_le_bytes());
            self.send_generic(c, ev);
        }
    }
}

impl Server {
    /// loom let go of a zero-copy buffer: the program may draw into it again.
    pub fn dmabuf_released(&mut self, pixmap: u32) {
        if let Some((window, serial, fence)) = self.gpu.idle_wait.remove(&pixmap) {
            if let Some(f) = self.gpu.fences.get_mut(&fence) {
                f.trigger();
            }
            self.present_event_idle(window, serial, pixmap, fence);
        }
    }
}
