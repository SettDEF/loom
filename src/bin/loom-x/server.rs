//! The X server: connections, windows, pixmaps, graphics contexts, atoms, properties, selections, input, and the core requests.

use std::{
    collections::HashMap,
    io::Write,
    os::unix::net::{UnixListener, UnixStream},
    time::Instant,
};

use smithay::reexports::calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction,
    generic::{FdWrapper, Generic},
};
use wayland_client::Proxy;
use x11rb_protocol::{
    protocol::{Request, xproto},
    x11_utils::RequestHeader,
};

use crate::{
    draw::{self, Gc, Target},
    keys::{self, Keymap},
    wl::{self, Top, Wl},
};

pub const ROOT: u32 = 0x100;
pub const COLORMAP: u32 = 0x20;
pub const VISUAL: u32 = 0x21;
/// the ARGB visual (depth 32)
pub const VISUAL32: u32 = 0x22;
const POINTER_ROOT: u32 = 1;

// event masks
const KEY_PRESS: u32 = 1;
const KEY_RELEASE: u32 = 2;
const BUTTON_PRESS: u32 = 4;
const BUTTON_RELEASE: u32 = 8;
const ENTER: u32 = 0x10;
const LEAVE: u32 = 0x20;
const POINTER_MOTION: u32 = 0x40;
const BUTTON1_MOTION: u32 = 0x100;
const BUTTON_MOTION: u32 = 0x2000;
const EXPOSURE: u32 = 0x8000;
const STRUCTURE: u32 = 0x20000;
const SUBSTRUCTURE: u32 = 0x80000;
const FOCUS_CHANGE: u32 = 0x200000;
const PROPERTY_CHANGE: u32 = 0x400000;

// errors
const BAD_REQUEST: u8 = 1;
const BAD_VALUE: u8 = 2;
const BAD_WINDOW: u8 = 3;
const BAD_ATOM: u8 = 5;
const BAD_MATCH: u8 = 8;
const BAD_DRAWABLE: u8 = 9;
const BAD_ALLOC: u8 = 11;
const BAD_GC: u8 = 13;
const BAD_NAME: u8 = 15;
const BAD_FONT: u8 = 7;
const BAD_LENGTH: u8 = 16;

pub struct Client {
    stream: UnixStream,
    inbuf: Vec<u8>,
    out: Vec<u8>,
    seq: u16,
    ready: bool,
    /// BIG-REQUESTS enabled: a zero length means a 32-bit one follows
    bigreq: bool,
    fds: Vec<std::os::fd::OwnedFd>,
    pub out_fds: Vec<std::os::fd::OwnedFd>,
}

pub struct Prop {
    pub type_: u32,
    pub format: u8,
    pub data: Vec<u8>,
}

pub struct Win {
    pub parent: u32,
    /// bottom to top
    pub children: Vec<u32>,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub border: u32,
    pub mapped: bool,
    pub override_redirect: bool,
    pub input_only: bool,
    pub depth: u8,
    pub bg: Option<u32>,
    /// a background pixmap, tiled from the window's corner (wins over `bg`)
    pub bg_pixmap: Option<u32>,
    pub border_pixel: u32,
    pub masks: HashMap<usize, u32>,
    pub dont_propagate: u32,
    pub props: HashMap<u32, Prop>,
    /// a top-level window's Wayland half, while it is mapped
    pub top: Option<Top>,
    /// its bounding shape (SHAPE), none = rectangular
    pub shape: Option<crate::ext::Shape>,
}

pub struct Pix {
    pub img: tiny_skia::Pixmap,
    pub depth: u8,
    /// a GPU buffer behind it (DRI3): read into `img` before it is used
    pub dmabuf: Option<crate::gpu::DmaBuf>,
}

/// Where a drawable's pixels are: a pixmap, or a window inside its top-level's picture.
enum Where {
    Pix(u32),
    Win {
        top: u32,
        ox: i32,
        oy: i32,
        clip: (i32, i32, i32, i32),
    },
}

pub struct Server {
    pub clients: HashMap<usize, Client>,
    next_client: usize,
    pub wins: HashMap<u32, Win>,
    pixmaps: HashMap<u32, Pix>,
    gcs: HashMap<u32, Gc>,
    atoms: Vec<Vec<u8>>,
    atom_ids: HashMap<Vec<u8>, u32>,
    pub selections: HashMap<u32, u32>,
    pub clip: Option<crate::clip::Clip>,
    /// XFIXES selection watchers: (client, window, selection, mask)
    pub fixes_sel: Vec<(usize, u32, u32, u32)>,
    /// MIT-SHM segments by id
    pub shm: HashMap<u32, crate::shm::Seg>,
    /// RENDER's pictures and glyph sets
    pub render: crate::render::Render,
    /// XI2 selections: (client, window, device, event mask)
    pub xi_sel: Vec<(usize, u32, u16, u32)>,
    /// DRI3, SYNC and Present
    pub gpu: crate::gpu::Gpu,
    pub glx: crate::glx::Glx,
    /// XKB: event selections and per-client flags (detectable auto-repeat) by client
    pub xkb_sel: HashMap<usize, u32>,
    pub xkb_flags: HashMap<usize, u32>,
    /// key repeat: (delay ms, keys a second) from loom, and the key repeating now
    pub repeat: (u32, u32),
    repeat_key: Option<(u32, smithay::reexports::calloop::RegistrationToken)>,
    /// sending a repeat: releases skip clients with detectable auto-repeat
    repeating: bool,
    /// the pointer at the last motion (raw deltas)
    raw_last: (f64, f64),
    pub wl: Wl,
    pub keymap: Keymap,
    fonts: crate::fonts::Fonts,
    mods: u16,
    buttons: u16,
    focus: u32,
    kb_top: Option<u32>,
    ptr_top: Option<u32>,
    ptr: (f64, f64),
    under: u32,
    grab: Option<u32>,
    size: (u16, u16),
    start: Instant,
    pub handle: LoopHandle<'static, Server>,
    /// the atoms loom-x itself needs
    a_net_wm_name: u32,
    a_wm_protocols: u32,
    a_wm_delete: u32,
}

const PREDEFINED: [&str; 68] = [
    "PRIMARY",
    "SECONDARY",
    "ARC",
    "ATOM",
    "BITMAP",
    "CARDINAL",
    "COLORMAP",
    "CURSOR",
    "CUT_BUFFER0",
    "CUT_BUFFER1",
    "CUT_BUFFER2",
    "CUT_BUFFER3",
    "CUT_BUFFER4",
    "CUT_BUFFER5",
    "CUT_BUFFER6",
    "CUT_BUFFER7",
    "DRAWABLE",
    "FONT",
    "INTEGER",
    "PIXMAP",
    "POINT",
    "RECTANGLE",
    "RESOURCE_MANAGER",
    "RGB_COLOR_MAP",
    "RGB_BEST_MAP",
    "RGB_BLUE_MAP",
    "RGB_DEFAULT_MAP",
    "RGB_GRAY_MAP",
    "RGB_GREEN_MAP",
    "RGB_RED_MAP",
    "STRING",
    "VISUALID",
    "WINDOW",
    "WM_COMMAND",
    "WM_HINTS",
    "WM_CLIENT_MACHINE",
    "WM_ICON_NAME",
    "WM_ICON_SIZE",
    "WM_NAME",
    "WM_NORMAL_HINTS",
    "WM_SIZE_HINTS",
    "WM_ZOOM_HINTS",
    "MIN_SPACE",
    "NORM_SPACE",
    "MAX_SPACE",
    "END_SPACE",
    "SUPERSCRIPT_X",
    "SUPERSCRIPT_Y",
    "SUBSCRIPT_X",
    "SUBSCRIPT_Y",
    "UNDERLINE_POSITION",
    "UNDERLINE_THICKNESS",
    "STRIKEOUT_ASCENT",
    "STRIKEOUT_DESCENT",
    "ITALIC_ANGLE",
    "X_HEIGHT",
    "QUAD_WIDTH",
    "WEIGHT",
    "POINT_SIZE",
    "RESOLUTION",
    "COPYRIGHT",
    "NOTICE",
    "FONT_NAME",
    "FAMILY_NAME",
    "FULL_NAME",
    "CAP_HEIGHT",
    "WM_CLASS",
    "WM_TRANSIENT_FOR",
];
const ATOM_WM_NAME: u32 = 39;
const ATOM_WM_CLASS: u32 = 67;
const ATOM_STRING: u32 = 31;
const ATOM_RESOURCE_MANAGER: u32 = 23;
const ATOM_FONT: u32 = 18;

