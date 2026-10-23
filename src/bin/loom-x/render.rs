//! RENDER: pictures, Porter-Duff compositing with masks, glyph sets (Xft text), trapezoids (cairo shapes), solid fills and gradients.

use std::collections::HashMap;

use x11rb_protocol::protocol::{render, Request};

use crate::{
    draw::Target,
    server::{Rep, Server, VISUAL},
};

// picture formats
const A8: u32 = 0x70;
const X8R8G8B8: u32 = 0x71;
const A8R8G8B8: u32 = 0x72;
const A1: u32 = 0x73;

/// Premultiplied colour, 0..1.
type Px = [f32; 4];

pub enum Pic {
    Drawable {
        d: u32,
        _depth: u8,
        repeat: bool,
        clip: Option<Vec<(i32, i32, u32, u32)>>,
        component_alpha: bool,
        /// destination to source (SetPictureTransform; None = identity), and the filter
        transform: Option<[f32; 9]>,
        bilinear: bool,
    },
    Solid(Px),
    Linear { p1: (f32, f32), p2: (f32, f32), stops: Vec<(f32, Px)> },
    Radial { c: (f32, f32), r1: f32, r2: f32, stops: Vec<(f32, Px)> },
}

pub struct GlyphImg {
    w: u32,
    h: u32,
    x: i16,
    y: i16,
    xoff: i16,
    yoff: i16,
    /// coverage per pixel (one value) or premultiplied colour (ARGB glyphs)
    px: Vec<Px>,
    /// coverage as bytes (alpha glyph sets), for the fast text path
    cov: std::rc::Rc<Vec<u8>>,
}

pub struct GlyphSet {
    depth: u8,
    glyphs: HashMap<u32, GlyphImg>,
}

#[derive(Default)]
pub struct Render {
    pub pics: HashMap<u32, Pic>,
    pub sets: HashMap<u32, GlyphSet>,
}

fn depth_of_format(f: u32) -> u8 {
    match f {
        A8 => 8,
        A8R8G8B8 => 32,
        A1 => 1,
        _ => 24,
    }
}

fn color(c: &render::Color) -> Px {
    let a = c.alpha as f32 / 65535.0;
    [c.red as f32 / 65535.0 * a, c.green as f32 / 65535.0 * a, c.blue as f32 / 65535.0 * a, a]
}

fn fixed(v: i32) -> f32 {
    v as f32 / 65536.0
}

/// A stored pixel as premultiplied colour, by the drawable's depth (24: opaque, 8: alpha only).
fn load(t: &Target, x: i32, y: i32) -> Option<Px> {
    let (iw, ih) = (t.img.width() as i32, t.img.height() as i32);
    let (dx, dy) = (t.ox + x, t.oy + y);
    let (cx0, cy0, cx1, cy1) = t.clip;
    if dx < cx0.max(0) || dy < cy0.max(0) || dx >= cx1.min(iw) || dy >= cy1.min(ih) {
        return None;
    }
    let c = t.img.pixels()[(dy * iw + dx) as usize];
    let f = |v: u8| v as f32 / 255.0;
    Some(match t.depth {
        8 => [0.0, 0.0, 0.0, f(c.alpha())],
        1 => {
            let a = if c.red() > 127 { 1.0 } else { 0.0 };
            [a, a, a, a]
        }
        32 => [f(c.red()), f(c.green()), f(c.blue()), f(c.alpha())],
        _ => [f(c.red()), f(c.green()), f(c.blue()), 1.0],
    })
}

fn store(t: &mut Target, x: i32, y: i32, p: Px) {
    let (iw, ih) = (t.img.width() as i32, t.img.height() as i32);
    let (dx, dy) = (t.ox + x, t.oy + y);
    let (cx0, cy0, cx1, cy1) = t.clip;
    if dx < cx0.max(0) || dy < cy0.max(0) || dx >= cx1.min(iw) || dy >= cy1.min(ih) {
        return;
    }
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let c = match t.depth {
        8 => tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, b(p[3])),
        1 => {
            let v = if p[3] > 0.5 { 255 } else { 0 };
            tiny_skia::PremultipliedColorU8::from_rgba(v, v, v, 255)
        }
        32 => {
            let a = b(p[3]);
            tiny_skia::PremultipliedColorU8::from_rgba(b(p[0]).min(a), b(p[1]).min(a), b(p[2]).min(a), a)
        }
        // no alpha to keep: what shows is the colour over black
        _ => tiny_skia::PremultipliedColorU8::from_rgba(b(p[0]), b(p[1]), b(p[2]), 255),
    };
    if let Some(c) = c {
        t.img.pixels_mut()[(dy * iw + dx) as usize] = c;
    }
}

