//! X extensions: the table QueryExtension answers from, and BIG-REQUESTS, SHAPE (shaped windows as transparent pixels), RandR (the screen as one output), XFIXES (selection notices).

use x11rb_protocol::{
    protocol::Request,
    x11_utils::{ExtInfoProvider, ExtensionInformation},
};

use crate::server::{Rep, Server, ROOT};

pub struct Ext {
    pub name: &'static str,
    pub major: u8,
    pub first_event: u8,
    pub first_error: u8,
}

pub const EXTS: &[Ext] = &[
    Ext { name: "BIG-REQUESTS", major: 128, first_event: 0, first_error: 0 },
    Ext { name: "SHAPE", major: 129, first_event: 64, first_error: 0 },
    Ext { name: "RANDR", major: 130, first_event: 65, first_error: 128 },
    Ext { name: "XFIXES", major: 131, first_event: 67, first_error: 132 },
    Ext { name: "MIT-SHM", major: 132, first_event: 69, first_error: 133 },
    Ext { name: "RENDER", major: 133, first_event: 0, first_error: 134 },
    Ext { name: "XInputExtension", major: 134, first_event: 70, first_error: 139 },
    Ext { name: "DRI3", major: 135, first_event: 0, first_error: 0 },
    Ext { name: "SYNC", major: 136, first_event: 90, first_error: 144 },
    Ext { name: "Present", major: 137, first_event: 0, first_error: 0 },
    Ext { name: "GLX", major: 138, first_event: 95, first_error: 146 },
    Ext { name: "XKEYBOARD", major: 139, first_event: 97, first_error: 160 },
];

const XFIXES_EVENT: u8 = 67;
const CRTC: u32 = 0x60;
const OUTPUT: u32 = 0x61;
const MODE: u32 = 0x62;
pub const BIG_MAX: u32 = 4 * 1024 * 1024 - 1;

pub struct Table;
impl ExtInfoProvider for Table {
    fn get_from_major_opcode(&self, major: u8) -> Option<(&str, ExtensionInformation)> {
        EXTS.iter().find(|e| e.major == major).map(|e| (e.name, ExtensionInformation { major_opcode: e.major, first_event: e.first_event, first_error: e.first_error }))
    }
    fn get_from_event_code(&self, _: u8) -> Option<(&str, ExtensionInformation)> {
        None
    }
    fn get_from_error_code(&self, _: u8) -> Option<(&str, ExtensionInformation)> {
        None
    }
}

/// A window's bounding shape: which of its pixels are inside (row by row, its size when set).
#[derive(Clone)]
pub struct Shape {
    pub w: u32,
    pub h: u32,
    pub inside: Vec<bool>,
}

impl Shape {
    fn full(w: u32, h: u32, on: bool) -> Shape {
        Shape { w, h, inside: vec![on; (w * h) as usize] }
    }
    pub fn at(&self, x: u32, y: u32) -> bool {
        x < self.w && y < self.h && self.inside[(y * self.w + x) as usize]
    }
    /// Set, Union, Intersect, Subtract, Invert (SHAPE's operations) of `src` into this.
    fn combine(&mut self, op: u8, src: &Shape) {
        for (i, v) in self.inside.iter_mut().enumerate() {
            let s = src.inside.get(i).copied().unwrap_or(false);
            *v = match op {
                0 => s,
                1 => *v || s,
                2 => *v && s,
                3 => *v && !s,
                _ => !*v && s,
            };
        }
    }
    /// The inside as rectangles (a run per row), for GetRectangles.
    fn rects(&self) -> Vec<(i16, i16, u16, u16)> {
        let mut out = Vec::new();
        for y in 0..self.h {
            let mut x = 0;
            while x < self.w {
                if self.at(x, y) {
                    let s = x;
                    while x < self.w && self.at(x, y) {
                        x += 1;
                    }
                    out.push((s as i16, y as i16, (x - s) as u16, 1));
                } else {
                    x += 1;
                }
            }
        }
        out
    }
}

impl Server {

    fn reshape(&mut self, w: u32, kind: u8, op: u8, src: Shape) {
        // only the bounding shape shows (clip and input shapes are taken as the window)
        if kind != 0 {
            return;
        }
        let Some(win) = self.wins.get_mut(&w) else { return };
        let (ww, wh) = (win.w, win.h);
        let mut cur = win.shape.take().filter(|s| s.w == ww && s.h == wh).unwrap_or_else(|| Shape::full(ww, wh, true));
        cur.combine(op, &src);
        win.shape = if cur.inside.iter().all(|v| *v) { None } else { Some(cur) };
        if let Some(t) = self.top_of(w).and_then(|t| self.wins.get_mut(&t)).and_then(|t| t.top.as_mut()) {
            t.dirty = true;
        }
    }