/// A reply under construction: 32 bytes at least, its length filled in at the end.
pub struct Rep(Vec<u8>);
impl Rep {
    pub fn new(seq: u16, data: u8) -> Rep {
        let mut v = vec![1, data];
        v.extend_from_slice(&seq.to_le_bytes());
        v.extend_from_slice(&[0; 4]);
        Rep(v)
    }
    pub fn u8(mut self, v: u8) -> Rep {
        self.0.push(v);
        self
    }
    pub fn u16(mut self, v: u16) -> Rep {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i16(self, v: i16) -> Rep {
        self.u16(v as u16)
    }
    pub fn u32(mut self, v: u32) -> Rep {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn pad(mut self, n: usize) -> Rep {
        self.0.extend(std::iter::repeat_n(0, n));
        self
    }
    pub fn bytes(mut self, b: &[u8]) -> Rep {
        self.0.extend_from_slice(b);
        self
    }
    pub fn done(mut self) -> Vec<u8> {
        while self.0.len() < 32 || self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        let len = ((self.0.len() - 32) / 4) as u32;
        self.0[4..8].copy_from_slice(&len.to_le_bytes());
        self.0
    }
}

/// An event's 32 bytes, filled in field by field (the sequence number goes in per client).
struct Ev([u8; 32], usize);
impl Ev {
    fn new(code: u8, detail: u8) -> Ev {
        let mut b = [0u8; 32];
        b[0] = code;
        b[1] = detail;
        Ev(b, 4)
    }
    pub fn u8(mut self, v: u8) -> Ev {
        self.0[self.1] = v;
        self.1 += 1;
        self
    }
    pub fn u16(mut self, v: u16) -> Ev {
        self.0[self.1..self.1 + 2].copy_from_slice(&v.to_le_bytes());
        self.1 += 2;
        self
    }
    pub fn i16(self, v: i16) -> Ev {
        self.u16(v as u16)
    }
    pub fn u32(mut self, v: u32) -> Ev {
        self.0[self.1..self.1 + 4].copy_from_slice(&v.to_le_bytes());
        self.1 += 4;
        self
    }
}

fn pad4(n: usize) -> usize {
    (4 - n % 4) % 4
}

pub fn client_of(id: u32) -> usize {
    (id >> 21) as usize
}

/// The values a CreateGC or ChangeGC sets (two types with the same fields).
macro_rules! apply_gc {
    ($gc:expr, $a:expr) => {{
        let gc: &mut Gc = $gc;
        let a = $a;
    if let Some(v) = a.function {
        gc.function = u32::from(v) as u8;
    }
    if let Some(v) = a.foreground {
        gc.fg = v;
    }
    if let Some(v) = a.background {
        gc.bg = v;
    }
    if let Some(v) = a.line_width {
        gc.line_width = v;
    }
    if let Some(v) = a.cap_style {
        gc.cap = u32::from(v) as u8;
    }
    if let Some(v) = a.join_style {
        gc.join = u32::from(v) as u8;
    }
    if let Some(v) = a.fill_rule {
        gc.fill_rule_winding = u32::from(v) == 1;
    }
    if let Some(v) = a.arc_mode {
        gc.arc_pie = u32::from(v) == 1;
    }
    if let Some(v) = a.font {
        gc.font = v;
    }
    if let Some(v) = a.fill_style {
        gc.fill_style = u32::from(v) as u8;
    }
    if let Some(v) = a.tile {
        gc.tile = v;
    }
    if let Some(v) = a.stipple {
        gc.stipple = v;
    }
    if let Some(v) = a.tile_stipple_x_origin {
        gc.ts_origin.0 = v;
    }
    if let Some(v) = a.tile_stipple_y_origin {
        gc.ts_origin.1 = v;
    }
    if let Some(v) = a.graphics_exposures {
        gc.exposures = v != 0;
    }
    if let Some(v) = a.clip_x_origin {
        gc.clip_origin.0 = v;
    }
    if let Some(v) = a.clip_y_origin {
        gc.clip_origin.1 = v;
    }
    // a clip mask pixmap is drawn as no clip (shaped clips come later); None clears the rectangles
    if a.clip_mask.is_some() {
        gc.clip = None;
    }
    }};
}

impl Server {
    fn new(wl: Wl, handle: LoopHandle<'static, Server>, size: (u16, u16)) -> Server {
        let scale = wl.scale;
        let mut s = Server {
            clients: HashMap::new(),
            next_client: 1,
            wins: HashMap::new(),
            pixmaps: HashMap::new(),
            gcs: HashMap::new(),
            atoms: Vec::new(),
            atom_ids: HashMap::new(),
            selections: HashMap::new(),
            clip: None,
            fixes_sel: Vec::new(),
            shm: HashMap::new(),
            render: Default::default(),
            xi_sel: Vec::new(),
            gpu: Default::default(),
            glx: Default::default(),
            xkb_sel: HashMap::new(),
            xkb_flags: HashMap::new(),
            repeat: (600, 25),
            repeat_key: None,
            repeating: false,
            raw_last: (0.0, 0.0),
            wl,
            keymap: Keymap::us(),
            fonts: crate::fonts::Fonts::new(scale),
            mods: 0,
            buttons: 0,
            focus: POINTER_ROOT,
            kb_top: None,
            ptr_top: None,
            ptr: (0.0, 0.0),
            under: ROOT,
            grab: None,
            size,
            start: Instant::now(),
            handle,
            a_net_wm_name: 0,
            a_wm_protocols: 0,
            a_wm_delete: 0,
        };
        for n in PREDEFINED {
            s.intern(n.as_bytes());
        }
        s.a_net_wm_name = s.intern(b"_NET_WM_NAME");
        s.a_wm_protocols = s.intern(b"WM_PROTOCOLS");
        s.a_wm_delete = s.intern(b"WM_DELETE_WINDOW");
        s.wins.insert(
            ROOT,
            Win {
                parent: 0,
                children: Vec::new(),
                x: 0,
                y: 0,
                w: size.0 as u32,
                h: size.1 as u32,
                border: 0,
                mapped: true,
                override_redirect: false,
                input_only: false,
                depth: 24,
                bg: Some(0),
                bg_pixmap: None,
                border_pixel: 0,
                masks: HashMap::new(),
                dont_propagate: 0,
                props: HashMap::new(),
                top: None,
                shape: None,
            },
        );
        // loom-x's own window: owns X selections for Wayland's, receives X owners' answers
        s.wins.insert(crate::clip::SEL_WIN, Win { parent: ROOT, children: Vec::new(), x: -1, y: -1, w: 1, h: 1, border: 0, mapped: false, override_redirect: true, input_only: true, depth: 0, bg: None, bg_pixmap: None, border_pixel: 0, masks: HashMap::new(), dont_propagate: 0, props: HashMap::new(), top: None, shape: None });
        // the user's X settings (Xft.dpi, cursor size), as xrdb would put them
        if let Some(text) = std::env::var_os("HOME").and_then(|h| {
            std::fs::read_to_string(std::path::Path::new(&h).join(".Xresources")).ok()
        }) {
            let mut db: String = text
                .lines()
                .filter(|l| !l.trim_start().starts_with(['!', '#']))
                .collect::<Vec<_>>()
                .join("\n");
            db.push('\n');
            s.wins.get_mut(&ROOT).unwrap().props.insert(
                ATOM_RESOURCE_MANAGER,
                Prop {
                    type_: ATOM_STRING,
                    format: 8,
                    data: db.into_bytes(),
                },
            );
        }
        s
    }

    pub fn intern(&mut self, name: &[u8]) -> u32 {
        if let Some(&a) = self.atom_ids.get(name) {
            return a;
        }
        self.atoms.push(name.to_vec());
        let a = self.atoms.len() as u32;
        self.atom_ids.insert(name.to_vec(), a);
        a
    }

    pub fn now(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    // ---- connections ----

    fn new_client(&mut self, stream: UnixStream) {
        let idx = self.next_client;
        self.next_client += 1;
        let _ = stream.set_nonblocking(true);
        let Ok(read) = stream.try_clone() else { return };
        self.clients.insert(
            idx,
            Client {
                stream,
                inbuf: Vec::new(),
                out: Vec::new(),
                seq: 0,
                ready: false,
                bigreq: false,
                fds: Vec::new(),
                out_fds: Vec::new(),
            },
        );
        let _ = self.handle.insert_source(
            Generic::new(read, Interest::READ, Mode::Level),
            move |_, s, server: &mut Server| {
                let mut buf = [0u8; 65536];
                loop {
                    let mut got = Vec::new();
                    match recv(std::os::fd::AsRawFd::as_raw_fd(unsafe { s.get_mut() }), &mut buf, &mut got) {
                        Ok(0) => {
                            if std::env::var_os("LOOM_X_TRACE").is_some() { eprintln!("loom-x: client {idx} closed (line 481)"); }
server.close(idx);
                            return Ok(PostAction::Remove);
                        }
                        Ok(n) => {
                            if let Some(c) = server.clients.get_mut(&idx) {
                                c.inbuf.extend_from_slice(&buf[..n]);
                                // file descriptors passed along (MIT-SHM AttachFd)
                                c.fds.extend(got);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => {
                            if std::env::var_os("LOOM_X_TRACE").is_some() { eprintln!("loom-x: client {idx} closed (line 493)"); }
server.close(idx);
                            return Ok(PostAction::Remove);
                        }
                    }
                }
                server.process(idx);
                if server.clients.contains_key(&idx) {
                    Ok(PostAction::Continue)
                } else {
                    Ok(PostAction::Remove)
                }
            },
        );
    }

    /// A client gone: its windows, pixmaps and graphics contexts with it.
    fn close(&mut self, idx: usize) {
        if self.clients.remove(&idx).is_none() {
            return;
        }
        let mine: Vec<u32> = self
            .wins
            .keys()
            .copied()
            .filter(|&w| w != ROOT && client_of(w) == idx)
            .collect();
        for w in mine {
            if self.wins.contains_key(&w) {
                self.destroy(w);
            }
        }
        self.pixmaps.retain(|&p, _| client_of(p) != idx);
        self.gcs.retain(|&g, _| client_of(g) != idx);
        self.shm.retain(|&s, _| client_of(s) != idx);
        self.render.pics.retain(|&p, _| client_of(p) != idx);
        self.xi_sel.retain(|(c, ..)| *c != idx);
        self.xkb_sel.remove(&idx);
        self.xkb_flags.remove(&idx);
        self.gpu.fences.retain(|&f, _| client_of(f) != idx);
        self.gpu.selected.retain(|(c, ..)| *c != idx);
        for list in self.gpu.pending.values_mut() {
            list.retain(|p| p.client != idx);
        }
        self.render.sets.retain(|&s, _| client_of(s) != idx);
        for w in self.wins.values_mut() {
            w.masks.remove(&idx);
        }
        let gone: Vec<u32> = self.selections.iter().filter(|(_, o)| client_of(**o) == idx).map(|(s, _)| *s).collect();
        for sel in gone {
            self.selections.remove(&sel);
            self.clip_x_cleared(sel);
            self.fixes_selection(sel, 0, 2);
        }
    }

    fn send(&mut self, idx: usize, bytes: &[u8]) {
        if let Some(c) = self.clients.get_mut(&idx) {
            c.out.extend_from_slice(bytes);
        }
    }

    pub fn send_event(&mut self, idx: usize, mut ev: [u8; 32]) {
        if let Some(c) = self.clients.get_mut(&idx) {
            if ev[0] & 0x7f != 11 {
                ev[2..4].copy_from_slice(&c.seq.to_le_bytes());
            }
            c.out.extend_from_slice(&ev);
        }
    }

    pub fn error(&mut self, idx: usize, code: u8, bad: u32, major: u8, minor: u8) {
        if std::env::var_os("LOOM_X_TRACE").is_some() {
            eprintln!("loom-x: error {code} to client {idx} (request {major} minor {minor}, value {bad:#x})");
        }
        let seq = self.clients.get(&idx).map(|c| c.seq).unwrap_or(0);
        let mut e = [0u8; 32];
        e[1] = code;
        e[2..4].copy_from_slice(&seq.to_le_bytes());
        e[4..8].copy_from_slice(&bad.to_le_bytes());
        e[8..10].copy_from_slice(&(minor as u16).to_le_bytes());
        e[10] = major;
        self.send(idx, &e);
    }

    /// What arrived from a client: the connection setup first, then requests.
    fn process(&mut self, idx: usize) {
        loop {
            let Some(c) = self.clients.get_mut(&idx) else {
                return;
            };
            if !c.ready {
                if c.inbuf.len() < 12 {
                    return;
                }
                let n = u16::from_le_bytes([c.inbuf[6], c.inbuf[7]]) as usize;
                let d = u16::from_le_bytes([c.inbuf[8], c.inbuf[9]]) as usize;
                let total = 12 + n + pad4(n) + d + pad4(d);
                if c.inbuf.len() < total {
                    return;
                }
                if c.inbuf[0] != b'l' {
                    eprintln!("loom-x: a big-endian client (not supported)");
                    if std::env::var_os("LOOM_X_TRACE").is_some() { eprintln!("loom-x: client {idx} closed (line 582)"); }
self.close(idx);
                    return;
                }
                c.inbuf.drain(..total);
                c.ready = true;
                let setup = self.setup(idx);
                self.send(idx, &setup);
                continue;
            }
            if c.inbuf.len() < 4 {
                return;
            }
            let mut len = u16::from_le_bytes([c.inbuf[2], c.inbuf[3]]) as usize * 4;
            // the header's size: 4 bytes, or 8 with a BIG-REQUESTS length
            let mut head = 4;
            if len == 0 {
                if !c.bigreq {
                    if std::env::var_os("LOOM_X_TRACE").is_some() { eprintln!("loom-x: client {idx} closed (line 599)"); }
self.close(idx);
                    return;
                }
                if c.inbuf.len() < 8 {
                    return;
                }
                len = u32::from_le_bytes([c.inbuf[4], c.inbuf[5], c.inbuf[6], c.inbuf[7]]) as usize * 4;
                head = 8;
                if len < 8 {
                    if std::env::var_os("LOOM_X_TRACE").is_some() { eprintln!("loom-x: client {idx} closed (line 608)"); }
self.close(idx);
                    return;
                }
            }
            if c.inbuf.len() < len {
                return;
            }
            let req: Vec<u8> = c.inbuf.drain(..len).collect();
            c.seq = c.seq.wrapping_add(1);
            self.request(idx, &req, head);
        }
    }

    fn setup(&self, idx: usize) -> Vec<u8> {
        let vendor = b"loom";
        let mut b: Vec<u8> = Vec::new();
        let u16 = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
        let u32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        u32(&mut b, 1); // release
        u32(&mut b, (idx as u32) << 21);
        u32(&mut b, 0x1fffff);
        u32(&mut b, 256); // motion buffer
        u16(&mut b, vendor.len() as u16);
        u16(&mut b, 0xffff); // maximum request length
        // one screen, four pixmap formats
        b.extend_from_slice(&[1, 4, 0, 0, 32, 32, keys::MIN, keys::MAX, 0, 0, 0, 0]);
        b.extend_from_slice(vendor);
        b.extend(std::iter::repeat_n(0, pad4(vendor.len())));
        // pixmap formats: depth, bits per pixel, scanline pad
        for (d, bpp) in [(1u8, 1u8), (8, 8), (24, 32), (32, 32)] {
            b.extend_from_slice(&[d, bpp, 32, 0, 0, 0, 0, 0]);
        }
        // the screen
        u32(&mut b, ROOT);
        u32(&mut b, COLORMAP);
        u32(&mut b, 0xffffff);
        u32(&mut b, 0);
        u32(&mut b, 0);
        u16(&mut b, self.size.0);
        u16(&mut b, self.size.1);
        // millimetres at the screen's real density (96 dpi a logical pixel): toolkits size points by it
        let dpi = 96.0 * self.wl.scale;
        u16(&mut b, (self.size.0 as f64 * 25.4 / dpi).round() as u16);
        u16(&mut b, (self.size.1 as f64 * 25.4 / dpi).round() as u16);
        u16(&mut b, 1);
        u16(&mut b, 1);
        u32(&mut b, VISUAL);
        b.extend_from_slice(&[0, 0, 24, 4]); // backing stores, save unders, root depth, depths
        // depth 1 (bitmaps, no visual), depth 24 with the TrueColor visual
        b.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]);
        // depths 8 and 32: pixmaps only (RENDER's alpha masks and ARGB images)
        b.extend_from_slice(&[8, 0, 0, 0, 0, 0, 0, 0]);
        // depth 32 with the ARGB visual (transparent windows)
        b.extend_from_slice(&[32, 0, 1, 0, 0, 0, 0, 0]);
        u32(&mut b, VISUAL32);
        b.extend_from_slice(&[4, 8]);
        u16(&mut b, 256);
        u32(&mut b, 0xff0000);
        u32(&mut b, 0x00ff00);
        u32(&mut b, 0x0000ff);
        u32(&mut b, 0);
        b.extend_from_slice(&[24, 0, 1, 0, 0, 0, 0, 0]);
        u32(&mut b, VISUAL);
        b.extend_from_slice(&[4, 8]);
        u16(&mut b, 256);
        u32(&mut b, 0xff0000);
        u32(&mut b, 0x00ff00);
        u32(&mut b, 0x0000ff);
        u32(&mut b, 0);
        let mut out = vec![1, 0];
        out.extend_from_slice(&11u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&((b.len() / 4) as u16).to_le_bytes());
        out.extend_from_slice(&b);
        out
    }

    /// Everything waiting goes out: client replies and events, changed windows to loom.
    fn flush(&mut self) {
        let tops: Vec<u32> = self
            .wins
            .iter()
            .filter(|(_, w)| w.top.as_ref().is_some_and(|t| t.dirty))
            .map(|(&id, _)| id)
            .collect();
        // a window last shown zero-copy that got CPU drawing: the GPU picture copied in once first
        let external: Vec<(u32, u32)> = self.wins.iter().filter_map(|(&id, w)| w.top.as_ref().filter(|t| t.dirty).and_then(|t| t.external).map(|p| (id, p))).collect();
        for (id, p) in external {
            if let Some(t) = self.wins.get_mut(&id).and_then(|w| w.top.as_mut()) {
                t.external = None;
            }
            self.present(id, p, 0, 0);
        }
        for id in tops {
            if let Some(win) = self.wins.get_mut(&id) {
                if let Some(t) = win.top.as_mut() {
                    self.wl.present(t, win.shape.as_ref(), win.depth == 32);
                }
            }
        }
        let mut gone = Vec::new();
        for (&idx, c) in self.clients.iter_mut() {
            while !c.out.is_empty() {
                // file descriptors for a reply (MIT-SHM CreateSegment) go with the bytes that carry it
                let sent = if c.out_fds.is_empty() { c.stream.write(&c.out) } else { send(std::os::fd::AsRawFd::as_raw_fd(&c.stream), &c.out, std::mem::take(&mut c.out_fds)) };
                match sent {
                    Ok(n) => {
                        c.out.drain(..n);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        gone.push(idx);
                        break;
                    }
                }
            }
        }
        for idx in gone {
            if std::env::var_os("LOOM_X_TRACE").is_some() { eprintln!("loom-x: client {idx} closed (line 704)"); }
self.close(idx);
        }
    }

    fn output_waiting(&self) -> bool {
        self.clients.values().any(|c| !c.out.is_empty())
    }

    // ---- windows ----

    fn viewable(&self, mut w: u32) -> bool {
        while w != ROOT {
            match self.wins.get(&w) {
                Some(win) if win.mapped => w = win.parent,
                _ => return false,
            }
        }
        true
    }

    /// The top-level window `w` is in (ROOT itself has none).
    pub fn top_of(&self, mut w: u32) -> Option<u32> {
        loop {
            let win = self.wins.get(&w)?;
            if win.parent == ROOT {
                return Some(w);
            }
            if win.parent == 0 {
                return None;
            }
            w = win.parent;
        }
    }

    /// `w`'s origin inside its top-level window.
    fn in_top(&self, mut w: u32) -> (i32, i32) {
        let (mut x, mut y) = (0, 0);
        while let Some(win) = self.wins.get(&w) {
            if win.parent == ROOT || win.parent == 0 {
                break;
            }
            x += win.x + win.border as i32;
            y += win.y + win.border as i32;
            w = win.parent;
        }
        (x, y)
    }

    /// `w`'s origin on the X screen.
    fn on_root(&self, w: u32) -> (i32, i32) {
        if w == ROOT {
            return (0, 0);
        }
        let (x, y) = self.in_top(w);
        let t = self
            .top_of(w)
            .and_then(|t| self.wins.get(&t))
            .map(|t| (t.x, t.y))
            .unwrap_or((0, 0));
        (t.0 + x, t.1 + y)
    }

    fn locate(&self, d: u32) -> Option<Where> {
        if self.pixmaps.contains_key(&d) {
            return Some(Where::Pix(d));
        }
        let w = self.wins.get(&d)?;
        if d == ROOT || w.input_only || !self.viewable(d) {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (0, 0, w.w as i32, w.h as i32);
        let (mut ax, mut ay) = (0, 0);
        let mut cur = d;
        loop {
            let c = self.wins.get(&cur)?;
            if c.parent == ROOT {
                break;
            }
            ax += c.x + c.border as i32;
            ay += c.y + c.border as i32;
            let p = self.wins.get(&c.parent)?;
            x0 = x0.max(-ax);
            y0 = y0.max(-ay);
            x1 = x1.min(p.w as i32 - ax);
            y1 = y1.min(p.h as i32 - ay);
            cur = c.parent;
        }
        let t = self.wins.get(&cur)?.top.as_ref()?;
        // drawn before loom has configured it too (X exposes on map): shown once it is
        let _ = t;
        Some(Where::Win {
            top: cur,
            ox: ax,
            oy: ay,
            clip: (x0 + ax, y0 + ay, x1 + ax, y1 + ay),
        })
    }

    /// Draws on drawable `d` (nothing when it is not shown); true when it exists.
    pub fn draw(&mut self, d: u32, f: impl FnOnce(&mut Target)) -> bool {
        match self.locate(d) {
            Some(Where::Pix(id)) => {
                let p = self.pixmaps.get_mut(&id).unwrap();
                let (w, h) = (p.img.width() as i32, p.img.height() as i32);
                let depth = p.depth;
                f(&mut Target {
                    img: &mut p.img,
                    ox: 0,
                    oy: 0,
                    clip: (0, 0, w, h),
                    depth,
                });
                true
            }
            Some(Where::Win { top, ox, oy, clip }) => {
                // an ARGB window keeps its alpha
                let depth = if self.wins.get(&d).is_some_and(|w| w.depth == 32) { 32 } else { 24 };
                let t = self.wins.get_mut(&top).unwrap().top.as_mut().unwrap();
                f(&mut Target {
                    img: &mut t.img,
                    ox,
                    oy,
                    clip,
                    depth,
                });
                t.dirty = true;
                true
            }
            None => self.wins.contains_key(&d),
        }
    }

    /// Reads drawable `d` (a window not shown reads as black).
    pub fn read(&mut self, d: u32, f: impl FnOnce(&Target)) {
        self.sync_dmabuf(d);
        match self.locate(d) {
            Some(Where::Pix(id)) => {
                let p = self.pixmaps.get_mut(&id).unwrap();
                let (w, h) = (p.img.width() as i32, p.img.height() as i32);
                let depth = p.depth;
                f(&Target {
                    img: &mut p.img,
                    ox: 0,
                    oy: 0,
                    clip: (0, 0, w, h),
                    depth,
                });
            }
            Some(Where::Win { top, ox, oy, clip }) => {
                let depth = if self.wins.get(&d).is_some_and(|w| w.depth == 32) { 32 } else { 24 };
                let t = self.wins.get_mut(&top).unwrap().top.as_mut().unwrap();
                f(&Target {
                    img: &mut t.img,
                    ox,
                    oy,
                    clip,
                    depth,
                });
            }
            None => {}
        }
    }

    /// `w` and its mapped children painted with their backgrounds, then told to draw (Expose).
    fn expose_tree(&mut self, w: u32) {
        let Some(win) = self.wins.get(&w) else { return };
        if !win.mapped || win.input_only {
            return;
        }
        let (ww, wh, bg, children) = (win.w, win.h, win.bg, win.children.clone());
        // its border, drawn in its parent around it
        let (b, bp, parent, x, y) = (win.border, win.border_pixel, win.parent, win.x, win.y);
        if b > 0 && parent != ROOT {
            let ow = ww + 2 * b;
            self.draw(parent, |t| {
                t.clear((x, y, ow, b), bp);
                t.clear((x, y + (b + wh) as i32, ow, b), bp);
                t.clear((x, y + b as i32, b, wh), bp);
                t.clear((x + (b + ww) as i32, y + b as i32, b, wh), bp);
            });
        }
        let _ = bg;
        self.paint_bg(w, (0, 0, ww, wh));
        self.deliver(w, EXPOSURE, |_| {
            Ev::new(12, 0)
                .u32(w)
                .u16(0)
                .u16(0)
                .u16(ww as u16)
                .u16(wh as u16)
                .u16(0)
                .0
        });
        for c in children {
            self.expose_tree(c);
        }
    }

    /// An event to every client selecting `mask` on `w` (built per receiving window `ev`).
    fn deliver(&mut self, w: u32, mask: u32, make: impl Fn(u32) -> [u8; 32]) -> bool {
        let to: Vec<usize> = self
            .wins
            .get(&w)
            .map(|win| {
                win.masks
                    .iter()
                    .filter(|(_, m)| **m & mask != 0)
                    .map(|(c, _)| *c)
                    .collect()
            })
            .unwrap_or_default();
        // a repeat's release is not sent to clients that asked for detectable auto-repeat
        let skip = |c: &usize| self.repeating && mask == KEY_RELEASE && self.xkb_flags.get(c).is_some_and(|f| f & 1 != 0);
        let to: Vec<usize> = to.into_iter().filter(|c| !skip(c)).collect();
        for c in &to {
            self.send_event(*c, make(w));
        }
        !to.is_empty()
    }

    /// A structure event: to `w` (StructureNotify) and its parent (SubstructureNotify); `make` gets the receiving window.
    fn structure(&mut self, w: u32, make: impl Fn(u32) -> [u8; 32]) {
        self.deliver(w, STRUCTURE, &make);
        if let Some(p) = self.wins.get(&w).map(|x| x.parent) {
            self.deliver(p, SUBSTRUCTURE, &make);
        }
    }

    fn map(&mut self, w: u32) {
        let Some(win) = self.wins.get_mut(&w) else {
            return;
        };
        if win.mapped || w == ROOT {
            return;
        }
        win.mapped = true;
        let (parent, or, ww, wh) = (win.parent, win.override_redirect, win.w, win.h);
        self.structure(w, |ev| Ev::new(19, 0).u32(ev).u32(w).u8(or as u8).0);
        if parent == ROOT {
            // its own Wayland window (a menu or tooltip: a popup of the program's window); drawn and exposed when loom has configured it
            let (x, y) = (self.wins[&w].x, self.wins[&w].y);
            let parent_top = if or { self.popup_parent(w) } else { None };
            let t = match parent_top {
                Some(p) => {
                    let (px, py) = (self.wins[&p].x, self.wins[&p].y);
                    self.wl.popup(w, self.wins[&p].top.as_ref().unwrap(), x - px, y - py, ww, wh)
                }
                None => self.wl.top(w, ww, wh),
            };
            self.wins.get_mut(&w).unwrap().top = Some(t);
            self.title_changed(w);
            // exposed at once, as X does: no wait for loom's configure (it may resize it after)
            self.expose_tree(w);
        } else if self.viewable(w) {
            self.expose_tree(w);
        }
    }

    fn unmap(&mut self, w: u32) {
        let Some(win) = self.wins.get_mut(&w) else {
            return;
        };
        if !win.mapped || w == ROOT {
            return;
        }
        win.mapped = false;
        win.top = None;
        let parent = win.parent;
        if self
            .grab
            .is_some_and(|g| g == w || self.top_of(g) == Some(w))
        {
            self.grab = None;
        }
        self.structure(w, |ev| Ev::new(18, 0).u32(ev).u32(w).u8(0).0);
        // what it covered shows again
        if parent != ROOT && self.viewable(parent) {
            self.expose_tree(parent);
        }
    }

    fn destroy(&mut self, w: u32) {
        let Some(children) = self.wins.get(&w).map(|x| x.children.clone()) else {
            return;
        };
        for c in children {
            self.destroy(c);
        }
        if self.wins.get(&w).is_some_and(|x| x.mapped) {
            self.unmap(w);
        }
        self.structure(w, |ev| Ev::new(17, 0).u32(ev).u32(w).0);
        if let Some(win) = self.wins.remove(&w) {
            if let Some(p) = self.wins.get_mut(&win.parent) {
                p.children.retain(|&c| c != w);
            }
        }
        if self.focus == w {
            self.focus = POINTER_ROOT;
        }
        if self.under == w {
            self.under = ROOT;
        }
        if self.ptr_top == Some(w) {
            self.ptr_top = None;
        }
        if self.kb_top == Some(w) {
            self.kb_top = None;
        }
    }

    fn configure_notify(&mut self, w: u32) {
        let Some(win) = self.wins.get(&w) else { return };
        let (x, y, ww, wh, b, or) = (
            win.x,
            win.y,
            win.w,
            win.h,
            win.border,
            win.override_redirect,
        );
        self.structure(w, |ev| {
            Ev::new(22, 0)
                .u32(ev)
                .u32(w)
                .u32(0)
                .i16(x as i16)
                .i16(y as i16)
                .u16(ww as u16)
                .u16(wh as u16)
                .u16(b as u16)
                .u8(or as u8)
                .0
        });
    }

    /// A top-level window's new size (its own request, or loom tiling it): a new picture, everything drawn again.
    fn resize_top(&mut self, w: u32, ww: u32, wh: u32) {
        let Some(win) = self.wins.get_mut(&w) else {
            return;
        };
        win.w = ww;
        win.h = wh;
        let (wx, wy) = (win.x, win.y);
        self.present_configure(w, wx, wy, ww, wh);
        let Some(win) = self.wins.get_mut(&w) else { return };
        if let Some(t) = win.top.as_mut() {
            if (t.img.width(), t.img.height()) != (ww.max(1), wh.max(1)) {
                t.img = wl::new_img(ww, wh);
            }
            t.dirty = true;
        }
        self.configure_notify(w);
        // drawn again at the new size (shown once loom has configured it)
        if self.wins.get(&w).is_some_and(|x| x.top.is_some()) {
            self.expose_tree(w);
        }
    }

    /// loom configured a top-level window (the size it gives, 0 = the window's own).
    pub fn top_configured(&mut self, w: u32, size: (u32, u32)) {
        // loom gives logical pixels; X has the screen's own
        let k = self.wl.scale;
        let size = (
            (size.0 as f64 * k).round() as u32,
            (size.1 as f64 * k).round() as u32,
        );
        let Some(win) = self.wins.get_mut(&w) else {
            return;
        };
        let first = win.top.as_ref().is_some_and(|t| !t.configured);
        if let Some(t) = win.top.as_mut() {
            t.configured = true;
        }
        let (cw, ch) = (win.w, win.h);
        if size.0 > 0 && size.1 > 0 && size != (cw, ch) {
            self.resize_top(w, size.0, size.1);
        } else if first {
            // already drawn (exposed on map): shown now
            if let Some(t) = self.wins.get_mut(&w).and_then(|x| x.top.as_mut()) {
                t.dirty = true;
            }
        }
    }

    pub fn top_of_surface(
        &mut self,
        s: &wayland_client::protocol::wl_surface::WlSurface,
    ) -> Option<&mut Top> {
        let w = *s.data::<u32>()?;
        self.wins.get_mut(&w)?.top.as_mut()
    }

    /// Title and app id for loom from WM_NAME / _NET_WM_NAME and WM_CLASS.
    fn title_changed(&mut self, w: u32) {
        let Some(win) = self.wins.get(&w) else { return };
        let Some(t) = win.top.as_ref().and_then(|t| t.toplevel.as_ref()) else { return };
        let name = win
            .props
            .get(&self.a_net_wm_name)
            .or_else(|| win.props.get(&ATOM_WM_NAME));
        if let Some(p) = name {
            t.set_title(String::from_utf8_lossy(&p.data).into_owned());
        }
        if let Some(p) = win.props.get(&ATOM_WM_CLASS) {
            // "instance\0class\0": the class is the app id
            let class = p
                .data
                .split(|&b| b == 0)
                .nth(1)
                .filter(|c| !c.is_empty())
                .or_else(|| p.data.split(|&b| b == 0).next());
            if let Some(c) = class {
                t.set_app_id(String::from_utf8_lossy(c).into_owned());
            }
        }
    }

    /// loom's close button: WM_DELETE_WINDOW when the program speaks it, else its connection closes.
    pub fn close_request(&mut self, w: u32) {
        let speaks = self
            .wins
            .get(&w)
            .and_then(|x| x.props.get(&self.a_wm_protocols))
            .is_some_and(|p| {
                p.data
                    .chunks_exact(4)
                    .any(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) == self.a_wm_delete)
            });
        let owner = client_of(w);
        if speaks {
            let mut ev = Ev::new(33 | 0x80, 32)
                .u32(w)
                .u32(self.a_wm_protocols)
                .u32(self.a_wm_delete)
                .u32(self.now())
                .0;
            ev[0] = 33 | 0x80;
            self.send_event(owner, ev);
        } else {
            self.close(owner);
        }
    }

    /// The deepest mapped window under (x, y) in top-level `top` (its coordinates).
    fn deepest(&self, top: u32, x: i32, y: i32) -> u32 {
        let mut w = top;
        let (mut x, mut y) = (x, y);
        'down: loop {
            let Some(win) = self.wins.get(&w) else {
                return w;
            };
            for &c in win.children.iter().rev() {
                let Some(cw) = self.wins.get(&c) else {
                    continue;
                };
                let (cx, cy) = (cw.x + cw.border as i32, cw.y + cw.border as i32);
                if cw.mapped && x >= cx && y >= cy && x < cx + cw.w as i32 && y < cy + cw.h as i32 {
                    x -= cx;
                    y -= cy;
                    w = c;
                    continue 'down;
                }
            }
            return w;
        }
    }

    // ---- input ----

    pub fn state(&self) -> u16 {
        self.mods | self.buttons
    }

    /// An input event from `start` up to the first window selecting `mask`: returns that window.
    fn deliver_input(&mut self, start: u32, mask: u32, code: u8, detail: u8) -> Option<u32> {
        let (tx, ty) = (self.ptr.0 as i32, self.ptr.1 as i32);
        let (rx, ry) = self
            .ptr_top
            .and_then(|t| self.wins.get(&t))
            .map(|t| (t.x + tx, t.y + ty))
            .unwrap_or((tx, ty));
        let (time, state, under) = (self.now(), self.state(), self.under);
        // the XI2 copy and the raw event (code: 2 key press .. 6 motion, as XI2 numbers them)
        let xi = crate::xi::Input { evtype: code as u16, detail: detail as u32, root: (rx as f64, ry as f64), buttons: self.buttons, mods: self.mods };
        self.xi_input(start, under, &xi, |s, w| s.on_root(w));
        let delta = (code == 6).then(|| (rx as f64 - self.raw_last.0, ry as f64 - self.raw_last.1));
        if code == 6 {
            self.raw_last = (rx as f64, ry as f64);
        }
        self.xi_raw(code as u16, detail as u32, delta);
        let mut w = start;
        loop {
            let win = self.wins.get(&w)?;
            if win.masks.values().any(|m| m & mask != 0) {
                let (ox, oy) = self.on_root(w);
                // the child of `w` the pointer is in, if any
                let mut child = 0;
                let mut c = under;
                while c != w && c != ROOT && c != 0 {
                    let p = self.wins.get(&c).map(|x| x.parent).unwrap_or(0);
                    if p == w {
                        child = c;
                        break;
                    }
                    c = p;
                }
                self.deliver(w, mask, |ev| {
                    Ev::new(code, detail)
                        .u32(time)
                        .u32(ROOT)
                        .u32(ev)
                        .u32(child)
                        .i16(rx as i16)
                        .i16(ry as i16)
                        .i16((rx - ox) as i16)
                        .i16((ry - oy) as i16)
                        .u16(state)
                        .u8(1)
                        .0
                });
                return Some(w);
            }
            if win.dont_propagate & mask != 0 || win.parent == ROOT || win.parent == 0 {
                return None;
            }
            w = win.parent;
        }
    }

    fn crossing(&mut self, w: u32, code: u8, mask: u32) {
        if w == ROOT || !self.wins.contains_key(&w) {
            return;
        }
        let (tx, ty) = (self.ptr.0 as i32, self.ptr.1 as i32);
        let (rx, ry) = self
            .ptr_top
            .and_then(|t| self.wins.get(&t))
            .map(|t| (t.x + tx, t.y + ty))
            .unwrap_or((tx, ty));
        let (ox, oy) = self.on_root(w);
        let (time, state) = (self.now(), self.state());
        let (b, m) = (self.buttons, self.mods);
        self.xi_crossing(w, if code == 7 { crate::xi::ENTER } else { crate::xi::LEAVE }, (rx as f64, ry as f64), (ox, oy), b, m);
        self.deliver(w, mask, |ev| {
            Ev::new(code, 3)
                .u32(time)
                .u32(ROOT)
                .u32(ev)
                .u32(0)
                .i16(rx as i16)
                .i16(ry as i16)
                .i16((rx - ox) as i16)
                .i16((ry - oy) as i16)
                .u16(state)
                .u8(0)
                .u8(3)
                .0
        });
    }

    pub fn pointer_enter(&mut self, top: Option<u32>, x: f64, y: f64) {
        if top.is_none() {
            let old = self.under;
            self.crossing(old, 8, LEAVE);
            self.under = ROOT;
            self.ptr_top = None;
            return;
        }
        self.ptr_top = top;
        self.pointer_motion(x, y);
    }

    pub fn pointer_motion(&mut self, x: f64, y: f64) {
        self.ptr = (x, y);
        let Some(top) = self.ptr_top else { return };
        let d = self.deepest(top, x as i32, y as i32);
        if d != self.under {
            let old = self.under;
            self.crossing(old, 8, LEAVE);
            self.under = d;
            self.crossing(d, 7, ENTER);
        }
        let mut mask = POINTER_MOTION;
        if self.buttons != 0 {
            mask |= BUTTON_MOTION;
            for b in 0..5 {
                if self.buttons & (0x100 << b) != 0 {
                    mask |= BUTTON1_MOTION << b;
                }
            }
        }
        let start = self.grab.unwrap_or(d);
        self.deliver_input(start, mask, 6, 0);
    }

    pub fn pointer_button(&mut self, b: u8, pressed: bool) {
        if self.ptr_top.is_none() {
            return;
        }
        let bit = if b <= 5 { 0x80u16 << b } else { 0 };
        if pressed {
            let start = self.grab.unwrap_or(self.under);
            let got = self.deliver_input(start, BUTTON_PRESS, 4, b);
            if self.grab.is_none() {
                self.grab = got;
            }
            self.buttons |= bit;
        } else {
            let start = self.grab.unwrap_or(self.under);
            self.deliver_input(start, BUTTON_RELEASE, 5, b);
            self.buttons &= !bit;
            if self.buttons == 0 {
                self.grab = None;
            }
        }
    }

    pub fn keymap_fd(&mut self, fd: std::os::fd::OwnedFd, size: usize) {
        if let Some(k) = Keymap::from_fd(fd, size) {
            self.keymap = k;
            // every client reads the keyboard again
            let ev = Ev::new(34, 0)
                .u8(1)
                .u8(keys::MIN)
                .u8((keys::MAX - keys::MIN) as u8 + 1)
                .0;
            let all: Vec<usize> = self.clients.keys().copied().collect();
            for c in all {
                self.send_event(c, ev);
            }
        }
    }

    pub fn keyboard_enter(&mut self, top: Option<u32>, _pressed: &[u32]) {
        if let Some((_, t)) = self.repeat_key.take() {
            self.handle.remove(t);
        }
        let (b, m) = (self.buttons, self.mods);
        if let Some(old) = self.kb_top.take() {
            self.deliver(old, FOCUS_CHANGE, |ev| Ev::new(10, 3).u32(ev).u8(0).0);
            self.xi_crossing(old, crate::xi::FOCUS_OUT, (0.0, 0.0), (0, 0), b, m);
        }
        self.kb_top = top;
        if let Some(t) = top {
            self.deliver(t, FOCUS_CHANGE, |ev| Ev::new(9, 3).u32(ev).u8(0).0);
            self.xi_crossing(t, crate::xi::FOCUS_IN, (0.0, 0.0), (0, 0), b, m);
        }
    }

    pub fn modifiers(&mut self, mods: u32) {
        let before = self.mods;
        self.mods = keys::x_state(mods);
        if self.mods != before {
            self.xkb_state_changed();
        }
    }

    fn key_event(&mut self, code: u32, pressed: bool) {
        let Some(top) = self.kb_top else { return };
        let under_here = self.ptr_top == Some(top);
        // X's focus rules: PointerRoot follows the pointer; a focus window takes the keys unless the pointer is inside it
        let start = match self.focus {
            0 => return,
            POINTER_ROOT => {
                if under_here {
                    self.under
                } else {
                    top
                }
            }
            f => {
                let mut c = self.under;
                let mut inside = false;
                while c != ROOT && c != 0 {
                    if c == f {
                        inside = true;
                        break;
                    }
                    c = self.wins.get(&c).map(|x| x.parent).unwrap_or(0);
                }
                if inside && under_here { self.under } else { f }
            }
        };
        let (mask, ev) = if pressed {
            (KEY_PRESS, 2)
        } else {
            (KEY_RELEASE, 3)
        };
        self.deliver_input(start, mask, ev, code as u8);
    }

    // ---- requests ----

    fn request(&mut self, idx: usize, raw: &[u8], head: usize) {
        let header = RequestHeader { major_opcode: raw[0], minor_opcode: raw[1], remaining_length: ((raw.len() - head) / 4) as u32 };
        let major = raw[0];
        if std::env::var_os("LOOM_X_TRACE").is_some() {
            eprintln!("loom-x: client {idx} request {} minor {} ({} bytes)", raw[0], raw[1], raw.len());
        }
        let seq = self.clients.get(&idx).map(|c| c.seq).unwrap_or(0);
        let mut fds = self.clients.get_mut(&idx).map(|c| std::mem::take(&mut c.fds)).unwrap_or_default();
        let parsed = Request::parse(header, &raw[head..], &mut fds, &crate::ext::Table);
        // the ones this request did not take stay for the next
        if let Some(c) = self.clients.get_mut(&idx) {
            c.fds = fds;
        }
        let req = match parsed {
            Ok(r) => r,
            Err(_) => {
                self.error(
                    idx,
                    if major >= 128 {
                        BAD_REQUEST
                    } else {
                        BAD_LENGTH
                    },
                    0,
                    major,
                    raw[1],
                );
                return;
            }
        };
        self.handle(idx, seq, major, req);
    }

    pub fn reply(&mut self, idx: usize, r: Rep) {
        let b = r.done();
        if std::env::var_os("LOOM_X_TRACE").is_some() {
            eprintln!("loom-x: reply {} bytes (length {})", b.len(), (b.len() - 32) / 4);
        }
        self.send(idx, &b);
    }

    fn handle(&mut self, idx: usize, seq: u16, major: u8, req: Request) {
        match req {
            Request::CreateWindow(r) => {
                let Some(parent) = self.wins.get(&r.parent) else {
                    return self.error(idx, BAD_WINDOW, r.parent, major, 0);
                };
                if self.wins.contains_key(&r.wid) || client_of(r.wid) != idx {
                    return self.error(idx, 14, r.wid, major, 0);
                }
                let a = &r.value_list;
                let input_only = u16::from(r.class) == 2;
                let depth = if r.depth == 0 { parent.depth } else { r.depth };
                let mut masks = HashMap::new();
                if let Some(m) = a.event_mask {
                    masks.insert(idx, u32::from(m));
                }
                let win = Win {
                    parent: r.parent,
                    children: Vec::new(),
                    x: r.x as i32,
                    y: r.y as i32,
                    w: r.width.max(1) as u32,
                    h: r.height.max(1) as u32,
                    border: r.border_width as u32,
                    mapped: false,
                    override_redirect: a.override_redirect.is_some_and(|v| v != 0),
                    input_only,
                    depth,
                    // background None (no pixmap given) keeps what was there: drawn as the parent's
                    bg: a.background_pixel.or(if a.background_pixmap.is_some() {
                        Some(0xffffff)
                    } else {
                        None
                    }),
                    bg_pixmap: a.background_pixmap.filter(|&p| p > 1),
                    border_pixel: a.border_pixel.unwrap_or(0),
                    masks,
                    dont_propagate: a.do_not_propogate_mask.map(u32::from).unwrap_or(0),
                    props: HashMap::new(),
                    top: None,
                    shape: None,
                };
                self.wins.insert(r.wid, win);
                if let Some(p) = self.wins.get_mut(&r.parent) {
                    p.children.push(r.wid);
                }
                let (x, y, w, h, b, or) = (
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    r.border_width,
                    a.override_redirect.is_some_and(|v| v != 0),
                );
                let wid = r.wid;
                self.deliver(r.parent, SUBSTRUCTURE, |ev| {
                    Ev::new(16, 0)
                        .u32(ev)
                        .u32(wid)
                        .i16(x)
                        .i16(y)
                        .u16(w)
                        .u16(h)
                        .u16(b)
                        .u8(or as u8)
                        .0
                });
            }
            Request::ChangeWindowAttributes(r) => {
                let Some(win) = self.wins.get_mut(&r.window) else {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                };
                let a = &r.value_list;
                if let Some(p) = a.background_pixmap {
                    win.bg_pixmap = (p > 1).then_some(p);
                }
                if let Some(p) = a.background_pixel {
                    win.bg_pixmap = None;
                    win.bg = Some(p);
                }
                if let Some(p) = a.border_pixel {
                    win.border_pixel = p;
                }
                if let Some(m) = a.event_mask {
                    win.masks.insert(idx, u32::from(m));
                }
                if let Some(m) = a.do_not_propogate_mask {
                    win.dont_propagate = u32::from(m);
                }
                if let Some(o) = a.override_redirect {
                    win.override_redirect = o != 0;
                }
            }
            Request::GetWindowAttributes(r) => {
                let viewable = self.viewable(r.window);
                let Some(win) = self.wins.get(&r.window) else {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                };
                let all = win.masks.values().fold(0, |a, m| a | m);
                let mine = win.masks.get(&idx).copied().unwrap_or(0);
                let map_state = if !win.mapped {
                    0
                } else if viewable {
                    2
                } else {
                    1
                };
                let rep = Rep::new(seq, 0)
                    .u32(if win.depth == 32 { VISUAL32 } else { VISUAL })
                    .u16(if win.input_only { 2 } else { 1 })
                    .u8(0)
                    .u8(1)
                    .u32(0)
                    .u32(0)
                    .u8(0)
                    .u8(1)
                    .u8(map_state)
                    .u8(win.override_redirect as u8)
                    .u32(COLORMAP)
                    .u32(all)
                    .u32(mine)
                    .u16(win.dont_propagate as u16)
                    .pad(2);
                self.reply(idx, rep);
            }
            Request::DestroyWindow(r) => {
                if r.window != ROOT && self.wins.contains_key(&r.window) {
                    self.destroy(r.window);
                }
            }
            Request::DestroySubwindows(r) => {
                for c in self
                    .wins
                    .get(&r.window)
                    .map(|w| w.children.clone())
                    .unwrap_or_default()
                {
                    self.destroy(c);
                }
            }
            Request::ChangeSaveSet(_) => {}
            Request::ReparentWindow(r) => {
                if !self.wins.contains_key(&r.window) || !self.wins.contains_key(&r.parent) {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                }
                let mapped = self.wins[&r.window].mapped;
                if mapped {
                    self.unmap(r.window);
                }
                let old = self.wins[&r.window].parent;
                if let Some(p) = self.wins.get_mut(&old) {
                    p.children.retain(|&c| c != r.window);
                }
                self.wins
                    .get_mut(&r.parent)
                    .unwrap()
                    .children
                    .push(r.window);
                let w = self.wins.get_mut(&r.window).unwrap();
                w.parent = r.parent;
                w.x = r.x as i32;
                w.y = r.y as i32;
                let (win, parent, x, y, or) = (r.window, r.parent, r.x, r.y, w.override_redirect);
                self.structure(win, |ev| {
                    Ev::new(21, 0)
                        .u32(ev)
                        .u32(win)
                        .u32(parent)
                        .i16(x)
                        .i16(y)
                        .u8(or as u8)
                        .0
                });
                if mapped {
                    self.map(r.window);
                }
            }
            Request::MapWindow(r) => {
                if !self.wins.contains_key(&r.window) {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                }
                self.map(r.window);
            }
            Request::MapSubwindows(r) => {
                for c in self
                    .wins
                    .get(&r.window)
                    .map(|w| w.children.clone())
                    .unwrap_or_default()
                {
                    self.map(c);
                }
            }
            Request::UnmapWindow(r) => self.unmap(r.window),
            Request::UnmapSubwindows(r) => {
                for c in self
                    .wins
                    .get(&r.window)
                    .map(|w| w.children.clone())
                    .unwrap_or_default()
                {
                    self.unmap(c);
                }
            }
            Request::ConfigureWindow(r) => {
                let Some(win) = self.wins.get_mut(&r.window) else {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                };
                let a = &r.value_list;
                let (ow, oh) = (win.w, win.h);
                if let Some(v) = a.x {
                    win.x = v;
                }
                if let Some(v) = a.y {
                    win.y = v;
                }
                if let Some(v) = a.border_width {
                    win.border = v;
                }
                let (nw, nh) = (a.width.unwrap_or(ow).max(1), a.height.unwrap_or(oh).max(1));
                let parent = win.parent;
                let is_top = win.top.is_some();
                if let Some(mode) = a.stack_mode {
                    if let Some(p) = self.wins.get_mut(&parent) {
                        p.children.retain(|&c| c != r.window);
                        // Above (0): to the top; Below (1): to the bottom
                        if u32::from(mode) == 1 {
                            p.children.insert(0, r.window);
                        } else {
                            p.children.push(r.window);
                        }
                    }
                }
                if is_top {
                    // loom decides where it goes; its size is the program's to ask for
                    self.resize_top(r.window, nw, nh);
                } else {
                    let w = self.wins.get_mut(&r.window).unwrap();
                    w.w = nw;
                    w.h = nh;
                    self.configure_notify(r.window);
                    if self.viewable(parent) {
                        self.expose_tree(parent);
                    }
                }
            }
            Request::CirculateWindow(_) => {}
            Request::GetGeometry(r) => {
                if let Some(p) = self.pixmaps.get(&r.drawable) {
                    let rep = Rep::new(seq, p.depth)
                        .u32(ROOT)
                        .i16(0)
                        .i16(0)
                        .u16(p.img.width() as u16)
                        .u16(p.img.height() as u16)
                        .u16(0);
                    return self.reply(idx, rep);
                }
                let Some(w) = self.wins.get(&r.drawable) else {
                    return self.error(idx, BAD_DRAWABLE, r.drawable, major, 0);
                };
                let rep = Rep::new(seq, w.depth)
                    .u32(ROOT)
                    .i16(w.x as i16)
                    .i16(w.y as i16)
                    .u16(w.w as u16)
                    .u16(w.h as u16)
                    .u16(w.border as u16);
                self.reply(idx, rep);
            }
            Request::QueryTree(r) => {
                let Some(w) = self.wins.get(&r.window) else {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                };
                let mut rep = Rep::new(seq, 0)
                    .u32(ROOT)
                    .u32(w.parent)
                    .u16(w.children.len() as u16)
                    .pad(14);
                for c in w.children.clone() {
                    rep = rep.u32(c);
                }
                self.reply(idx, rep);
            }
            Request::InternAtom(r) => {
                let a = if r.only_if_exists {
                    self.atom_ids.get(r.name.as_ref()).copied().unwrap_or(0)
                } else {
                    self.intern(&r.name)
                };
                self.reply(idx, Rep::new(seq, 0).u32(a));
            }
            Request::GetAtomName(r) => {
                let Some(name) = self.atoms.get((r.atom as usize).wrapping_sub(1)).cloned() else {
                    return self.error(idx, BAD_ATOM, r.atom, major, 0);
                };
                self.reply(
                    idx,
                    Rep::new(seq, 0).u16(name.len() as u16).pad(22).bytes(&name),
                );
            }
            Request::ChangeProperty(r) => {
                if r.format != 8 && r.format != 16 && r.format != 32 {
                    return self.error(idx, BAD_VALUE, r.format as u32, major, 0);
                }
                let Some(win) = self.wins.get_mut(&r.window) else {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                };
                let n = r.data_len as usize * r.format as usize / 8;
                let data = r.data[..n.min(r.data.len())].to_vec();
                // Replace (0), Prepend (1), Append (2): the last two only onto the same type and format
                let mode = u32::from(r.mode);
                let mismatch = mode != 0
                    && win
                        .props
                        .get(&r.property)
                        .is_some_and(|p| p.type_ != r.type_ || p.format != r.format);
                if mismatch {
                    return self.error(idx, BAD_MATCH, r.property, major, 0);
                }
                match win.props.get_mut(&r.property) {
                    Some(p) if mode == 1 => {
                        p.data.splice(0..0, data);
                    }
                    Some(p) if mode == 2 => p.data.extend_from_slice(&data),
                    _ => {
                        win.props.insert(
                            r.property,
                            Prop {
                                type_: r.type_,
                                format: r.format,
                                data,
                            },
                        );
                    }
                }
                self.property_notify(r.window, r.property, 0);
                if r.property == ATOM_WM_NAME
                    || r.property == self.a_net_wm_name
                    || r.property == ATOM_WM_CLASS
                {
                    self.title_changed(r.window);
                }
            }
            Request::DeleteProperty(r) => {
                if self
                    .wins
                    .get_mut(&r.window)
                    .and_then(|w| w.props.remove(&r.property))
                    .is_some()
                {
                    self.property_notify(r.window, r.property, 1);
                }
            }
            Request::GetProperty(r) => {
                let Some(win) = self.wins.get(&r.window) else {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                };
                let Some(p) = win.props.get(&r.property) else {
                    return self.reply(idx, Rep::new(seq, 0).u32(0).u32(0).u32(0).pad(12));
                };
                if r.type_ != 0 && r.type_ != p.type_ {
                    let rep = Rep::new(seq, p.format)
                        .u32(p.type_)
                        .u32(p.data.len() as u32)
                        .u32(0)
                        .pad(12);
                    return self.reply(idx, rep);
                }
                let off = 4 * r.long_offset as usize;
                if off > p.data.len() {
                    return self.error(idx, BAD_VALUE, r.long_offset, major, 0);
                }
                let end = p.data.len().min(off + 4 * r.long_length as usize);
                let value = p.data[off..end].to_vec();
                let after = (p.data.len() - end) as u32;
                let unit = (p.format / 8).max(1) as usize;
                let rep = Rep::new(seq, p.format)
                    .u32(p.type_)
                    .u32(after)
                    .u32((value.len() / unit) as u32)
                    .pad(12)
                    .bytes(&value);
                self.reply(idx, rep);
                if r.delete && after == 0 {
                    self.wins
                        .get_mut(&r.window)
                        .unwrap()
                        .props
                        .remove(&r.property);
                    self.property_notify(r.window, r.property, 1);
                }
            }
            Request::ListProperties(r) => {
                let Some(win) = self.wins.get(&r.window) else {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                };
                let atoms: Vec<u32> = win.props.keys().copied().collect();
                let mut rep = Rep::new(seq, 0).u16(atoms.len() as u16).pad(22);
                for a in atoms {
                    rep = rep.u32(a);
                }
                self.reply(idx, rep);
            }
            Request::SetSelectionOwner(r) => {
                if r.owner == 0 {
                    let had = self.selections.remove(&r.selection);
                    if let Some(o) = had {
                        let (time, sel) = (self.now(), r.selection);
                        self.send_to(o, Ev::new(29, 0).u32(time).u32(o).u32(sel).0);
                    }
                    self.clip_x_cleared(r.selection);
                    self.fixes_selection(r.selection, 0, 0);
                } else {
                    self.take_selection(r.selection, r.owner);
                    self.clip_x_owned(r.selection);
                }
            }
            Request::GetSelectionOwner(r) => {
                let o = self.selections.get(&r.selection).copied().unwrap_or(0);
                self.reply(idx, Rep::new(seq, 0).u32(o));
            }
            Request::ConvertSelection(r) => {
                let time = if r.time == 0 { self.now() } else { r.time };
                match self.selections.get(&r.selection).copied() {
                    // Wayland's selection, held for it by loom-x
                    Some(crate::clip::SEL_WIN) => self.clip_convert(r.requestor, r.selection, r.target, r.property, time),
                    Some(owner) => self.send_to(owner, selection_request(time, owner, r.requestor, r.selection, r.target, r.property)),
                    None => self.selection_notify(r.requestor, r.selection, r.target, 0, time),
                }
            }
            Request::SendEvent(r) => {
                let dest = match r.destination {
                    0 => self.under,
                    1 => {
                        if self.focus == POINTER_ROOT {
                            self.under
                        } else {
                            self.focus
                        }
                    }
                    d => d,
                };
                if !self.wins.contains_key(&dest) {
                    return self.error(idx, BAD_WINDOW, r.destination, major, 0);
                }
                let mut ev = *r.event;
                // an X owner's answer to a Wayland program's paste
                if dest == crate::clip::SEL_WIN {
                    if ev[0] & 0x7f == 31 {
                        self.clip_x_answer(&ev);
                    }
                    return;
                }
                ev[0] |= 0x80;
                let mask = u32::from(r.event_mask);
                if mask == 0 {
                    self.send_event(client_of(dest), ev);
                } else {
                    self.deliver(dest, mask, |_| ev);
                }
            }
            Request::GrabPointer(_) | Request::GrabKeyboard(_) => self.reply(idx, Rep::new(seq, 0)),
            Request::UngrabPointer(_)
            | Request::UngrabKeyboard(_)
            | Request::GrabButton(_)
            | Request::UngrabButton(_)
            | Request::ChangeActivePointerGrab(_)
            | Request::GrabKey(_)
            | Request::UngrabKey(_)
            | Request::AllowEvents(_)
            | Request::GrabServer(_)
            | Request::UngrabServer(_) => {}
            Request::QueryPointer(r) => {
                let (tx, ty) = (self.ptr.0 as i32, self.ptr.1 as i32);
                let (rx, ry) = self
                    .ptr_top
                    .and_then(|t| self.wins.get(&t))
                    .map(|t| (t.x + tx, t.y + ty))
                    .unwrap_or((tx, ty));
                let (ox, oy) = self.on_root(r.window);
                let rep = Rep::new(seq, 1)
                    .u32(ROOT)
                    .u32(0)
                    .i16(rx as i16)
                    .i16(ry as i16)
                    .i16((rx - ox) as i16)
                    .i16((ry - oy) as i16)
                    .u16(self.state())
                    .pad(6);
                self.reply(idx, rep);
            }
            Request::GetMotionEvents(_) => self.reply(idx, Rep::new(seq, 0).u32(0).pad(20)),
            Request::TranslateCoordinates(r) => {
                if !self.wins.contains_key(&r.src_window) || !self.wins.contains_key(&r.dst_window)
                {
                    return self.error(idx, BAD_WINDOW, r.src_window, major, 0);
                }
                let (sx, sy) = self.on_root(r.src_window);
                let (dx, dy) = self.on_root(r.dst_window);
                let (x, y) = (sx + r.src_x as i32 - dx, sy + r.src_y as i32 - dy);
                let child = self.wins[&r.dst_window]
                    .children
                    .iter()
                    .rev()
                    .copied()
                    .find(|c| {
                        self.wins.get(c).is_some_and(|w| {
                            w.mapped
                                && x >= w.x
                                && y >= w.y
                                && x < w.x + w.w as i32
                                && y < w.y + w.h as i32
                        })
                    })
                    .unwrap_or(0);
                self.reply(idx, Rep::new(seq, 1).u32(child).i16(x as i16).i16(y as i16));
            }
            Request::WarpPointer(_) => {}
            Request::SetInputFocus(r) => self.focus = r.focus,
            Request::GetInputFocus(_) => {
                let f = self.focus;
                self.reply(idx, Rep::new(seq, 1).u32(f));
            }
            Request::QueryKeymap(_) => self.reply(idx, Rep::new(seq, 0).pad(32)),
            Request::OpenFont(r) => {
                let name = String::from_utf8_lossy(&r.name).into_owned();
                match self.fonts.load(&name) {
                    Some(f) => {
                        self.fonts.open.insert(r.fid, f);
                    }
                    None => self.error(idx, BAD_NAME, r.fid, major, 0),
                }
            }
            Request::CloseFont(r) => {
                self.fonts.open.remove(&r.font);
            }
            Request::QueryFont(r) => {
                let Some(f) = self.font_of(r.font) else {
                    return self.error(idx, BAD_FONT, r.font, major, 0);
                };
                let rep = self.font_info(Rep::new(seq, 0), &f, None);
                self.reply(idx, rep);
            }
            Request::QueryTextExtents(r) => {
                let Some(f) = self.font_of(r.font) else {
                    return self.error(idx, BAD_FONT, r.font, major, 0);
                };
                let text: Vec<u16> = r
                    .string
                    .iter()
                    .map(|c| (c.byte1 as u16) << 8 | c.byte2 as u16)
                    .collect();
                let (asc, desc) = text
                    .iter()
                    .filter_map(|&c| f.chars.get(c as usize))
                    .fold((0i16, 0i16), |(a, d), ci| {
                        (a.max(ci.ascent), d.max(ci.descent))
                    });
                let w = f.width(&text);
                let left = text
                    .first()
                    .and_then(|&c| f.chars.get(c as usize))
                    .map(|c| c.lsb as i32)
                    .unwrap_or(0);
                let right = (w - f.advance as i32)
                    + text
                        .last()
                        .and_then(|&c| f.chars.get(c as usize))
                        .map(|c| c.rsb as i32)
                        .unwrap_or(0);
                self.reply(
                    idx,
                    Rep::new(seq, 0)
                        .i16(f.ascent)
                        .i16(f.descent)
                        .i16(asc)
                        .i16(desc)
                        .u32(w as u32)
                        .u32(left as u32)
                        .u32(right as u32),
                );
            }
            Request::ListFonts(r) => {
                let names = self.font_names(&r.pattern, r.max_names);
                let mut rep = Rep::new(seq, 0).u16(names.len() as u16).pad(22);
                for n in names {
                    rep = rep.u8(n.len() as u8).bytes(n.as_bytes());
                }
                self.reply(idx, rep);
            }
            Request::ListFontsWithInfo(r) => {
                for n in self.font_names(&r.pattern, r.max_names) {
                    if let Some(f) = self.fonts.load(&n) {
                        let rep = self.font_info(Rep::new(seq, n.len() as u8), &f, Some(&n));
                        self.reply(idx, rep);
                    }
                }
                // the last reply: no name
                self.reply(idx, Rep::new(seq, 0).pad(52));
            }
            Request::GetFontPath(_) => self.reply(idx, Rep::new(seq, 0).u16(0).pad(22)),
            Request::SetFontPath(_) => {}
            Request::CreatePixmap(r) => {
                if r.width == 0 || r.height == 0 {
                    return self.error(idx, BAD_VALUE, 0, major, 0);
                }
                let Some(img) = tiny_skia::Pixmap::new(r.width as u32, r.height as u32) else {
                    return self.error(idx, BAD_ALLOC, r.pid, major, 0);
                };
                self.pixmaps.insert(
                    r.pid,
                    Pix {
                        img,
                        depth: r.depth,
                        dmabuf: None,
                    },
                );
            }
            Request::FreePixmap(r) => {
                self.pixmaps.remove(&r.pixmap);
            }
            Request::CreateGC(r) => {
                let mut gc = Gc::default();
                apply_gc!(&mut gc, &*r.value_list);
                self.gcs.insert(r.cid, gc);
            }
            Request::ChangeGC(r) => {
                let Some(gc) = self.gcs.get_mut(&r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                apply_gc!(gc, &*r.value_list);
            }
            Request::CopyGC(r) => {
                if let Some(src) = self.gcs.get(&r.src_gc).cloned() {
                    self.gcs.insert(r.dst_gc, src);
                }
            }
            Request::SetDashes(_) => {}
            Request::SetClipRectangles(r) => {
                let Some(gc) = self.gcs.get_mut(&r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                gc.clip = Some(
                    r.rectangles
                        .iter()
                        .map(|q| (q.x as i32, q.y as i32, q.width as u32, q.height as u32))
                        .collect(),
                );
                gc.clip_origin = (r.clip_x_origin as i32, r.clip_y_origin as i32);
            }
            Request::FreeGC(r) => {
                self.gcs.remove(&r.gc);
            }
            Request::ClearArea(r) => {
                let Some(win) = self.wins.get(&r.window) else {
                    return self.error(idx, BAD_WINDOW, r.window, major, 0);
                };
                let w = if r.width == 0 {
                    win.w.saturating_sub(r.x.max(0) as u32)
                } else {
                    r.width as u32
                };
                let h = if r.height == 0 {
                    win.h.saturating_sub(r.y.max(0) as u32)
                } else {
                    r.height as u32
                };
                self.paint_bg(r.window, (r.x as i32, r.y as i32, w, h));
                if r.exposures {
                    let (win, x, y) = (r.window, r.x, r.y);
                    self.deliver(win, EXPOSURE, |_| {
                        Ev::new(12, 0)
                            .u32(win)
                            .u16(x as u16)
                            .u16(y as u16)
                            .u16(w as u16)
                            .u16(h as u16)
                            .u16(0)
                            .0
                    });
                }
            }
            Request::CopyArea(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let (w, h) = (r.width as u32, r.height as u32);
                let mut px = vec![0u32; (w * h) as usize];
                self.read(r.src_drawable, |t| {
                    for j in 0..h {
                        for i in 0..w {
                            px[(j * w + i) as usize] =
                                t.get(r.src_x as i32 + i as i32, r.src_y as i32 + j as i32);
                        }
                    }
                });
                if !self.draw(r.dst_drawable, |t| {
                    t.put(&gc, r.dst_x as i32, r.dst_y as i32, w, h, |i, j| {
                        Some(px[(j * w + i) as usize])
                    })
                }) && !self.pixmaps.contains_key(&r.dst_drawable)
                {
                    return self.error(idx, BAD_DRAWABLE, r.dst_drawable, major, 0);
                }
                if gc.exposures {
                    let d = r.dst_drawable;
                    self.send_event(idx, Ev::new(14, 0).u32(d).u16(0).u8(major).0);
                }
            }
            Request::CopyPlane(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let (w, h) = (r.width as u32, r.height as u32);
                let mut px = vec![0u32; (w * h) as usize];
                let plane = r.bit_plane;
                self.read(r.src_drawable, |t| {
                    for j in 0..h {
                        for i in 0..w {
                            let v = t.get(r.src_x as i32 + i as i32, r.src_y as i32 + j as i32);
                            px[(j * w + i) as usize] = if v & plane != 0 { gc.fg } else { gc.bg };
                        }
                    }
                });
                self.draw(r.dst_drawable, |t| {
                    t.put(&gc, r.dst_x as i32, r.dst_y as i32, w, h, |i, j| {
                        Some(px[(j * w + i) as usize])
                    })
                });
                if gc.exposures {
                    let d = r.dst_drawable;
                    self.send_event(idx, Ev::new(14, 0).u32(d).u16(0).u8(major).0);
                }
            }
            Request::PolyPoint(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let pts = points(&r.points, u32::from(r.coordinate_mode) == 1);
                self.draw(r.drawable, |t| t.points(&gc, &pts));
            }
            Request::PolyLine(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let pts = points(&r.points, u32::from(r.coordinate_mode) == 1);
                self.draw(r.drawable, |t| t.lines(&gc, &pts, false));
            }
            Request::PolySegment(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let segs: Vec<_> = r
                    .segments
                    .iter()
                    .map(|s| ((s.x1 as i32, s.y1 as i32), (s.x2 as i32, s.y2 as i32)))
                    .collect();
                self.draw(r.drawable, |t| t.segments(&gc, &segs));
            }
            Request::PolyRectangle(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let rects: Vec<_> = r
                    .rectangles
                    .iter()
                    .map(|q| (q.x as i32, q.y as i32, q.width as i32, q.height as i32))
                    .collect();
                self.draw(r.drawable, |t| {
                    for (x, y, w, h) in rects {
                        t.lines(&gc, &[(x, y), (x + w, y), (x + w, y + h), (x, y + h)], true);
                    }
                });
            }
            Request::PolyArc(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let arcs: Vec<_> = r
                    .arcs
                    .iter()
                    .map(|a| {
                        (
                            a.x as i32,
                            a.y as i32,
                            a.width as u32,
                            a.height as u32,
                            a.angle1 as i32,
                            a.angle2 as i32,
                        )
                    })
                    .collect();
                self.draw(r.drawable, |t| {
                    arcs.iter().for_each(|&a| t.arc(&gc, a, false))
                });
            }
            Request::PolyFillArc(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let arcs: Vec<_> = r
                    .arcs
                    .iter()
                    .map(|a| {
                        (
                            a.x as i32,
                            a.y as i32,
                            a.width as u32,
                            a.height as u32,
                            a.angle1 as i32,
                            a.angle2 as i32,
                        )
                    })
                    .collect();
                self.draw(r.drawable, |t| {
                    arcs.iter().for_each(|&a| t.arc(&gc, a, true))
                });
            }
            Request::FillPoly(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let pts = points(&r.points, u32::from(r.coordinate_mode) == 1);
                self.draw(r.drawable, |t| t.fill_poly(&gc, &pts));
            }
            Request::PolyFillRectangle(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let rects: Vec<_> = r
                    .rectangles
                    .iter()
                    .map(|q| (q.x as i32, q.y as i32, q.width as u32, q.height as u32))
                    .collect();
                if !self.draw(r.drawable, |t| t.fill_rects(&gc, &rects)) {
                    return self.error(idx, BAD_DRAWABLE, r.drawable, major, 0);
                }
            }
            Request::PutImage(r) => {
                let Some(gc) = self.gc_for(r.gc) else {
                    return self.error(idx, BAD_GC, r.gc, major, 0);
                };
                let (format, depth, w, h, pad) = (
                    u8::from(r.format),
                    r.depth,
                    r.width as u32,
                    r.height as u32,
                    r.left_pad,
                );
                let data = r.data.clone();
                self.draw(r.drawable, |t| {
                    t.put(&gc, r.dst_x as i32, r.dst_y as i32, w, h, |i, j| {
                        draw::image_pixel(format, depth, &data, w, pad, &gc, i, j)
                    })
                });
            }
            Request::GetImage(r) => {
                let (w, h) = (r.width as u32, r.height as u32);
                let depth = self
                    .pixmaps
                    .get(&r.drawable)
                    .map(|p| p.depth)
                    .or_else(|| self.wins.get(&r.drawable).map(|w| w.depth));
                let Some(depth) = depth else {
                    return self.error(idx, BAD_DRAWABLE, r.drawable, major, 0);
                };
                let mut data = Vec::new();
                if depth == 1 {
                    let stride = w.div_ceil(32) * 4;
                    data = vec![0u8; (stride * h) as usize];
                    self.read(r.drawable, |t| {
                        for j in 0..h {
                            for i in 0..w {
                                if t.get(r.x as i32 + i as i32, r.y as i32 + j as i32) & 1 == 1 {
                                    data[(j * stride + i / 8) as usize] |= 1 << (i % 8);
                                }
                            }
                        }
                    });
                } else {
                    data.resize((w * h * 4) as usize, 0);
                    self.read(r.drawable, |t| {
                        for j in 0..h {
                            for i in 0..w {
                                let o = ((j * w + i) * 4) as usize;
                                data[o..o + 4].copy_from_slice(
                                    &t.get(r.x as i32 + i as i32, r.y as i32 + j as i32)
                                        .to_le_bytes(),
                                );
                            }
                        }
                    });
                }
                self.reply(
                    idx,
                    Rep::new(seq, depth)
                        .u32(if depth == 1 { 0 } else { VISUAL })
                        .pad(20)
                        .bytes(&data),
                );
            }
            Request::ImageText8(r) => {
                let text: Vec<u16> = r.string.iter().map(|&b| b as u16).collect();
                self.text(
                    idx, major, r.drawable, r.gc, r.x as i32, r.y as i32, &text, true,
                );
            }
            Request::ImageText16(r) => {
                let text: Vec<u16> = r
                    .string
                    .iter()
                    .map(|c| (c.byte1 as u16) << 8 | c.byte2 as u16)
                    .collect();
                self.text(
                    idx, major, r.drawable, r.gc, r.x as i32, r.y as i32, &text, true,
                );
            }
            Request::PolyText8(r) => self.poly_text(
                idx, major, r.drawable, r.gc, r.x as i32, r.y as i32, &r.items, false,
            ),
            Request::PolyText16(r) => self.poly_text(
                idx, major, r.drawable, r.gc, r.x as i32, r.y as i32, &r.items, true,
            ),
            Request::CreateColormap(_)
            | Request::FreeColormap(_)
            | Request::CopyColormapAndFree(_)
            | Request::InstallColormap(_)
            | Request::UninstallColormap(_)
            | Request::FreeColors(_)
            | Request::StoreColors(_)
            | Request::StoreNamedColor(_) => {}
            Request::ListInstalledColormaps(_) => {
                self.reply(idx, Rep::new(seq, 0).u16(1).pad(22).u32(COLORMAP))
            }
            Request::AllocColor(r) => {
                let (red, green, blue) = (r.red >> 8, r.green >> 8, r.blue >> 8);
                let pixel = (red as u32) << 16 | (green as u32) << 8 | blue as u32;
                self.reply(
                    idx,
                    Rep::new(seq, 0)
                        .u16(red * 257)
                        .u16(green * 257)
                        .u16(blue * 257)
                        .pad(2)
                        .u32(pixel),
                );
            }
            Request::AllocNamedColor(r) => match crate::colors::lookup(&r.name) {
                Some((red, green, blue)) => {
                    let pixel = (red as u32) << 16 | (green as u32) << 8 | blue as u32;
                    let (a, b, c) = (red as u16 * 257, green as u16 * 257, blue as u16 * 257);
                    self.reply(
                        idx,
                        Rep::new(seq, 0)
                            .u32(pixel)
                            .u16(a)
                            .u16(b)
                            .u16(c)
                            .u16(a)
                            .u16(b)
                            .u16(c),
                    );
                }
                None => self.error(idx, BAD_NAME, 0, major, 0),
            },
            Request::LookupColor(r) => match crate::colors::lookup(&r.name) {
                Some((red, green, blue)) => {
                    let (a, b, c) = (red as u16 * 257, green as u16 * 257, blue as u16 * 257);
                    self.reply(
                        idx,
                        Rep::new(seq, 0).u16(a).u16(b).u16(c).u16(a).u16(b).u16(c),
                    );
                }
                None => self.error(idx, BAD_NAME, 0, major, 0),
            },
            Request::AllocColorCells(_) | Request::AllocColorPlanes(_) => {
                self.error(idx, BAD_ALLOC, 0, major, 0)
            }
            Request::QueryColors(r) => {
                let mut rep = Rep::new(seq, 0).u16(r.pixels.len() as u16).pad(22);
                for &p in r.pixels.iter() {
                    rep = rep
                        .u16(((p >> 16) & 0xff) as u16 * 257)
                        .u16(((p >> 8) & 0xff) as u16 * 257)
                        .u16((p & 0xff) as u16 * 257)
                        .pad(2);
                }
                self.reply(idx, rep);
            }
            Request::CreateCursor(_)
            | Request::CreateGlyphCursor(_)
            | Request::FreeCursor(_)
            | Request::RecolorCursor(_) => {}
            Request::QueryBestSize(r) => {
                self.reply(idx, Rep::new(seq, 0).u16(r.width).u16(r.height))
            }
            Request::QueryExtension(r) => {
                let e = crate::ext::EXTS.iter().find(|e| e.name.as_bytes() == r.name.as_ref());
                let rep = match e {
                    Some(e) => Rep::new(seq, 0).u8(1).u8(e.major).u8(e.first_event).u8(e.first_error),
                    None => Rep::new(seq, 0).u8(0).u8(0).u8(0).u8(0),
                };
                self.reply(idx, rep);
            }
            Request::ListExtensions(_) => {
                let mut rep = Rep::new(seq, crate::ext::EXTS.len() as u8).pad(24);
                for e in crate::ext::EXTS {
                    rep = rep.u8(e.name.len() as u8).bytes(e.name.as_bytes());
                }
                self.reply(idx, rep);
            }
            Request::ChangeKeyboardMapping(_)
            | Request::ChangeKeyboardControl(_)
            | Request::Bell(_)
            | Request::ChangePointerControl(_)
            | Request::SetScreenSaver(_)
            | Request::ChangeHosts(_)
            | Request::SetAccessControl(_)
            | Request::SetCloseDownMode(_)
            | Request::RotateProperties(_)
            | Request::ForceScreenSaver(_)
            | Request::NoOperation(_) => {}
            Request::GetKeyboardMapping(r) => {
                let first = r.first_keycode.max(keys::MIN) as usize - keys::MIN as usize;
                let n = r.count as usize;
                let mut rep = Rep::new(seq, keys::PER as u8).pad(24);
                for k in first..first + n {
                    for l in 0..keys::PER {
                        rep = rep.u32(
                            self.keymap
                                .syms
                                .get(k * keys::PER + l)
                                .copied()
                                .unwrap_or(0),
                        );
                    }
                }
                self.reply(idx, rep);
            }
            Request::GetKeyboardControl(_) => self.reply(
                idx,
                Rep::new(seq, 1)
                    .u32(0)
                    .u8(0)
                    .u8(50)
                    .u16(400)
                    .u16(100)
                    .pad(2)
                    .pad(32),
            ),
            Request::GetPointerControl(_) => self.reply(idx, Rep::new(seq, 0).u16(2).u16(1).u16(4)),
            Request::GetScreenSaver(_) => {
                self.reply(idx, Rep::new(seq, 0).u16(0).u16(0).u8(1).u8(1))
            }
            Request::ListHosts(_) => self.reply(idx, Rep::new(seq, 0).u16(0).pad(22)),
            Request::KillClient(r) => {
                if r.resource != 0 {
                    self.close(client_of(r.resource));
                }
            }
            Request::SetPointerMapping(_) | Request::SetModifierMapping(_) => {
                self.reply(idx, Rep::new(seq, 0))
            }
            Request::GetPointerMapping(_) => self.reply(
                idx,
                Rep::new(seq, 9).pad(24).bytes(&[1, 2, 3, 4, 5, 6, 7, 8, 9]),
            ),
            Request::GetModifierMapping(_) => {
                let mut rep = Rep::new(seq, 2).pad(24);
                for m in keys::MODIFIERS {
                    rep = rep.u8(m[0]).u8(m[1]);
                }
                self.reply(idx, rep);
            }
            // extensions (ext.rs)
            other => self.handle_ext(idx, seq, major, other),
        }
    }

    // ---- text ----

    /// A font id, or a graphics context's font (QueryFont takes either); 0 is the server's `fixed`.
    fn font_of(&mut self, id: u32) -> Option<std::rc::Rc<crate::fonts::XFont>> {
        if let Some(f) = self.fonts.open.get(&id) {
            return Some(f.clone());
        }
        match self.gcs.get(&id).map(|g| g.font) {
            Some(fid) if fid != 0 && fid != id => self.font_of(fid),
            Some(_) => self.fonts.load("fixed"),
            None => None,
        }
    }

    /// QueryFont's and ListFontsWithInfo's description of a font (`name`: the latter, with its name).
    fn font_info(&mut self, rep: Rep, f: &crate::fonts::XFont, name: Option<&str>) -> Rep {
        let ci = |r: Rep, c: &crate::fonts::CharInfo| {
            r.i16(c.lsb)
                .i16(c.rsb)
                .i16(c.width)
                .i16(c.ascent)
                .i16(c.descent)
                .u16(0)
        };
        let mut lo = crate::fonts::CharInfo {
            lsb: i16::MAX,
            rsb: i16::MAX,
            width: i16::MAX,
            ascent: i16::MAX,
            descent: i16::MAX,
        };
        let mut hi = crate::fonts::CharInfo {
            lsb: i16::MIN,
            rsb: i16::MIN,
            width: i16::MIN,
            ascent: i16::MIN,
            descent: i16::MIN,
        };
        for c in &f.chars[32..] {
            lo = crate::fonts::CharInfo {
                lsb: lo.lsb.min(c.lsb),
                rsb: lo.rsb.min(c.rsb),
                width: lo.width.min(c.width),
                ascent: lo.ascent.min(c.ascent),
                descent: lo.descent.min(c.descent),
            };
            hi = crate::fonts::CharInfo {
                lsb: hi.lsb.max(c.lsb),
                rsb: hi.rsb.max(c.rsb),
                width: hi.width.max(c.width),
                ascent: hi.ascent.max(c.ascent),
                descent: hi.descent.max(c.descent),
            };
        }
        let font_atom = self.intern(f.name.as_bytes());
        let mut r = ci(ci(rep, &lo).pad(4), &hi)
            .pad(4)
            .u16(0)
            .u16(255)
            .u16(32)
            .u16(1)
            .u8(0)
            .u8(0)
            .u8(0)
            .u8(1)
            .i16(f.ascent)
            .i16(f.descent);
        r = r.u32(if name.is_some() {
            0
        } else {
            f.chars.len() as u32
        });
        // FONT: the font's full name, which programs derive bold and italic names from
        r = r.u32(ATOM_FONT).u32(font_atom);
        match name {
            Some(n) => r.bytes(n.as_bytes()),
            None => f.chars.iter().fold(r, |r, c| ci(r, c)),
        }
    }

    /// Font names matching a pattern: the pattern itself, wildcards filled as the font they open.
    fn font_names(&mut self, pattern: &[u8], max: u16) -> Vec<String> {
        if max == 0 {
            return Vec::new();
        }
        let p = String::from_utf8_lossy(pattern).into_owned();
        if !p.contains(['*', '?']) {
            return vec![p];
        }
        self.fonts
            .load(&p)
            .map(|f| vec![f.name.clone()])
            .unwrap_or_default()
    }

    fn text(
        &mut self,
        idx: usize,
        major: u8,
        d: u32,
        gid: u32,
        x: i32,
        y: i32,
        text: &[u16],
        image: bool,
    ) {
        let Some(gc) = self.gc_for(gid) else {
            return self.error(idx, BAD_GC, gid, major, 0);
        };
        let Some(f) = self.font_of(gid) else { return };
        self.draw(d, |t| {
            if image {
                // ImageText: the text's box in the background first
                t.clear(
                    (
                        x,
                        y - f.ascent as i32,
                        f.width(text).max(0) as u32,
                        (f.ascent + f.descent) as u32,
                    ),
                    gc.bg,
                );
            }
            let mut pen = x;
            for &c in text {
                if let Some(g) = f.glyph(c) {
                    t.glyph(&gc, pen, y, g);
                }
                pen += f.adv(c);
            }
        });
    }

    /// PolyText items: (length, delta, characters), or 255 and a new font (4 bytes, most significant first).
    fn poly_text(
        &mut self,
        idx: usize,
        major: u8,
        d: u32,
        gid: u32,
        mut x: i32,
        y: i32,
        items: &[u8],
        wide: bool,
    ) {
        let mut i = 0;
        while i < items.len() {
            let n = items[i] as usize;
            if n == 0 {
                break;
            }
            if n == 255 {
                if i + 5 > items.len() {
                    break;
                }
                let fid =
                    u32::from_be_bytes([items[i + 1], items[i + 2], items[i + 3], items[i + 4]]);
                if let Some(g) = self.gcs.get_mut(&gid) {
                    g.font = fid;
                }
                i += 5;
                continue;
            }
            let delta = items.get(i + 1).copied().unwrap_or(0) as i8 as i32;
            let size = if wide { 2 } else { 1 };
            let s = &items[(i + 2).min(items.len())..(i + 2 + n * size).min(items.len())];
            let text: Vec<u16> = if wide {
                s.chunks_exact(2)
                    .map(|c| (c[0] as u16) << 8 | c[1] as u16)
                    .collect()
            } else {
                s.iter().map(|&b| b as u16).collect()
            };
            x += delta;
            self.text(idx, major, d, gid, x, y, &text, false);
            let adv = self.font_of(gid).map(|f| f.width(&text)).unwrap_or(0);
            x += adv;
            i += 2 + n * size;
        }
    }

    fn property_notify(&mut self, w: u32, atom: u32, state: u8) {
        let time = self.now();
        self.deliver(w, PROPERTY_CHANGE, |ev| {
            Ev::new(28, 0).u32(ev).u32(atom).u32(time).u8(state).0
        });
    }
}

fn points(p: &[xproto::Point], relative: bool) -> Vec<(i32, i32)> {
    let mut out = Vec::with_capacity(p.len());
    let (mut x, mut y) = (0, 0);
    for (i, q) in p.iter().enumerate() {
        if relative && i > 0 {
            x += q.x as i32;
            y += q.y as i32;
        } else {
            x = q.x as i32;
            y = q.y as i32;
        }
        out.push((x, y));
    }
    out
}

/// The X display's sockets (filesystem and abstract) and lock file; a dead server's leftovers are cleared.
fn listen(n: u32) -> std::io::Result<Vec<UnixListener>> {
    use std::os::linux::net::SocketAddrExt;
    let lock = format!("/tmp/.X{n}-lock");
    if let Ok(pid) = std::fs::read_to_string(&lock) {
        let pid: i32 = pid.trim().parse().unwrap_or(0);
        if pid > 0 && unsafe { libc::kill(pid, 0) } == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("display :{n} is in use (pid {pid})"),
            ));
        }
    }
    std::fs::write(&lock, format!("{:>10}\n", std::process::id()))?;
    let _ = std::fs::create_dir_all("/tmp/.X11-unix");
    let path = format!("/tmp/.X11-unix/X{n}");
    let _ = std::fs::remove_file(&path);
    let mut out = vec![UnixListener::bind(&path)?];
    if let Ok(addr) = std::os::unix::net::SocketAddr::from_abstract_name(path.as_bytes()) {
        if let Ok(l) = UnixListener::bind_addr(&addr) {
            out.push(l);
        }
    }
    Ok(out)
}

pub fn run(display: u32, size: (u16, u16), scale: f64) -> Result<(), Box<dyn std::error::Error>> {
    let (wl, mut queue) = wl::connect(scale)?;
    let mut ev: EventLoop<Server> = EventLoop::try_new()?;
    let mut s = Server::new(wl, ev.handle(), size);
    // the seat's pointer, keyboard and keymap before the first client
    queue.roundtrip(&mut s)?;
    // the clipboard shared with Wayland (once the seat is known)
    if let Some(m) = s.wl.data_control.take() {
        s.clip_init(m);
    }
    queue.roundtrip(&mut s)?;
    let listeners = listen(display)?;
    for l in listeners {
        l.set_nonblocking(true)?;
        ev.handle().insert_source(
            Generic::new(l, Interest::READ, Mode::Level),
            |_, l, s: &mut Server| {
                while let Ok((stream, _)) = l.accept() {
                    s.new_client(stream);
                }
                Ok(PostAction::Continue)
            },
        )?;
    }
    let conn = s.wl.conn.clone();
    let fd = std::os::fd::AsRawFd::as_raw_fd(&conn.backend().poll_fd());
    // Safety: the connection (and its fd) lives as long as the loop
    let w = unsafe { FdWrapper::new(fd) };
    ev.handle().insert_source(
        Generic::new(w, Interest::READ, Mode::Level),
        move |_, _, _: &mut Server| {
            if let Some(g) = conn.prepare_read() {
                let _ = g.read();
            }
            Ok(PostAction::Continue)
        },
    )?;
    eprintln!("loom-x: DISPLAY=:{display} ({}x{})", size.0, size.1);
    // with no X program for this long it ends (loom starts it again for the next one); 0 = never
    let idle = std::env::var("LOOM_X_IDLE").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(30);
    // SIGTERM, SIGINT, SIGHUP: the loop ends and the socket and lock go (a killed server leaves none behind)
    let quit = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == 0 {
            SIGNAL_FD.store(fds[1], std::sync::atomic::Ordering::SeqCst);
            for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
            }
            let r: std::fs::File = unsafe { std::os::fd::FromRawFd::from_raw_fd(fds[0]) };
            let q = quit.clone();
            ev.handle().insert_source(Generic::new(r, Interest::READ, Mode::Level), move |_, _, _: &mut Server| {
                q.set(true);
                Ok(PostAction::Remove)
            })?;
        }
    }
    let mut empty_since = Some(std::time::Instant::now());
    let result = loop {
        if quit.get() {
            break Ok(());
        }
        if let Err(e) = queue.dispatch_pending(&mut s) {
            break Err(e.into());
        }
        s.flush();
        if let Err(e) = s.wl.conn.flush() {
            break Err(e.into());
        }
        match (s.clients.is_empty(), empty_since) {
            (false, _) => empty_since = None,
            (true, None) => empty_since = Some(std::time::Instant::now()),
            (true, Some(t)) if idle > 0 && t.elapsed().as_secs() >= idle => break Ok(()),
            _ => {}
        }
        // a client that could not take all its output yet is tried again shortly; an idle server checks back to end
        let wait = if s.output_waiting() {
            Some(std::time::Duration::from_millis(5))
        } else if empty_since.is_some() && idle > 0 {
            Some(std::time::Duration::from_secs(idle))
        } else {
            None
        };
        if let Err(e) = ev.dispatch(wait, &mut s) {
            break Err(e.into());
        }
    };
    let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{display}"));
    let _ = std::fs::remove_file(format!("/tmp/.X{display}-lock"));
    result
}

