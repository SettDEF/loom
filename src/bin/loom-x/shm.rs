//! MIT-SHM: images through shared memory (System V segments or passed file descriptors) instead of the socket.

use std::os::fd::{AsRawFd, OwnedFd};

use x11rb_protocol::protocol::{shm, Request};

use crate::server::{Rep, Server};

const SHM_MAJOR: u8 = 132;
const COMPLETION: u8 = 69;

/// A client's shared memory, mapped into loom-x.
pub struct Seg {
    ptr: *mut u8,
    size: usize,
    sysv: bool,
}

impl Seg {
    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.size) }
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) }
    }

    fn sysv(id: u32, read_only: bool) -> Option<Seg> {
        let mut st: libc::shmid_ds = unsafe { std::mem::zeroed() };
        if unsafe { libc::shmctl(id as i32, libc::IPC_STAT, &mut st) } != 0 {
            return None;
        }
        let p = unsafe { libc::shmat(id as i32, std::ptr::null(), if read_only { libc::SHM_RDONLY } else { 0 }) };
        if p as isize == -1 {
            return None;
        }
        Some(Seg { ptr: p as *mut u8, size: st.shm_segsz, sysv: true })
    }

    fn fd(fd: &OwnedFd, read_only: bool) -> Option<Seg> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 || st.st_size <= 0 {
            return None;
        }
        let size = st.st_size as usize;
        let prot = if read_only { libc::PROT_READ } else { libc::PROT_READ | libc::PROT_WRITE };
        let p = unsafe { libc::mmap(std::ptr::null_mut(), size, prot, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
        if p == libc::MAP_FAILED {
            return None;
        }
        Some(Seg { ptr: p as *mut u8, size, sysv: false })
    }
}

impl Drop for Seg {
    fn drop(&mut self) {
        unsafe {
            if self.sysv {
                libc::shmdt(self.ptr as *const _);
            } else {
                libc::munmap(self.ptr as *mut _, self.size);
            }
        }
    }
}

impl Server {
    /// The MIT-SHM requests; false when `req` is not one of them.
    pub fn handle_shm<'a>(&mut self, idx: usize, seq: u16, major: u8, req: Request<'a>) -> Result<(), Request<'a>> {
        match req {
            Request::ShmQueryVersion(_) => {
                let (uid, gid) = unsafe { (libc::getuid() as u16, libc::getgid() as u16) };
                // no shared pixmaps; images in ZPixmap
                self.reply(idx, Rep::new(seq, 0).u16(1).u16(2).u16(uid).u16(gid).u8(2));
            }
            Request::ShmAttach(r) => match Seg::sysv(r.shmid, r.read_only) {
                Some(s) => {
                    self.shm.insert(r.shmseg, s);
                }
                None => self.error(idx, 10, r.shmid, major, 1),
            },
            Request::ShmAttachFd(r) => match Seg::fd(&r.shm_fd, r.read_only) {
                Some(s) => {
                    self.shm.insert(r.shmseg, s);
                }
                None => self.error(idx, 10, r.shmseg, major, 6),
            },
            Request::ShmDetach(r) => {
                self.shm.remove(&r.shmseg);
            }
            Request::ShmPutImage(r) => self.shm_put(idx, major, r),
            Request::ShmGetImage(r) => self.shm_get(idx, seq, major, r),
            Request::ShmCreateSegment(r) => {
                // loom-x makes the memory and hands the program its file descriptor with the reply
                let fd = unsafe { libc::memfd_create(c"loom-x-shm".as_ptr(), libc::MFD_CLOEXEC) };
                if fd < 0 || unsafe { libc::ftruncate(fd, r.size as i64) } != 0 {
                    return Ok(self.error(idx, 11, r.shmseg, major, 7));
                }
                let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
                match Seg::fd(&fd, r.read_only) {
                    Some(s) => {
                        self.shm.insert(r.shmseg, s);
                        self.reply(idx, Rep::new(seq, 1).pad(24));
                        self.pass_fd(idx, fd);
                    }
                    None => self.error(idx, 11, r.shmseg, major, 7),
                }
            }
            Request::ShmCreatePixmap(_) => self.error(idx, 1, 0, major, 0),
            other => return Err(other),
        }
        Ok(())
    }

    fn shm_put(&mut self, idx: usize, major: u8, r: shm::PutImageRequest) {
        let Some(gc) = self.gc(r.gc) else { return self.error(idx, 13, r.gc, major, 3) };
        let Some(seg) = self.shm.get(&r.shmseg) else { return self.error(idx, 133, r.shmseg, major, 3) };
        let data = seg.bytes().get(r.offset as usize..).unwrap_or(&[]).to_vec();
        let (tw, sx, sy) = (r.total_width as u32, r.src_x as u32, r.src_y as u32);
        let (format, depth) = (r.format, r.depth);
        self.draw_image(r.drawable, &gc, r.dst_x as i32, r.dst_y as i32, r.src_width as u32, r.src_height as u32, |i, j, gc| crate::draw::image_pixel(format, depth, &data, tw, 0, gc, sx + i, sy + j));
        if r.send_event {
            let mut ev = [0u8; 32];
            ev[0] = COMPLETION;
            ev[4..8].copy_from_slice(&r.drawable.to_le_bytes());
            ev[8..10].copy_from_slice(&3u16.to_le_bytes());
            ev[10] = SHM_MAJOR;
            ev[12..16].copy_from_slice(&r.shmseg.to_le_bytes());
            ev[16..20].copy_from_slice(&r.offset.to_le_bytes());
            self.send_event(idx, ev);
        }
    }

    fn shm_get(&mut self, idx: usize, seq: u16, major: u8, r: shm::GetImageRequest) {
        let (w, h) = (r.width as u32, r.height as u32);
        let Some(depth) = self.depth_of(r.drawable) else { return self.error(idx, 9, r.drawable, major, 4) };
        let mut px = vec![0u32; (w * h) as usize];
        self.read_pixels(r.drawable, r.x as i32, r.y as i32, w, h, &mut px);
        let Some(seg) = self.shm.get_mut(&r.shmseg) else { return self.error(idx, 133, r.shmseg, major, 4) };
        let out = seg.bytes_mut().get_mut(r.offset as usize..).unwrap_or(&mut []);
        let n = (w * h * 4) as usize;
        for (o, p) in out.chunks_exact_mut(4).zip(&px).take(n / 4) {
            o.copy_from_slice(&p.to_le_bytes());
        }
        self.reply(idx, Rep::new(seq, depth).u32(crate::server::VISUAL).u32(n as u32));
    }
}
