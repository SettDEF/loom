//! The clipboard and middle-click selection shared with Wayland (ext-data-control): what a Wayland program copies X programs paste, and the other way round.

use std::{
    io::{Read, Write},
    os::fd::{AsFd, OwnedFd},
    sync::Mutex,
};

use smithay::reexports::calloop::{generic::Generic, Interest, Mode, PostAction};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{ext_data_control_device_v1 as device, ext_data_control_manager_v1 as manager, ext_data_control_offer_v1 as offer, ext_data_control_source_v1 as source};

use crate::server::Server;

/// The window loom-x owns X selections with (for Wayland's), and receives X owners' answers on.
pub const SEL_WIN: u32 = 0x101;
/// Offered with every selection loom-x hands Wayland, so it knows its own when it comes back.
const MARK: &str = "application/x-loom-x";
const TEXT: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];

#[derive(Default)]
pub struct Mimes(Mutex<Vec<String>>);

pub struct Clip {
    pub mgr: manager::ExtDataControlManagerV1,
    pub device: Option<device::ExtDataControlDeviceV1>,
    /// Wayland's current selections (clipboard, primary) when another program made them
    clipboard: Option<(offer::ExtDataControlOfferV1, Vec<String>)>,
    primary: Option<(offer::ExtDataControlOfferV1, Vec<String>)>,
    /// loom-x's own sources, for selections X programs own
    src_clipboard: Option<source::ExtDataControlSourceV1>,
    src_primary: Option<source::ExtDataControlSourceV1>,
    /// Wayland pastes waiting for the X owner's answer: (selection atom, the pipe to fill)
    waiting: Vec<(u32, OwnedFd)>,
    pub a_clipboard: u32,
    a_targets: u32,
    a_utf8: u32,
    a_text: u32,
    a_prop: u32,
}

impl Server {
    pub fn clip_init(&mut self, mgr: manager::ExtDataControlManagerV1) {
        let device = self.wl.seat.as_ref().map(|seat| mgr.get_data_device(seat, &self.wl.qh, ()));
        let (a_clipboard, a_targets, a_utf8, a_text, a_prop) = (self.intern(b"CLIPBOARD"), self.intern(b"TARGETS"), self.intern(b"UTF8_STRING"), self.intern(b"TEXT"), self.intern(b"_LOOM_SELECTION"));
        self.clip = Some(Clip { mgr, device, clipboard: None, primary: None, src_clipboard: None, src_primary: None, waiting: Vec::new(), a_clipboard, a_targets, a_utf8, a_text, a_prop });
    }

    fn is_shared(&self, sel: u32) -> bool {
        self.clip.as_ref().is_some_and(|c| sel == c.a_clipboard || sel == 1)
    }

    // ---- Wayland to X ----

    /// Wayland's selection changed: X programs see it (loom-x owns the X selection for it).
    fn wayland_selection(&mut self, primary: bool, o: Option<offer::ExtDataControlOfferV1>) {
        let Some(c) = self.clip.as_mut() else { return };
        let mimes = o.as_ref().and_then(|o| o.data::<Mimes>()).map(|m| m.0.lock().unwrap().clone()).unwrap_or_default();
        // our own, back from Wayland: nothing changed
        if mimes.iter().any(|m| m == MARK) {
            return;
        }
        let sel = if primary { 1 } else { c.a_clipboard };
        let slot = if primary { &mut c.primary } else { &mut c.clipboard };
        if let Some((old, _)) = slot.take() {
            old.destroy();
        }
        match o {
            Some(o) => {
                *slot = Some((o, mimes));
                self.take_selection(sel, crate::clip::SEL_WIN);
            }
            None => {
                if self.selections.get(&sel) == Some(&SEL_WIN) {
                    self.selections.remove(&sel);
                }
            }
        }
    }