impl Server {
    /// loom's scale for X windows changed (another screen): shown at the new logical size.
    pub fn scale_changed(&mut self, scale: f64) {
        if (scale - self.wl.scale).abs() < 1e-3 {
            return;
        }
        self.wl.scale = scale;
        for w in self.wins.values_mut() {
            if let Some(t) = w.top.as_mut() {
                t.dirty = true;
            }
        }
    }
}

/// A SelectionRequest event (to a selection's owner).
pub fn selection_request(time: u32, owner: u32, requestor: u32, sel: u32, target: u32, property: u32) -> [u8; 32] {
    Ev::new(30, 0).u32(time).u32(owner).u32(requestor).u32(sel).u32(target).u32(property).0
}

impl Server {
    pub fn atom_name(&self, a: u32) -> String {
        self.atoms.get((a as usize).wrapping_sub(1)).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default()
    }

    /// An event to the client a window belongs to (none for loom-x's own).
    pub fn send_to(&mut self, w: u32, ev: [u8; 32]) {
        let c = client_of(w);
        if c != 0 {
            self.send_event(c, ev);
        }
    }

    pub fn set_prop(&mut self, w: u32, atom: u32, type_: u32, format: u8, data: Vec<u8>) {
        let Some(win) = self.wins.get_mut(&w) else { return };
        win.props.insert(atom, Prop { type_, format, data });
        self.property_notify(w, atom, 0);
    }