/// Porter-Duff: source `s` onto destination `d` (premultiplied) by RENDER's operator.
fn blend(op: u8, s: Px, d: Px) -> Px {
    let (sa, da) = (s[3], d[3]);
    let (fa, fb) = match op {
        0 => (0.0, 0.0),
        1 => (1.0, 0.0),
        2 => (0.0, 1.0),
        3 => (1.0, 1.0 - sa),
        4 => (1.0 - da, 1.0),
        5 => (da, 0.0),
        6 => (0.0, sa),
        7 => (1.0 - da, 0.0),
        8 => (0.0, 1.0 - sa),
        9 => (da, 1.0 - sa),
        10 => (1.0 - da, sa),
        11 => (1.0 - da, 1.0 - sa),
        12 => (1.0, 1.0),
        13 => (if sa > 0.0 { ((1.0 - da) / sa).min(1.0) } else { 1.0 }, 1.0),
        _ => (1.0, 1.0 - sa),
    };
    let mut o = [0.0; 4];
    for i in 0..4 {
        o[i] = (s[i] * fa + d[i] * fb).min(1.0);
    }
    o
}

fn stop_at(stops: &[(f32, Px)], t: f32) -> Px {
    let t = t.clamp(0.0, 1.0);
    let Some(first) = stops.first() else { return [0.0; 4] };
    if t <= first.0 {
        return first.1;
    }
    for w in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (w[0], w[1]);
        if t <= t1 {
            let k = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            return [c0[0] + (c1[0] - c0[0]) * k, c0[1] + (c1[1] - c0[1]) * k, c0[2] + (c1[2] - c0[2]) * k, c0[3] + (c1[3] - c0[3]) * k];
        }
    }
    stops.last().unwrap().1
}

impl Server {
    /// A rectangle of picture `p` as premultiplied colours (outside it: transparent, unless it repeats).
    fn pic_read(&mut self, p: u32, x: i32, y: i32, w: u32, h: u32) -> Vec<Px> {
        let mut out = vec![[0.0; 4]; (w * h) as usize];
        match self.render.pics.get(&p) {
            Some(Pic::Solid(c)) => out.fill(*c),
            Some(Pic::Linear { p1, p2, stops }) => {
                let (dx, dy) = (p2.0 - p1.0, p2.1 - p1.1);
                let len = (dx * dx + dy * dy).max(1e-6);
                for j in 0..h {
                    for i in 0..w {
                        let (px, py) = ((x + i as i32) as f32 + 0.5 - p1.0, (y + j as i32) as f32 + 0.5 - p1.1);
                        out[(j * w + i) as usize] = stop_at(stops, (px * dx + py * dy) / len);
                    }
                }
            }
            Some(Pic::Radial { c, r1, r2, stops }) => {
                for j in 0..h {
                    for i in 0..w {
                        let (px, py) = ((x + i as i32) as f32 + 0.5 - c.0, (y + j as i32) as f32 + 0.5 - c.1);
                        let d = (px * px + py * py).sqrt();
                        out[(j * w + i) as usize] = stop_at(stops, (d - r1) / (r2 - r1).max(1e-6));
                    }
                }
            }
            Some(Pic::Drawable { d, repeat, transform, bilinear, .. }) => {
                let (d, repeat, transform, bilinear) = (*d, *repeat, *transform, *bilinear);
                let size = self.drawable_size(d);
                self.read(d, |t| {
                    // a pixel of the picture, wrapped when it repeats (outside: transparent)
                    let at = |sx: i32, sy: i32| -> Px {
                        let (mut sx, mut sy) = (sx, sy);
                        if repeat {
                            if let Some((pw, ph)) = size {
                                sx = sx.rem_euclid(pw as i32);
                                sy = sy.rem_euclid(ph as i32);
                            }
                        }
                        load(t, sx, sy).unwrap_or([0.0; 4])
                    };
                    for j in 0..h {
                        for i in 0..w {
                            let (dx, dy) = (x + i as i32, y + j as i32);
                            out[(j * w + i) as usize] = match transform {
                                None => at(dx, dy),
                                Some(m) => {
                                    // the pixel centre through the matrix (projective), back to a corner
                                    let (cx, cy) = (dx as f32 + 0.5, dy as f32 + 0.5);
                                    let wz = (m[6] * cx + m[7] * cy + m[8]).max(1e-6);
                                    let u = (m[0] * cx + m[1] * cy + m[2]) / wz - 0.5;
                                    let v = (m[3] * cx + m[4] * cy + m[5]) / wz - 0.5;
                                    if bilinear {
                                        let (u0, v0) = (u.floor(), v.floor());
                                        let (fu, fv) = (u - u0, v - v0);
                                        let (u0, v0) = (u0 as i32, v0 as i32);
                                        let (a, b, c, e) = (at(u0, v0), at(u0 + 1, v0), at(u0, v0 + 1), at(u0 + 1, v0 + 1));
                                        let mut p = [0.0; 4];
                                        for k in 0..4 {
                                            p[k] = (a[k] * (1.0 - fu) + b[k] * fu) * (1.0 - fv) + (c[k] * (1.0 - fu) + e[k] * fu) * fv;
                                        }
                                        p
                                    } else {
                                        at(u.round() as i32, v.round() as i32)
                                    }
                                }
                            };
                        }
                    }
                });
            }
            None => {}
        }
        out
    }