    pub fn handle_ext(&mut self, idx: usize, seq: u16, major: u8, req: Request) {
        match req {
            Request::BigreqEnable(_) => {
                self.set_bigreq(idx);
                self.reply(idx, Rep::new(seq, 0).u32(BIG_MAX));
            }
            // ---- SHAPE ----
            Request::ShapeQueryVersion(_) => self.reply(idx, Rep::new(seq, 0).u16(1).u16(1)),
            Request::ShapeRectangles(r) => {
                let rects: Vec<_> = r.rectangles.iter().map(|q| (q.x as i32 + r.x_offset as i32, q.y as i32 + r.y_offset as i32, q.width as i32, q.height as i32)).collect();
                if let Some(s) = self.region_rects(r.destination_window, &rects) {
                    self.reshape(r.destination_window, u8::from(r.destination_kind), u8::from(r.operation), s);
                }
            }
            Request::ShapeMask(r) => {
                let (kind, op) = (u8::from(r.destination_kind), u8::from(r.operation));
                if r.source_bitmap == 0 {
                    // no bitmap: the shape is removed
                    if kind == 0 {
                        if let Some(w) = self.wins.get_mut(&r.destination_window) {
                            w.shape = None;
                        }
                    }
                    return;
                }
                let bits = self.bitmap_bits(r.source_bitmap);
                let (ox, oy) = (r.x_offset as i32, r.y_offset as i32);
                if let Some((bw, bh, bits)) = bits {
                    if let Some(s) = self.region_bits(r.destination_window, &bits, bw, bh, ox, oy) {
                        self.reshape(r.destination_window, kind, op, s);
                    }
                }
            }
            Request::ShapeCombine(r) => {
                let src = self.wins.get(&r.source_window).map(|w| w.shape.clone().unwrap_or_else(|| Shape::full(w.w, w.h, true)));
                let (ox, oy) = (r.x_offset as i32, r.y_offset as i32);
                if let Some(src) = src {
                    if let Some(s) = self.region_bits(r.destination_window, &src.inside, src.w, src.h, ox, oy) {
                        self.reshape(r.destination_window, u8::from(r.destination_kind), u8::from(r.operation), s);
                    }
                }
            }
            Request::ShapeOffset(_) | Request::ShapeSelectInput(_) => {}
            Request::ShapeInputSelected(_) => self.reply(idx, Rep::new(seq, 0)),
            Request::ShapeQueryExtents(r) => {
                let Some(w) = self.wins.get(&r.destination_window) else { return };
                let (ww, wh, shaped) = (w.w as u16, w.h as u16, w.shape.is_some());
                self.reply(idx, Rep::new(seq, 0).u8(shaped as u8).u8(0).pad(2).i16(0).i16(0).u16(ww).u16(wh).i16(0).i16(0).u16(ww).u16(wh));
            }
            Request::ShapeGetRectangles(r) => {
                let Some(w) = self.wins.get(&r.window) else { return };
                let rects = match (&w.shape, u8::from(r.source_kind)) {
                    (Some(s), 0) => s.rects(),
                    _ => vec![(0, 0, w.w as u16, w.h as u16)],
                };
                let mut rep = Rep::new(seq, 0).u32(rects.len() as u32).pad(20);
                for (x, y, ww, hh) in rects {
                    rep = rep.i16(x).i16(y).u16(ww).u16(hh);
                }
                self.reply(idx, rep);
            }
            // ---- RandR: the screen as one output on one CRTC in one mode ----
            Request::RandrQueryVersion(_) => self.reply(idx, Rep::new(seq, 0).u32(1).u32(5)),
            Request::RandrSelectInput(_) | Request::RandrSetCrtcGamma(_) | Request::RandrSetOutputPrimary(_) => {}
            Request::RandrGetScreenResources(_) | Request::RandrGetScreenResourcesCurrent(_) => {
                let (w, h) = self.screen();
                let name = b"LOOM-1";
                let rep = Rep::new(seq, 0).u32(0).u32(0).u16(1).u16(1).u16(1).u16(name.len() as u16).pad(8).u32(CRTC).u32(OUTPUT);
                let rep = mode_info(rep, w, h, name.len() as u16).bytes(name);
                self.reply(idx, rep);
            }
            Request::RandrGetOutputInfo(_) => {
                let (mw, mh) = self.screen_mm();
                let name = b"LOOM-1";
                let rep = Rep::new(seq, 0).u32(0).u32(CRTC).u32(mw as u32).u32(mh as u32).u8(0).u8(0).u16(1).u16(1).u16(1).u16(0).u16(name.len() as u16).u32(CRTC).u32(MODE).bytes(name);
                self.reply(idx, rep);
            }
            Request::RandrGetCrtcInfo(_) => {
                let (w, h) = self.screen();
                let rep = Rep::new(seq, 0).u32(0).i16(0).i16(0).u16(w).u16(h).u32(MODE).u16(1).u16(1).u16(1).u16(1).u32(OUTPUT).u32(OUTPUT);
                self.reply(idx, rep);
            }
            Request::RandrGetOutputPrimary(_) => self.reply(idx, Rep::new(seq, 0).u32(OUTPUT)),
            Request::RandrGetMonitors(_) => {
                let (w, h) = self.screen();
                let (mw, mh) = self.screen_mm();
                let name = self.intern(b"LOOM-1");
                let rep = Rep::new(seq, 0).u32(0).u32(1).u32(1).pad(12).u32(name).u8(1).u8(1).u16(1).i16(0).i16(0).u16(w).u16(h).u32(mw as u32).u32(mh as u32).u32(OUTPUT);
                self.reply(idx, rep);
            }
            Request::RandrGetScreenInfo(_) => {
                let (w, h) = self.screen();
                let (mw, mh) = self.screen_mm();
                // one size at 60 Hz: the size, then its rate list (count, rate)
                let rep = Rep::new(seq, 1).u32(ROOT).u32(0).u32(0).u16(1).u16(0).u16(1).u16(60).u16(2).pad(2).u16(w).u16(h).u16(mw).u16(mh).u16(1).u16(60);
                self.reply(idx, rep);
            }
            Request::RandrGetScreenSizeRange(_) => {
                let (w, h) = self.screen();
                self.reply(idx, Rep::new(seq, 0).u16(w).u16(h).u16(w).u16(h));
            }
            Request::RandrGetCrtcGammaSize(_) => self.reply(idx, Rep::new(seq, 0).u16(256)),
            Request::RandrGetCrtcGamma(_) => {
                // a straight ramp per channel (256 entries of red, then green, then blue)
                let mut rep = Rep::new(seq, 0).u16(256).pad(22);
                for _ in 0..3 {
                    for i in 0..256u32 {
                        rep = rep.u16((i * 257) as u16);
                    }
                }
                self.reply(idx, rep);
            }
            Request::RandrListOutputProperties(_) | Request::RandrListProviderProperties(_) => self.reply(idx, Rep::new(seq, 0).u16(0).pad(22)),
            Request::RandrGetOutputProperty(_) | Request::RandrGetProviderProperty(_) => self.reply(idx, Rep::new(seq, 0).u32(0).u32(0).u32(0).pad(12)),
            Request::RandrGetProviders(_) => self.reply(idx, Rep::new(seq, 0).u32(0).u16(0).pad(18)),
            // 36 bytes: timestamp, the panning area, the tracking area, the border
            Request::RandrGetPanning(_) => self.reply(idx, Rep::new(seq, 0).u32(0).pad(24)),
            Request::RandrGetCrtcTransform(_) => self.reply(idx, Rep::new(seq, 0).pad(80)),
            // ---- XFIXES ----
            Request::XfixesQueryVersion(_) => self.reply(idx, Rep::new(seq, 0).u32(5).u32(0)),
            Request::XfixesSelectSelectionInput(r) => {
                let (w, sel, mask) = (r.window, r.selection, u32::from(r.event_mask));
                self.fixes_sel.retain(|(c, ww, s, _)| !(*c == idx && *ww == w && *s == sel));
                if mask != 0 {
                    self.fixes_sel.push((idx, w, sel, mask));
                }
            }
            Request::XfixesSelectCursorInput(_)
            | Request::XfixesHideCursor(_)
            | Request::XfixesShowCursor(_)
            | Request::XfixesSetCursorName(_)
            | Request::XfixesChangeCursor(_)
            | Request::XfixesChangeCursorByName(_)
            | Request::XfixesCreateRegion(_)
            | Request::XfixesCreateRegionFromBitmap(_)
            | Request::XfixesCreateRegionFromWindow(_)
            | Request::XfixesCreateRegionFromGC(_)
            | Request::XfixesCreateRegionFromPicture(_)
            | Request::XfixesDestroyRegion(_)
            | Request::XfixesSetRegion(_)
            | Request::XfixesCopyRegion(_)
            | Request::XfixesUnionRegion(_)
            | Request::XfixesIntersectRegion(_)
            | Request::XfixesSubtractRegion(_)
            | Request::XfixesInvertRegion(_)
            | Request::XfixesTranslateRegion(_)
            | Request::XfixesRegionExtents(_)
            | Request::XfixesSetGCClipRegion(_)
            | Request::XfixesSetWindowShapeRegion(_)
            | Request::XfixesSetPictureClipRegion(_)
            | Request::XfixesChangeSaveSet(_)
            | Request::XfixesCreatePointerBarrier(_)
            | Request::XfixesDeletePointerBarrier(_) => {}
            Request::XfixesGetCursorImage(_) => self.reply(idx, Rep::new(seq, 0).pad(24)),
            other => {
                let other = match self.handle_shm(idx, seq, major, other) {
                    Ok(()) => return,
                    Err(o) => o,
                };
                let other = match self.handle_render(idx, seq, major, other) {
                    Ok(()) => return,
                    Err(o) => o,
                };
                let other = match self.handle_xi(idx, seq, major, other) {
                    Ok(()) => return,
                    Err(o) => o,
                };
                let other = match self.handle_gpu(idx, seq, major, other) {
                    Ok(()) => return,
                    Err(o) => o,
                };
                let other = match self.handle_glx(idx, seq, major, other) {
                    Ok(()) => return,
                    Err(o) => o,
                };
                if let Err(other) = self.handle_xkb(idx, seq, major, other) {
                    eprintln!("loom-x: extension request {major} not handled ({:?})", std::mem::discriminant(&other));
                    self.error(idx, 1, 0, major, 0);
                }
            }
        }
    }