    /// A property's data, the property gone.
    pub fn take_prop(&mut self, w: u32, atom: u32) -> Vec<u8> {
        let data = self.wins.get_mut(&w).and_then(|x| x.props.remove(&atom)).map(|p| p.data).unwrap_or_default();
        self.property_notify(w, atom, 1);
        data
    }

    pub fn selection_notify(&mut self, requestor: u32, sel: u32, target: u32, property: u32, time: u32) {
        self.send_to(requestor, Ev::new(31, 0).u32(time).u32(requestor).u32(sel).u32(target).u32(property).0);
    }

    /// `owner` owns selection `sel` now; the one before is told (SelectionClear).
    pub fn take_selection(&mut self, sel: u32, owner: u32) {
        let time = self.now();
        if let Some(o) = self.selections.insert(sel, owner).filter(|&o| o != owner) {
            self.send_to(o, Ev::new(29, 0).u32(time).u32(o).u32(sel).0);
        }
        self.fixes_selection(sel, owner, 0);
    }
}

impl Server {
    /// The window a menu or tooltip belongs to: where the pointer or keyboard is, else another of its program's.
    fn popup_parent(&self, w: u32) -> Option<u32> {
        let normal = |t: &u32| self.wins.get(t).and_then(|x| x.top.as_ref()).is_some_and(|t| t.toplevel.is_some());
        let owner = client_of(w);
        self.ptr_top
            .filter(|t| normal(t) && client_of(*t) == owner)
            .or(self.kb_top.filter(|t| normal(t) && client_of(*t) == owner))
            .or_else(|| self.wins.keys().copied().find(|t| normal(t) && client_of(*t) == owner))
            .or(self.ptr_top.filter(normal))
    }