    fn drawable_size(&self, d: u32) -> Option<(u32, u32)> {
        self.pixmaps_size(d).or_else(|| self.wins.get(&d).map(|w| (w.w, w.h)))
    }

    /// Composites `src` (each pixel already through its mask) onto picture `dst` at (x, y).
    fn pic_write(&mut self, op: u8, dst: u32, x: i32, y: i32, w: u32, h: u32, src: &[Px], mask: Option<&[Px]>, component: bool) {
        let Some(Pic::Drawable { d, clip, .. }) = self.render.pics.get(&dst) else { return };
        let (d, clip) = (*d, clip.clone());
        self.draw(d, |t| {
            for j in 0..h {
                for i in 0..w {
                    let (px, py) = (x + i as i32, y + j as i32);
                    if let Some(rs) = &clip {
                        if !rs.iter().any(|&(rx, ry, rw, rh)| px >= rx && py >= ry && px < rx + rw as i32 && py < ry + rh as i32) {
                            continue;
                        }
                    }
                    let k = (j * w + i) as usize;
                    let mut s = src[k];
                    if let Some(m) = mask {
                        let m = m[k];
                        if component {
                            // per channel (subpixel text): alpha by the strongest
                            s = [s[0] * m[0], s[1] * m[1], s[2] * m[2], s[3] * m[0].max(m[1]).max(m[2])];
                        } else {
                            let a = m[3];
                            s = [s[0] * a, s[1] * a, s[2] * a, s[3] * a];
                        }
                    }
                    let Some(dv) = load(t, px, py) else { continue };
                    store(t, px, py, blend(op, s, dv));
                }
            }
        });
    }