    /// An X program pastes a selection Wayland has: answered from Wayland's offer.
    pub fn clip_convert(&mut self, requestor: u32, sel: u32, target: u32, property: u32, time: u32) {
        let Some(c) = self.clip.as_ref() else { return };
        let Some((o, mimes)) = (if sel == 1 { c.primary.as_ref() } else { c.clipboard.as_ref() }) else { return self.selection_notify(requestor, sel, target, 0, time) };
        let property = if property == 0 { target } else { property };
        let (a_targets, a_utf8, a_text) = (c.a_targets, c.a_utf8, c.a_text);
        if target == a_targets {
            let mut atoms = vec![a_targets];
            if mimes.iter().any(|m| TEXT.contains(&m.as_str())) {
                atoms.extend([a_utf8, a_text, 31]);
            }
            let others: Vec<String> = mimes.iter().filter(|m| !TEXT.contains(&m.as_str())).cloned().collect();
            for m in others {
                atoms.push(self.intern(m.as_bytes()));
            }
            let data = atoms.iter().flat_map(|a| a.to_le_bytes()).collect();
            self.set_prop(requestor, property, 4, 32, data);
            return self.selection_notify(requestor, sel, target, property, time);
        }
        let name = self.atom_name(target);
        let text = target == a_utf8 || target == a_text || target == 31;
        let mime = if text { TEXT.iter().find(|t| mimes.iter().any(|m| m == *t)).map(|t| t.to_string()) } else { mimes.iter().find(|m| **m == name).cloned() };
        let Some(mime) = mime else { return self.selection_notify(requestor, sel, target, 0, time) };
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return self.selection_notify(requestor, sel, target, 0, time);
        }
        let (r, w) = unsafe { (std::os::fd::FromRawFd::from_raw_fd(fds[0]), <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fds[1])) };
        let r: std::fs::File = r;
        o.receive(mime, w.as_fd());
        drop(w);
        let _ = self.wl.conn.flush();
        let type_ = if text { a_utf8 } else { target };
        // read without blocking the server; answered when the writer is done
        let mut got = Vec::new();
        let _ = self.handle.insert_source(Generic::new(r, Interest::READ, Mode::Level), move |_, f, s: &mut Server| {
            let mut buf = [0u8; 65536];
            match unsafe { f.get_mut() }.read(&mut buf) {
                Ok(0) | Err(_) => {
                    s.set_prop(requestor, property, type_, 8, std::mem::take(&mut got));
                    s.selection_notify(requestor, sel, target, property, time);
                    Ok(PostAction::Remove)
                }
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    Ok(PostAction::Continue)
                }
            }
        });
    }

    // ---- X to Wayland ----

    /// An X program took a shared selection: Wayland programs can paste it.
    pub fn clip_x_owned(&mut self, sel: u32) {
        if !self.is_shared(sel) {
            return;
        }
        let qh = self.wl.qh.clone();
        let Some(c) = self.clip.as_mut() else { return };
        let src = c.mgr.create_data_source(&qh, sel);
        for m in TEXT.iter().chain([&MARK]) {
            src.offer(m.to_string());
        }
        if let Some(d) = &c.device {
            if sel == 1 {
                d.set_primary_selection(Some(&src));
            } else {
                d.set_selection(Some(&src));
            }
        }
        let slot = if sel == 1 { &mut c.src_primary } else { &mut c.src_clipboard };
        if let Some(old) = slot.replace(src) {
            old.destroy();
        }
    }

    /// The X owner of a shared selection let go (or closed): Wayland's is cleared if it was ours.
    pub fn clip_x_cleared(&mut self, sel: u32) {
        if !self.is_shared(sel) {
            return;
        }
        let Some(c) = self.clip.as_mut() else { return };
        let slot = if sel == 1 { &mut c.src_primary } else { &mut c.src_clipboard };
        if let Some(old) = slot.take() {
            old.destroy();
            if let Some(d) = &c.device {
                if sel == 1 {
                    d.set_primary_selection(None);
                } else {
                    d.set_selection(None);
                }
            }
        }
    }

    /// A Wayland program pastes what an X program copied: asked of the X owner, answered on SEL_WIN.
    fn wayland_paste(&mut self, sel: u32, mime: String, fd: OwnedFd) {
        let Some(owner) = self.selections.get(&sel).copied().filter(|&o| o != SEL_WIN) else { return };
        let Some(c) = self.clip.as_mut() else { return };
        let target = if mime == "STRING" || mime == "text/plain" { 31 } else { c.a_utf8 };
        let prop = c.a_prop;
        c.waiting.push((sel, fd));
        let time = self.now();
        self.send_to(owner, crate::server::selection_request(time, owner, SEL_WIN, sel, target, prop));
    }

    /// The X owner answered (SelectionNotify to SEL_WIN): its data goes to the waiting Wayland program.
    pub fn clip_x_answer(&mut self, ev: &[u8; 32]) {
        let sel = u32::from_le_bytes([ev[12], ev[13], ev[14], ev[15]]);
        let property = u32::from_le_bytes([ev[20], ev[21], ev[22], ev[23]]);
        let Some(c) = self.clip.as_mut() else { return };
        let Some(i) = c.waiting.iter().position(|(s, _)| *s == sel) else { return };
        let (_, fd) = c.waiting.remove(i);
        let data = if property == 0 { Vec::new() } else { self.take_prop(SEL_WIN, property) };
        // written off the loop: a slow reader must not stall the server
        std::thread::spawn(move || {
            let mut f = std::fs::File::from(fd);
            let _ = f.write_all(&data);
        });
    }
}

impl Dispatch<manager::ExtDataControlManagerV1, ()> for Server {
    fn event(_: &mut Self, _: &manager::ExtDataControlManagerV1, _: manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<device::ExtDataControlDeviceV1, ()> for Server {
    fn event(s: &mut Self, _: &device::ExtDataControlDeviceV1, e: device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            device::Event::Selection { id } => s.wayland_selection(false, id),
            device::Event::PrimarySelection { id } => s.wayland_selection(true, id),
            _ => {}
        }
    }
    event_created_child!(Server, device::ExtDataControlDeviceV1, [device::EVT_DATA_OFFER_OPCODE => (offer::ExtDataControlOfferV1, Mimes::default())]);
}

impl Dispatch<offer::ExtDataControlOfferV1, Mimes> for Server {
    fn event(_: &mut Self, _: &offer::ExtDataControlOfferV1, e: offer::Event, m: &Mimes, _: &Connection, _: &QueueHandle<Self>) {
        if let offer::Event::Offer { mime_type } = e {
            m.0.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<source::ExtDataControlSourceV1, u32> for Server {
    fn event(s: &mut Self, src: &source::ExtDataControlSourceV1, e: source::Event, sel: &u32, _: &Connection, _: &QueueHandle<Self>) {
        match e {
            source::Event::Send { mime_type, fd } => s.wayland_paste(*sel, mime_type, fd),
            source::Event::Cancelled => {
                if let Some(c) = s.clip.as_mut() {
                    for slot in [&mut c.src_clipboard, &mut c.src_primary] {
                        if slot.as_ref() == Some(src) {
                            *slot = None;
                        }
                    }
                }
                src.destroy();
            }
            _ => {}
        }
    }
}