    /// loom took a popup down (a click outside): it is hidden; the program unmaps it when it notices.
    pub fn popup_done(&mut self, w: u32) {
        if let Some(win) = self.wins.get_mut(&w) {
            win.top = None;
        }
    }
}

impl Server {
    pub fn set_bigreq(&mut self, idx: usize) {
        if let Some(c) = self.clients.get_mut(&idx) {
            c.bigreq = true;
        }
    }

    /// The screen in X pixels and millimetres.
    pub fn screen(&self) -> (u16, u16) {
        self.size
    }
    pub fn screen_mm(&self) -> (u16, u16) {
        let dpi = 96.0 * self.wl.scale;
        ((self.size.0 as f64 * 25.4 / dpi).round() as u16, (self.size.1 as f64 * 25.4 / dpi).round() as u16)
    }

    /// A depth-1 pixmap's pixels as on/off (shape masks).
    pub fn bitmap_bits(&self, p: u32) -> Option<(u32, u32, Vec<bool>)> {
        let pix = self.pixmaps.get(&p)?;
        let (w, h) = (pix.img.width(), pix.img.height());
        Some((w, h, pix.img.pixels().iter().map(|c| c.red() > 127).collect()))
    }
}

/// One read from a client socket, with any file descriptors passed along it (SCM_RIGHTS).
fn recv(fd: std::os::fd::RawFd, buf: &mut [u8], fds: &mut Vec<std::os::fd::OwnedFd>) -> std::io::Result<usize> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr() as *mut _, iov_len: buf.len() };
    let mut cmsg = [0u8; 256];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg.as_mut_ptr() as *mut _;
    msg.msg_controllen = cmsg.len();
    let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<i32>();
                let p = libc::CMSG_DATA(c) as *const i32;
                for i in 0..count {
                    fds.push(std::os::fd::FromRawFd::from_raw_fd(*p.add(i)));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    Ok(n as usize)
}