    /// The RENDER requests; Err(req) when it is not one.
    pub fn handle_render<'a>(&mut self, idx: usize, seq: u16, major: u8, req: Request<'a>) -> Result<(), Request<'a>> {
        match req {
            Request::RenderQueryVersion(_) => self.reply(idx, Rep::new(seq, 0).u32(0).u32(11)),
            Request::RenderQueryPictFormats(_) => self.reply(idx, pict_formats(seq)),
            Request::RenderQueryPictIndexValues(_) => self.reply(idx, Rep::new(seq, 0).u32(0).pad(20)),
            Request::RenderQueryFilters(_) => {
                let names: [&[u8]; 5] = [b"nearest", b"bilinear", b"fast", b"good", b"best"];
                let mut rep = Rep::new(seq, 0).u32(3).u32(names.len() as u32).pad(16).u16(0).u16(1).u16(1);
                if names.len() % 2 == 0 {
                    rep = rep.pad(2);
                } else {
                    rep = rep.u16(0);
                }
                for n in names {
                    rep = rep.u8(n.len() as u8).bytes(n);
                }
                self.reply(idx, rep);
            }
            Request::RenderCreatePicture(r) => {
                let a = &r.value_list;
                let depth = depth_of_format(r.format);
                let pic = Pic::Drawable { d: r.drawable, _depth: depth, repeat: a.repeat.is_some_and(|v| u32::from(v) != 0), clip: None, component_alpha: a.componentalpha.is_some_and(|v| v != 0), transform: None, bilinear: false };
                self.render.pics.insert(r.pid, pic);
            }
            Request::RenderChangePicture(r) => {
                let a = &r.value_list;
                if let Some(Pic::Drawable { repeat, component_alpha, clip, .. }) = self.render.pics.get_mut(&r.picture) {
                    if let Some(v) = a.repeat {
                        *repeat = u32::from(v) != 0;
                    }
                    if let Some(v) = a.componentalpha {
                        *component_alpha = v != 0;
                    }
                    if a.clipmask == Some(0) {
                        *clip = None;
                    }
                }
            }
            Request::RenderSetPictureClipRectangles(r) => {
                if let Some(Pic::Drawable { clip, .. }) = self.render.pics.get_mut(&r.picture) {
                    let (ox, oy) = (r.clip_x_origin as i32, r.clip_y_origin as i32);
                    *clip = Some(r.rectangles.iter().map(|q| (q.x as i32 + ox, q.y as i32 + oy, q.width as u32, q.height as u32)).collect());
                }
            }
            Request::RenderFreePicture(r) => {
                self.render.pics.remove(&r.picture);
            }
            Request::RenderSetPictureTransform(r) => {
                let t = &r.transform;
                let m = [t.matrix11, t.matrix12, t.matrix13, t.matrix21, t.matrix22, t.matrix23, t.matrix31, t.matrix32, t.matrix33].map(fixed);
                let identity = m == [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
                if let Some(Pic::Drawable { transform, .. }) = self.render.pics.get_mut(&r.picture) {
                    *transform = (!identity).then_some(m);
                }
            }
            Request::RenderSetPictureFilter(r) => {
                let smooth = matches!(r.filter.as_ref(), b"bilinear" | b"good" | b"best" | b"convolution");
                if let Some(Pic::Drawable { bilinear, .. }) = self.render.pics.get_mut(&r.picture) {
                    *bilinear = smooth;
                }
            }
            Request::RenderCreateCursor(_) | Request::RenderCreateAnimCursor(_) | Request::RenderReferenceGlyphSet(_) => {}
            Request::RenderCreateSolidFill(r) => {
                self.render.pics.insert(r.picture, Pic::Solid(color(&r.color)));
            }
            Request::RenderCreateLinearGradient(r) => {
                let stops = r.stops.iter().zip(r.colors.iter()).map(|(s, c)| (fixed(*s), color(c))).collect();
                self.render.pics.insert(r.picture, Pic::Linear { p1: (fixed(r.p1.x), fixed(r.p1.y)), p2: (fixed(r.p2.x), fixed(r.p2.y)), stops });
            }
            Request::RenderCreateRadialGradient(r) => {
                let stops = r.stops.iter().zip(r.colors.iter()).map(|(s, c)| (fixed(*s), color(c))).collect();
                self.render.pics.insert(r.picture, Pic::Radial { c: (fixed(r.outer.x), fixed(r.outer.y)), r1: fixed(r.inner_radius), r2: fixed(r.outer_radius), stops });
            }
            Request::RenderCreateConicalGradient(r) => {
                let stops: Vec<(f32, Px)> = r.stops.iter().zip(r.colors.iter()).map(|(s, c)| (fixed(*s), color(c))).collect();
                self.render.pics.insert(r.picture, Pic::Solid(stops.first().map(|s| s.1).unwrap_or([0.0; 4])));
            }
            Request::RenderComposite(r) => {
                let (w, h) = (r.width as u32, r.height as u32);
                let src = self.pic_read(r.src, r.src_x as i32, r.src_y as i32, w, h);
                let component = matches!(self.render.pics.get(&r.mask), Some(Pic::Drawable { component_alpha: true, .. }));
                let mask = if r.mask != 0 { Some(self.pic_read(r.mask, r.mask_x as i32, r.mask_y as i32, w, h)) } else { None };
                self.pic_write(u8::from(r.op), r.dst, r.dst_x as i32, r.dst_y as i32, w, h, &src, mask.as_deref(), component);
            }
            Request::RenderFillRectangles(r) => {
                let c = color(&r.color);
                let rects: Vec<(i32, i32, u32, u32)> = r.rects.iter().map(|q| (q.x as i32, q.y as i32, q.width as u32, q.height as u32)).collect();
                if self.fill_fast(u8::from(r.op), r.dst, c, &rects) {
                    return Ok(());
                }
                for q in r.rects.iter() {
                    let (w, h) = (q.width as u32, q.height as u32);
                    let src = vec![c; (w * h) as usize];
                    self.pic_write(u8::from(r.op), r.dst, q.x as i32, q.y as i32, w, h, &src, None, false);
                }
            }
            Request::RenderCreateGlyphSet(r) => {
                self.render.sets.insert(r.gsid, GlyphSet { depth: depth_of_format(r.format), glyphs: HashMap::new() });
            }
            Request::RenderFreeGlyphSet(r) => {
                self.render.sets.remove(&r.glyphset);
            }
            Request::RenderAddGlyphs(r) => {
                let Some(set) = self.render.sets.get_mut(&r.glyphset) else { return Ok(self.error(idx, 137, r.glyphset, major, 20)) };
                let mut off = 0usize;
                for (id, g) in r.glyphids.iter().zip(r.glyphs.iter()) {
                    let (w, h) = (g.width as u32, g.height as u32);
                    let stride = match set.depth {
                        1 => w.div_ceil(32) * 4,
                        32 => w * 4,
                        _ => w.div_ceil(4) * 4,
                    } as usize;
                    let mut px = Vec::with_capacity((w * h) as usize);
                    for j in 0..h as usize {
                        for i in 0..w as usize {
                            let row = off + j * stride;
                            px.push(match set.depth {
                                1 => {
                                    let on = r.data.get(row + i / 8).is_some_and(|b| b >> (i % 8) & 1 == 1);
                                    let v = on as u32 as f32;
                                    [v, v, v, v]
                                }
                                32 => {
                                    let b = r.data.get(row + i * 4..row + i * 4 + 4).unwrap_or(&[0; 4]);
                                    let f = |v: u8| v as f32 / 255.0;
                                    [f(b[2]), f(b[1]), f(b[0]), f(b[3])]
                                }
                                _ => {
                                    let a = r.data.get(row + i).copied().unwrap_or(0) as f32 / 255.0;
                                    [a, a, a, a]
                                }
                            });
                        }
                    }
                    off += stride * h as usize;
                    let cov = std::rc::Rc::new(px.iter().map(|p| (p[3] * 255.0 + 0.5) as u8).collect());
                    set.glyphs.insert(*id, GlyphImg { w, h, x: g.x, y: g.y, xoff: g.x_off, yoff: g.y_off, px, cov });
                }
            }
            Request::RenderFreeGlyphs(r) => {
                if let Some(set) = self.render.sets.get_mut(&r.glyphset) {
                    for g in r.glyphs.iter() {
                        set.glyphs.remove(g);
                    }
                }
            }
            Request::RenderCompositeGlyphs8(r) => self.glyphs(u8::from(r.op), r.src, r.dst, r.glyphset, r.src_x, r.src_y, &r.glyphcmds, 1),
            Request::RenderCompositeGlyphs16(r) => self.glyphs(u8::from(r.op), r.src, r.dst, r.glyphset, r.src_x, r.src_y, &r.glyphcmds, 2),
            Request::RenderCompositeGlyphs32(r) => self.glyphs(u8::from(r.op), r.src, r.dst, r.glyphset, r.src_x, r.src_y, &r.glyphcmds, 4),
            Request::RenderTrapezoids(r) => {
                let polys: Vec<Vec<(f32, f32)>> = r
                    .traps
                    .iter()
                    .map(|t| {
                        let at = |l: &render::Linefix, y: f32| {
                            let (x1, y1, x2, y2) = (fixed(l.p1.x), fixed(l.p1.y), fixed(l.p2.x), fixed(l.p2.y));
                            if (y2 - y1).abs() < 1e-6 { x1 } else { x1 + (x2 - x1) * (y - y1) / (y2 - y1) }
                        };
                        let (top, bottom) = (fixed(t.top), fixed(t.bottom));
                        vec![(at(&t.left, top), top), (at(&t.right, top), top), (at(&t.right, bottom), bottom), (at(&t.left, bottom), bottom)]
                    })
                    .collect();
                self.shapes(u8::from(r.op), r.src, r.dst, r.src_x, r.src_y, &polys);
            }
            Request::RenderTriangles(r) => {
                let polys: Vec<Vec<(f32, f32)>> = r.triangles.iter().map(|t| vec![(fixed(t.p1.x), fixed(t.p1.y)), (fixed(t.p2.x), fixed(t.p2.y)), (fixed(t.p3.x), fixed(t.p3.y))]).collect();
                self.shapes(u8::from(r.op), r.src, r.dst, r.src_x, r.src_y, &polys);
            }
            other => return Err(other),
        }
        Ok(())
    }

    /// CompositeGlyphs: items of (count, dx, dy, glyph ids), or 255 and a new glyph set.
    fn glyphs(&mut self, op: u8, src: u32, dst: u32, mut set: u32, src_x: i16, src_y: i16, cmds: &[u8], size: usize) {
        if self.glyphs_fast(op, src, dst, set, cmds, size) {
            return;
        }
        let (mut pen_x, mut pen_y) = (0i32, 0i32);
        let mut first: Option<(i32, i32)> = None;
        let mut i = 0;
        while i + 8 <= cmds.len() {
            let n = cmds[i] as usize;
            let dx = i16::from_le_bytes([cmds[i + 4], cmds[i + 5]]) as i32;
            let dy = i16::from_le_bytes([cmds[i + 6], cmds[i + 7]]) as i32;
            i += 8;
            if n == 255 {
                if i + 4 > cmds.len() {
                    break;
                }
                set = u32::from_le_bytes([cmds[i], cmds[i + 1], cmds[i + 2], cmds[i + 3]]);
                i += 4;
                continue;
            }
            pen_x += dx;
            pen_y += dy;
            for k in 0..n {
                let o = i + k * size;
                let Some(b) = cmds.get(o..o + size) else { break };
                let id = match size {
                    1 => b[0] as u32,
                    2 => u16::from_le_bytes([b[0], b[1]]) as u32,
                    _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                };
                let f = *first.get_or_insert((pen_x, pen_y));
                let Some(g) = self.render.sets.get(&set).and_then(|s| s.glyphs.get(&id)) else { continue };
                let (gx, gy, w, h, xo, yo) = (pen_x - g.x as i32, pen_y - g.y as i32, g.w, g.h, g.xoff as i32, g.yoff as i32);
                let mask = g.px.clone();
                let component = self.render.sets.get(&set).is_some_and(|s| s.depth == 32);
                // the source lines up with the first glyph's origin at (src_x, src_y)
                let s = self.pic_read(src, src_x as i32 + gx - f.0, src_y as i32 + gy - f.1, w, h);
                self.pic_write(op, dst, gx, gy, w, h, &s, Some(&mask), component);
                pen_x += xo;
                pen_y += yo;
            }
            i += (n * size).div_ceil(4) * 4;
        }
    }

    /// Anti-aliased polygons (trapezoids, triangles) filled from `src` onto `dst`.
    fn shapes(&mut self, op: u8, src: u32, dst: u32, src_x: i16, src_y: i16, polys: &[Vec<(f32, f32)>]) {
        let pts = polys.iter().flatten();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        if x0 > x1 {
            return;
        }
        let (bx, by) = (x0.floor() as i32, y0.floor() as i32);
        let (w, h) = (((x1.ceil() as i32 - bx).max(1)) as u32, ((y1.ceil() as i32 - by).max(1)) as u32);
        let Some(mut m) = tiny_skia::Mask::new(w, h) else { return };
        let mut pb = tiny_skia::PathBuilder::new();
        for p in polys {
            if let Some(f) = p.first() {
                pb.move_to(f.0 - bx as f32, f.1 - by as f32);
                for q in &p[1..] {
                    pb.line_to(q.0 - bx as f32, q.1 - by as f32);
                }
                pb.close();
            }
        }
        let Some(path) = pb.finish() else { return };
        m.fill_path(&path, tiny_skia::FillRule::Winding, true, tiny_skia::Transform::identity());
        let mask: Vec<Px> = m.data().iter().map(|&a| {
            let a = a as f32 / 255.0;
            [a, a, a, a]
        }).collect();
        // the source lines up with the first point at (src_x, src_y)
        let f = polys.first().and_then(|p| p.first()).copied().unwrap_or((0.0, 0.0));
        let s = self.pic_read(src, src_x as i32 + bx - f.0.floor() as i32, src_y as i32 + by - f.1.floor() as i32, w, h);
        self.pic_write(op, dst, bx, by, w, h, &s, Some(&mask), false);
    }
}