    /// XFIXES selection notice to everyone watching `sel`: subtype 0 set, 1 window gone, 2 client gone.
    pub fn fixes_selection(&mut self, sel: u32, owner: u32, subtype: u8) {
        let time = self.now();
        let to: Vec<(usize, u32)> = self.fixes_sel.iter().filter(|(_, _, s, m)| *s == sel && m & (1 << subtype) != 0).map(|(c, w, _, _)| (*c, *w)).collect();
        for (c, w) in to {
            let mut ev = [0u8; 32];
            ev[0] = XFIXES_EVENT;
            ev[1] = subtype;
            ev[4..8].copy_from_slice(&w.to_le_bytes());
            ev[8..12].copy_from_slice(&owner.to_le_bytes());
            ev[12..16].copy_from_slice(&sel.to_le_bytes());
            ev[16..20].copy_from_slice(&time.to_le_bytes());
            ev[20..24].copy_from_slice(&time.to_le_bytes());
            self.send_event(c, ev);
        }
    }
}

/// RandR's one mode (32 bytes): the screen at 60 Hz.
fn mode_info(rep: Rep, w: u16, h: u16, name_len: u16) -> Rep {
    let (htotal, vtotal) = (w as u32 + 160, h as u32 + 35);
    let clock = htotal * vtotal * 60;
    rep.u32(MODE).u16(w).u16(h).u32(clock).u16(w + 48).u16(w + 80).u16(htotal as u16).u16(0).u16(h + 3).u16(h + 9).u16(vtotal as u16).u16(name_len).u32(0)
}