impl Server {
    pub fn gc(&self, id: u32) -> Option<Gc> {
        self.gcs.get(&id).cloned()
    }

    pub fn depth_of(&self, d: u32) -> Option<u8> {
        self.pixmaps.get(&d).map(|p| p.depth).or_else(|| self.wins.get(&d).map(|w| w.depth))
    }

    /// An image's pixels (`px(i, j, gc)`, None = left alone) drawn at (x, y) of drawable `d`.
    pub fn draw_image(&mut self, d: u32, gc: &Gc, x: i32, y: i32, w: u32, h: u32, px: impl Fn(u32, u32, &Gc) -> Option<u32>) {
        self.draw(d, |t| t.put(gc, x, y, w, h, |i, j| px(i, j, gc)));
    }

    /// Drawable `d`'s pixels in a rectangle, row by row.
    pub fn read_pixels(&mut self, d: u32, x: i32, y: i32, w: u32, h: u32, out: &mut [u32]) {
        self.read(d, |t| {
            for j in 0..h {
                for i in 0..w {
                    out[(j * w + i) as usize] = t.get(x + i as i32, y + j as i32);
                }
            }
        });
    }
}

impl Server {
    /// A window's background in a rectangle: its pixmap tiled from its corner, or its colour (none: left alone).
    fn paint_bg(&mut self, w: u32, rect: (i32, i32, u32, u32)) {
        let Some(win) = self.wins.get(&w) else { return };
        let (bg, pm) = (win.bg, win.bg_pixmap);
        if let Some(p) = pm.and_then(|p| self.pixmaps.get(&p)) {
            let (pw, ph) = (p.img.width(), p.img.height());
            let depth = p.depth;
            let tile: Vec<u32> = p.img.pixels().iter().map(|c| crate::draw::pixel_of(*c, depth)).collect();
            let gc = Gc::default();
            let (x0, y0) = (rect.0, rect.1);
            self.draw(w, |t| {
                t.put(&gc, x0, y0, rect.2, rect.3, |i, j| {
                    let (tx, ty) = ((x0 + i as i32).rem_euclid(pw as i32) as u32, (y0 + j as i32).rem_euclid(ph as i32) as u32);
                    Some(tile[(ty * pw + tx) as usize])
                })
            });
        } else if let Some(px) = bg {
            self.draw(w, |t| t.clear(rect, px));
        }
    }
}