/// QueryPictFormats: A8, X8R8G8B8 (the screen's visual), A8R8G8B8, A1; the screen's depths.
fn pict_formats(seq: u16) -> Rep {
    let formats: [(u32, u8, [u16; 8]); 4] = [
        (A8, 8, [0, 0, 0, 0, 0, 0, 0, 0xff]),
        (X8R8G8B8, 24, [16, 0xff, 8, 0xff, 0, 0xff, 0, 0]),
        (A8R8G8B8, 32, [16, 0xff, 8, 0xff, 0, 0xff, 24, 0xff]),
        (A1, 1, [0, 0, 0, 0, 0, 0, 0, 1]),
    ];
    // formats, screens, depths (1, 8, 24, 32), visuals (two), subpixel entries
    let mut rep = Rep::new(seq, 0).u32(formats.len() as u32).u32(1).u32(4).u32(2).u32(1).pad(4);
    for (id, depth, m) in formats {
        rep = rep.u32(id).u8(1).u8(depth).pad(2);
        for v in m {
            rep = rep.u16(v);
        }
        rep = rep.u32(0);
    }
    rep = rep.u32(4).u32(X8R8G8B8);
    for d in [1u8, 8] {
        rep = rep.u8(d).pad(1).u16(0).pad(4);
    }
    rep = rep.u8(24).pad(1).u16(1).pad(4).u32(VISUAL).u32(X8R8G8B8);
    rep = rep.u8(32).pad(1).u16(1).pad(4).u32(crate::server::VISUAL32).u32(A8R8G8B8);
    // subpixel order: unknown
    rep.u32(0)
}