impl Server {
    /// A region the size of window `w`: rectangles (window coordinates), filled row by row.
    fn region_rects(&self, w: u32, rects: &[(i32, i32, i32, i32)]) -> Option<Shape> {
        let win = self.wins.get(&w)?;
        let (ww, wh) = (win.w as i32, win.h as i32);
        let mut s = Shape::full(ww as u32, wh as u32, false);
        for &(rx, ry, rw, rh) in rects {
            let (x0, x1) = (rx.max(0), (rx + rw).min(ww));
            if x0 >= x1 {
                continue;
            }
            for y in ry.max(0)..(ry + rh).min(wh) {
                let o = (y * ww) as usize;
                s.inside[o + x0 as usize..o + x1 as usize].fill(true);
            }
        }
        Some(s)
    }

    /// A region the size of window `w` from on/off pixels (`bw` x `bh`) placed at (ox, oy), copied row by row.
    fn region_bits(&self, w: u32, bits: &[bool], bw: u32, bh: u32, ox: i32, oy: i32) -> Option<Shape> {
        let win = self.wins.get(&w)?;
        let (ww, wh) = (win.w as i32, win.h as i32);
        let mut s = Shape::full(ww as u32, wh as u32, false);
        let (x0, x1) = (ox.max(0), (ox + bw as i32).min(ww));
        if x0 < x1 {
            for y in oy.max(0)..(oy + bh as i32).min(wh) {
                let src = ((y - oy) as u32 * bw) as usize + (x0 - ox) as usize;
                let dst = (y * ww) as usize + x0 as usize;
                let n = (x1 - x0) as usize;
                s.inside[dst..dst + n].copy_from_slice(&bits[src..src + n]);
            }
        }
        Some(s)
    }
}