/// Bytes to a client with file descriptors attached (SCM_RIGHTS); they arrive with the first byte.
fn send(fd: std::os::fd::RawFd, buf: &[u8], fds: Vec<std::os::fd::OwnedFd>) -> std::io::Result<usize> {
    use std::os::fd::AsRawFd;
    let mut iov = libc::iovec { iov_base: buf.as_ptr() as *mut _, iov_len: buf.len() };
    let raw: Vec<i32> = fds.iter().map(|f| f.as_raw_fd()).collect();
    let space = unsafe { libc::CMSG_SPACE((raw.len() * std::mem::size_of::<i32>()) as u32) } as usize;
    let mut cmsg = vec![0u8; space];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg.as_mut_ptr() as *mut _;
    msg.msg_controllen = space;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN((raw.len() * std::mem::size_of::<i32>()) as u32) as usize;
        std::ptr::copy_nonoverlapping(raw.as_ptr(), libc::CMSG_DATA(c) as *mut i32, raw.len());
    }
    let n = unsafe { libc::sendmsg(fd, &msg, libc::MSG_NOSIGNAL) };
    // the fds are the client's now; ours close here
    drop(fds);
    if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(n as usize) }
}

impl Server {
    pub fn pass_fd(&mut self, idx: usize, fd: std::os::fd::OwnedFd) {
        if let Some(c) = self.clients.get_mut(&idx) {
            c.out_fds.push(fd);
        }
    }
}