/// Solid `s` (premultiplied, 0..255) over the pixel at index `k` with coverage `c` (0..255): integer Over.
#[inline]
fn over(px: &mut [tiny_skia::PremultipliedColorU8], k: usize, s: [u32; 4], c: u32, opaque_dst: bool) {
    let a = s[3] * c / 255;
    if a == 0 {
        return;
    }
    let d = px[k];
    let inv = 255 - a;
    let mix = |sv: u32, dv: u8| ((sv * c / 255) + dv as u32 * inv / 255).min(255) as u8;
    let da = if opaque_dst { 255 } else { (a + d.alpha() as u32 * inv / 255).min(255) as u8 };
    let (r, g, b) = (mix(s[0], d.red()).min(da), mix(s[1], d.green()).min(da), mix(s[2], d.blue()).min(da));
    if let Some(c) = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, da) {
        px[k] = c;
    }
}

impl Server {
    /// The drawable and clip behind a destination picture.
    fn dst_of(&self, dst: u32) -> Option<(u32, Option<Vec<(i32, i32, u32, u32)>>)> {
        match self.render.pics.get(&dst)? {
            Pic::Drawable { d, clip, .. } => Some((*d, clip.clone())),
            _ => None,
        }
    }

    /// Xft's text: a solid colour Over a picture through alpha glyphs, all glyphs in one pass. False: not that case.
    fn glyphs_fast(&mut self, op: u8, src: u32, dst: u32, set: u32, cmds: &[u8], size: usize) -> bool {
        let Some(Pic::Solid(c)) = self.render.pics.get(&src) else { return false };
        if op != 3 {
            return false;
        }
        let s = c.map(|v| (v * 255.0 + 0.5) as u32);
        let Some((d, clip)) = self.dst_of(dst) else { return false };
        // the glyphs and where they go (a set switch in the middle: the slow path)
        let Some(gs) = self.render.sets.get(&set) else { return true };
        if gs.depth == 32 {
            return false;
        }
        let mut place: Vec<(i32, i32, u32, u32, std::rc::Rc<Vec<u8>>)> = Vec::new();
        let (mut pen_x, mut pen_y) = (0i32, 0i32);
        let mut i = 0;
        while i + 8 <= cmds.len() {
            let n = cmds[i] as usize;
            if n == 255 {
                return false;
            }
            pen_x += i16::from_le_bytes([cmds[i + 4], cmds[i + 5]]) as i32;
            pen_y += i16::from_le_bytes([cmds[i + 6], cmds[i + 7]]) as i32;
            i += 8;
            for k in 0..n {
                let o = i + k * size;
                let Some(b) = cmds.get(o..o + size) else { break };
                let id = match size {
                    1 => b[0] as u32,
                    2 => u16::from_le_bytes([b[0], b[1]]) as u32,
                    _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                };
                if let Some(g) = gs.glyphs.get(&id) {
                    place.push((pen_x - g.x as i32, pen_y - g.y as i32, g.w, g.h, g.cov.clone()));
                    pen_x += g.xoff as i32;
                    pen_y += g.yoff as i32;
                }
            }
            i += (n * size).div_ceil(4) * 4;
        }
        self.draw(d, |t| {
            let (iw, ih) = (t.img.width() as i32, t.img.height() as i32);
            let (cx0, cy0, cx1, cy1) = (t.clip.0.max(0), t.clip.1.max(0), t.clip.2.min(iw), t.clip.3.min(ih));
            let opaque = t.depth != 32;
            let (ox, oy) = (t.ox, t.oy);
            let px = t.img.pixels_mut();
            for (gx, gy, w, h, cov) in &place {
                for j in 0..*h as i32 {
                    let y = oy + gy + j;
                    if y < cy0 || y >= cy1 {
                        continue;
                    }
                    for i in 0..*w as i32 {
                        let x = ox + gx + i;
                        if x < cx0 || x >= cx1 {
                            continue;
                        }
                        if let Some(rs) = &clip {
                            let (px_, py_) = (gx + i, gy + j);
                            if !rs.iter().any(|&(rx, ry, rw, rh)| px_ >= rx && py_ >= ry && px_ < rx + rw as i32 && py_ < ry + rh as i32) {
                                continue;
                            }
                        }
                        let c = cov[(j * *w as i32 + i) as usize] as u32;
                        if c != 0 {
                            over(px, (y * iw + x) as usize, s, c, opaque);
                        }
                    }
                }
            }
        });
        true
    }

    /// A solid colour into rectangles: Src (and an opaque Over) as a plain fill, Over with alpha blended. False: other ops.
    fn fill_fast(&mut self, op: u8, dst: u32, c: Px, rects: &[(i32, i32, u32, u32)]) -> bool {
        let plain = op == 1 || (op == 3 && c[3] >= 1.0);
        if !plain && op != 3 {
            return false;
        }
        let Some((d, clip)) = self.dst_of(dst) else { return false };
        let s = c.map(|v| (v * 255.0 + 0.5) as u32);
        self.draw(d, |t| {
            let (iw, ih) = (t.img.width() as i32, t.img.height() as i32);
            let (cx0, cy0, cx1, cy1) = (t.clip.0.max(0), t.clip.1.max(0), t.clip.2.min(iw), t.clip.3.min(ih));
            let opaque = t.depth != 32;
            let (ox, oy) = (t.ox, t.oy);
            let a = if opaque && plain { 255 } else { s[3] as u8 };
            let solid = tiny_skia::PremultipliedColorU8::from_rgba((s[0] as u8).min(a), (s[1] as u8).min(a), (s[2] as u8).min(a), a);
            let px = t.img.pixels_mut();
            // the spans to fill: each rect cut by the canvas, and by the clip rects when there are any
            let mut spans: Vec<(i32, i32, i32, i32)> = Vec::new();
            for &(rx, ry, rw, rh) in rects {
                let (x0, y0) = (rx, ry);
                let (x1, y1) = (rx + rw as i32, ry + rh as i32);
                match &clip {
                    None => spans.push((x0, y0, x1, y1)),
                    Some(rs) => {
                        for &(qx, qy, qw, qh) in rs {
                            spans.push((x0.max(qx), y0.max(qy), x1.min(qx + qw as i32), y1.min(qy + qh as i32)));
                        }
                    }
                }
            }
            // blending twice where clip rects overlap would be wrong: those go pixel by pixel
            let overlap = clip.as_ref().is_some_and(|rs| rs.len() > 1) && !(plain && solid.is_some());
            for &(x0, y0, x1, y1) in &spans {
                let (x0, x1) = ((ox + x0).max(cx0), (ox + x1).min(cx1));
                let (y0, y1) = ((oy + y0).max(cy0), (oy + y1).min(cy1));
                if x0 >= x1 || overlap {
                    continue;
                }
                for y in y0..y1 {
                    let row = (y * iw) as usize;
                    match (plain, solid) {
                        (true, Some(sc)) => px[row + x0 as usize..row + x1 as usize].fill(sc),
                        _ => {
                            for k in row + x0 as usize..row + x1 as usize {
                                over(px, k, s, 255, opaque);
                            }
                        }
                    }
                }
            }
            if overlap {
                for &(rx, ry, rw, rh) in rects {
                    for y in (oy + ry).max(cy0)..(oy + ry + rh as i32).min(cy1) {
                        for x in (ox + rx).max(cx0)..(ox + rx + rw as i32).min(cx1) {
                            let (px_, py_) = (x - ox, y - oy);
                            if clip.as_ref().is_some_and(|rs| !rs.iter().any(|&(qx, qy, qw, qh)| px_ >= qx && py_ >= qy && px_ < qx + qw as i32 && py_ < qy + qh as i32)) {
                                continue;
                            }
                            over(px, (y * iw + x) as usize, s, 255, opaque);
                        }
                    }
                }
            }
        });
        true
    }
}