impl Server {
    pub fn pixmaps_size(&self, p: u32) -> Option<(u32, u32)> {
        self.pixmaps.get(&p).map(|p| (p.img.width(), p.img.height()))
    }
}

impl Server {
    /// A GC to draw with: its tile or stipple made into a picture to repeat.
    pub fn gc_for(&self, id: u32) -> Option<Gc> {
        let mut gc = self.gcs.get(&id)?.clone();
        let pat = match gc.fill_style {
            1 => self.pixmaps.get(&gc.tile).map(|p| p.img.clone()),
            2 | 3 => self.pixmaps.get(&gc.stipple).and_then(|p| {
                let mut img = tiny_skia::Pixmap::new(p.img.width(), p.img.height())?;
                let fg = crate::draw::color(gc.fg, 24).to_color_u8().premultiply();
                let bg = if gc.fill_style == 3 { crate::draw::color(gc.bg, 24).to_color_u8().premultiply() } else { tiny_skia::PremultipliedColorU8::TRANSPARENT };
                for (o, s) in img.pixels_mut().iter_mut().zip(p.img.pixels()) {
                    *o = if s.red() > 127 { fg } else { bg };
                }
                Some(img)
            }),
            _ => None,
        };
        gc.pattern = pat.map(std::rc::Rc::new);
        Some(gc)
    }
}

impl Server {
    /// A generic (XI2) event of any length, its sequence number filled in.
    pub fn send_generic(&mut self, idx: usize, mut ev: Vec<u8>) {
        if let Some(c) = self.clients.get_mut(&idx) {
            ev[2..4].copy_from_slice(&c.seq.to_le_bytes());
            c.out.extend_from_slice(&ev);
        }
    }

    /// The child of `w` on the way down to `under` (0: none).
    pub fn child_toward(&self, w: u32, under: u32) -> u32 {
        let mut c = under;
        while c != w && c != ROOT && c != 0 {
            let p = self.wins.get(&c).map(|x| x.parent).unwrap_or(0);
            if p == w {
                return c;
            }
            c = p;
        }
        0
    }

    /// The pointer for queries: on the root, in `w`, the child of `w` it is in, buttons, modifiers.
    pub fn pointer_state(&self, w: u32) -> ((f64, f64), (f64, f64), u32, u16, u16) {
        let (tx, ty) = self.ptr;
        let (rx, ry) = self.ptr_top.and_then(|t| self.wins.get(&t)).map(|t| (t.x as f64 + tx, t.y as f64 + ty)).unwrap_or((tx, ty));
        let (ox, oy) = self.on_root(w);
        ((rx, ry), (rx - ox as f64, ry - oy as f64), self.child_toward(w, self.under), self.buttons, self.mods)
    }

    pub fn focus_window(&self) -> u32 {
        self.focus
    }

    pub fn set_focus(&mut self, w: u32) {
        self.focus = w;
    }
}

impl Server {
    pub fn dri3_pixmap(&mut self, pid: u32, w: u32, h: u32, depth: u8, buf: crate::gpu::DmaBuf) {
        if let Some(img) = tiny_skia::Pixmap::new(w.max(1), h.max(1)) {
            self.pixmaps.insert(pid, Pix { img, depth, dmabuf: Some(buf) });
        }
    }

    /// A GPU-backed pixmap's current pixels into its picture (BGRA rows to RGBA; depth 24 is opaque).
    pub fn sync_dmabuf(&mut self, pid: u32) {
        let Some(p) = self.pixmaps.get_mut(&pid) else { return };
        let Some(buf) = p.dmabuf.as_ref() else { return };
        let (w, h) = (p.img.width() as usize, p.img.height() as usize);
        let opaque = p.depth != 32;
        let (stride, offset) = (buf.stride as usize, buf.offset as usize);
        let dst = p.img.data_mut();
        buf.with(|src| {
            for y in 0..h {
                let row = offset + y * stride;
                // one block read per row: GPU memory is uncached, reading it pixel by pixel is slow
                let Some(line) = src.get(row..row + w * 4).map(|l| l.to_vec()) else { break };
                for (x, s) in line.chunks_exact(4).enumerate() {
                    let d = &mut dst[(y * w + x) * 4..(y * w + x) * 4 + 4];
                    let a = if opaque { 255 } else { s[3] };
                    d.copy_from_slice(&[s[2].min(a), s[1].min(a), s[0].min(a), a]);
                }
            }
        });
    }

    /// loom shows this top-level window (Present waits for its frames).
    pub fn is_shown(&self, t: u32) -> bool {
        self.wins.get(&t).and_then(|w| w.top.as_ref()).is_some_and(|t| t.configured)
    }

    /// PresentPixmap: the pixmap's pixels into the window at (x, y).
    pub fn present(&mut self, window: u32, pixmap: u32, x: i32, y: i32) {
        self.sync_dmabuf(pixmap);
        let Some((pw, ph)) = self.pixmaps.get(&pixmap).map(|p| (p.img.width(), p.img.height())) else { return };
        // the common case: a buffer the size of a top-level window, copied whole
        let whole = x == 0 && y == 0 && self.wins.get(&window).is_some_and(|w| w.parent == ROOT && w.top.as_ref().is_some_and(|t| (t.img.width(), t.img.height()) == (pw, ph)));
        if whole {
            let data = self.pixmaps[&pixmap].img.data().to_vec();
            if let Some(t) = self.wins.get_mut(&window).and_then(|w| w.top.as_mut()) {
                t.img.data_mut().copy_from_slice(&data);
                t.dirty = true;
            }
            return;
        }
        let mut px = vec![0u32; (pw * ph) as usize];
        self.read_pixels(pixmap, 0, 0, pw, ph, &mut px);
        let gc = Gc::default();
        self.draw(window, |t| t.put(&gc, x, y, pw, ph, |i, j| Some(px[(j * pw + i) as usize])));
    }
}

impl Server {
    /// PresentPixmap without copying: a GPU pixmap the size of a top-level window goes to loom as it is.
    pub fn present_zero_copy(&mut self, window: u32, pixmap: u32) -> bool {
        let Some(win) = self.wins.get(&window) else { return false };
        let Some(top) = win.top.as_ref() else { return false };
        let alpha = win.depth == 32;
        let (tw, th) = (top.img.width(), top.img.height());
        let Some(p) = self.pixmaps.get_mut(&pixmap) else { return false };
        if (p.img.width(), p.img.height()) != (tw, th) || win.shape.is_some() {
            return false;
        }
        let Some(buf) = p.dmabuf.as_mut() else { return false };
        if buf.wl.is_none() {
            buf.wl = self.wl.dmabuf_buffer(std::os::fd::AsFd::as_fd(&buf.fd), buf.offset, buf.stride, tw, th, alpha, pixmap);
        }
        let Some(b) = buf.wl.clone() else { return false };
        let t = self.wins.get_mut(&window).unwrap().top.as_mut().unwrap();
        self.wl.show_external(t, &b, tw, th, pixmap);
        true
    }
}

impl Server {
    /// A window's or pixmap's size (0 when there is none).
    pub fn drawable_size_any(&self, d: u32) -> (u32, u32) {
        self.pixmaps.get(&d).map(|p| (p.img.width(), p.img.height())).or_else(|| self.wins.get(&d).map(|w| (w.w, w.h))).unwrap_or((0, 0))
    }
}

impl Server {
    /// Key repeat as loom's keyboard says: (delay ms, keys a second).
    pub fn repeat_info(&self) -> (u32, u32) {
        self.repeat
    }

    /// A key from loom: X programs expect the server to repeat held keys (Wayland leaves it to the client).
    pub fn key(&mut self, code: u32, pressed: bool) {
        if let Some((_, t)) = self.repeat_key.take() {
            self.handle.remove(t);
        }
        self.key_event(code, pressed);
        let repeats = self.keymap.xkb.repeats.get((code as usize).wrapping_sub(keys::MIN as usize)).copied().unwrap_or(false);
        let (delay, rate) = self.repeat;
        if !pressed || !repeats || rate == 0 {
            return;
        }
        let every = std::time::Duration::from_millis(1000 / rate.max(1) as u64);
        let t = self.handle.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_millis(delay as u64)), move |_, _, s: &mut Server| {
            // a release (for clients without detectable auto-repeat) and a press, again and again
            s.repeating = true;
            s.key_event(code, false);
            s.key_event(code, true);
            s.repeating = false;
            smithay::reexports::calloop::timer::TimeoutAction::ToDuration(every)
        });
        if let Ok(t) = t {
            self.repeat_key = Some((code, t));
        }
    }
}

/// The write end of the pipe a signal wakes the loop through.
static SIGNAL_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_signal(_: libc::c_int) {
    let fd = SIGNAL_FD.load(std::sync::atomic::Ordering::SeqCst);
    if fd >= 0 {
        // async-signal-safe: one byte down the pipe
        unsafe { libc::write(fd, b"q".as_ptr() as *const _, 1) };
    }
}
