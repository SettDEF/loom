//! X drawing on pictures: graphics contexts and the core requests (lines, rectangles, arcs, polygons, images, copies), aliased as X draws.

use tiny_skia::{
    FillRule, Mask, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Rect, Stroke, Transform,
};

/// A graphics context: what X draws with.
#[derive(Clone)]
pub struct Gc {
    pub function: u8,
    pub fg: u32,
    pub bg: u32,
    pub line_width: u32,
    pub cap: u8,
    pub join: u8,
    pub fill_rule_winding: bool,
    pub arc_pie: bool,
    pub font: u32,
    pub exposures: bool,
    /// clip rectangles (relative to the clip origin), none = the whole drawable
    pub clip: Option<Vec<(i32, i32, u32, u32)>>,
    pub clip_origin: (i32, i32),
    /// Solid (0), Tiled (1), Stippled (2), OpaqueStippled (3), with the pixmaps and their origin
    pub fill_style: u8,
    pub tile: u32,
    pub stipple: u32,
    pub ts_origin: (i32, i32),
    /// the tile or stipple as a picture to repeat (made when the GC draws)
    pub pattern: Option<std::rc::Rc<Pixmap>>,
}

impl Default for Gc {
    fn default() -> Gc {
        Gc {
            function: 3,
            fg: 0,
            bg: 1,
            line_width: 0,
            cap: 1,
            join: 0,
            fill_rule_winding: false,
            arc_pie: true,
            font: 0,
            exposures: true,
            clip: None,
            clip_origin: (0, 0),
            fill_style: 0,
            tile: 0,
            stipple: 0,
            ts_origin: (0, 0),
            pattern: None,
        }
    }
}

/// Where a request draws: a picture, the drawable's origin in it, what it may touch (picture coordinates).
pub struct Target<'a> {
    pub img: &'a mut Pixmap,
    pub ox: i32,
    pub oy: i32,
    pub clip: (i32, i32, i32, i32),
    /// the drawable's depth: 1 (bitmap: 1 white, 0 black), 8 (alpha only), 24 (RGB), 32 (ARGB)
    pub depth: u8,
}

/// An X pixel value as a colour, by the drawable's depth.
pub fn color(pixel: u32, depth: u8) -> tiny_skia::Color {
    match depth {
        1 => {
            if pixel & 1 == 1 {
                tiny_skia::Color::WHITE
            } else {
                tiny_skia::Color::BLACK
            }
        }
        8 => tiny_skia::Color::from_rgba8(0, 0, 0, pixel as u8),
        32 => tiny_skia::Color::from_rgba8((pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8, (pixel >> 24) as u8),
        _ => tiny_skia::Color::from_rgba8((pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8, 255),
    }
}

/// A stored pixel back as an X pixel value.
pub fn pixel_of(c: PremultipliedColorU8, depth: u8) -> u32 {
    match depth {
        1 => (c.red() > 127) as u32,
        8 => c.alpha() as u32,
        32 => {
            let d = c.demultiply();
            (d.alpha() as u32) << 24 | (d.red() as u32) << 16 | (d.green() as u32) << 8 | d.blue() as u32
        }
        _ => (c.red() as u32) << 16 | (c.green() as u32) << 8 | c.blue() as u32,
    }
}

impl Target<'_> {
    /// The mask for the target's clip and the GC's clip rectangles (None: everything may be drawn).
    fn mask(&self, gc: &Gc) -> Option<Mask> {
        let (w, h) = (self.img.width() as i32, self.img.height() as i32);
        let full = self.clip == (0, 0, w, h) && gc.clip.is_none();
        if full {
            return None;
        }
        let mut m = Mask::new(w as u32, h as u32)?;
        let mut pb = PathBuilder::new();
        let (cx0, cy0, cx1, cy1) = self.clip;
        match &gc.clip {
            None => {
                if let Some(r) = Rect::from_ltrb(cx0 as f32, cy0 as f32, cx1 as f32, cy1 as f32) {
                    pb.push_rect(r);
                }
            }
            Some(rects) => {
                for &(x, y, rw, rh) in rects {
                    let x0 = (self.ox + gc.clip_origin.0 + x).max(cx0);
                    let y0 = (self.oy + gc.clip_origin.1 + y).max(cy0);
                    let x1 = (self.ox + gc.clip_origin.0 + x + rw as i32).min(cx1);
                    let y1 = (self.oy + gc.clip_origin.1 + y + rh as i32).min(cy1);
                    if let Some(r) = Rect::from_ltrb(x0 as f32, y0 as f32, x1 as f32, y1 as f32) {
                        pb.push_rect(r);
                    }
                }
            }
        }
        if let Some(p) = pb.finish() {
            m.fill_path(&p, FillRule::Winding, false, Transform::identity());
        }
        Some(m)
    }

    fn paint<'g>(&self, gc: &'g Gc, pixel: u32) -> Paint<'g> {
        let mut p = Paint::default();
        p.anti_alias = false;
        p.blend_mode = tiny_skia::BlendMode::Source;
        // a tile or stipple, repeated from the GC's origin (a stipple's gaps leave what is there)
        if let Some(pat) = gc.pattern.as_ref().filter(|_| pixel == gc.fg && gc.fill_style != 0) {
            p.shader = tiny_skia::Pattern::new(pat.as_ref().as_ref(), tiny_skia::SpreadMode::Repeat, tiny_skia::FilterQuality::Nearest, 1.0, Transform::from_translate(gc.ts_origin.0 as f32, gc.ts_origin.1 as f32));
            if gc.fill_style == 2 {
                p.blend_mode = tiny_skia::BlendMode::SourceOver;
            }
            return p;
        }
        // GXclear, GXset, GXcopy and the rest drawn as copy (no app here relies on raster ops)
        let pixel = match gc.function {
            0 => 0,
            15 => 0xffffff,
            _ => pixel,
        };
        p.set_color(color(pixel, self.depth));
        p
    }

    fn t(&self) -> Transform {
        Transform::from_translate(self.ox as f32, self.oy as f32)
    }

    pub fn fill_rects(&mut self, gc: &Gc, rects: &[(i32, i32, u32, u32)]) {
        let mask = self.mask(gc);
        let paint = self.paint(gc, gc.fg);
        let t = self.t();
        for &(x, y, w, h) in rects {
            if let Some(r) = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) {
                self.img.fill_rect(r, &paint, t, mask.as_ref());
            }
        }
    }

    /// A rectangle in a pixel value regardless of the GC (window backgrounds, ClearArea).
    pub fn clear(&mut self, rect: (i32, i32, u32, u32), pixel: u32) {
        let gc = Gc::default();
        let mask = self.mask(&gc);
        let paint = self.paint(&gc, pixel);
        let t = self.t();
        if let Some(r) = Rect::from_xywh(rect.0 as f32, rect.1 as f32, rect.2 as f32, rect.3 as f32)
        {
            self.img.fill_rect(r, &paint, t, mask.as_ref());
        }
    }

    fn stroke_of(gc: &Gc) -> Stroke {
        Stroke {
            width: gc.line_width.max(1) as f32,
            line_cap: match gc.cap {
                2 => tiny_skia::LineCap::Round,
                3 => tiny_skia::LineCap::Square,
                _ => tiny_skia::LineCap::Butt,
            },
            line_join: match gc.join {
                1 => tiny_skia::LineJoin::Round,
                2 => tiny_skia::LineJoin::Bevel,
                _ => tiny_skia::LineJoin::Miter,
            },
            ..Stroke::default()
        }
    }

    /// Connected lines (`closed`: back to the start), pixel centres as X has them.
    pub fn lines(&mut self, gc: &Gc, pts: &[(i32, i32)], closed: bool) {
        if pts.is_empty() {
            return;
        }
        let mask = self.mask(gc);
        let paint = self.paint(gc, gc.fg);
        let t = self.t();
        if pts.len() == 1 {
            if let Some(r) = Rect::from_xywh(pts[0].0 as f32, pts[0].1 as f32, 1.0, 1.0) {
                self.img.fill_rect(r, &paint, t, mask.as_ref());
            }
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0 as f32 + 0.5, pts[0].1 as f32 + 0.5);
        for p in &pts[1..] {
            pb.line_to(p.0 as f32 + 0.5, p.1 as f32 + 0.5);
        }
        if closed {
            pb.close();
        }
        if let Some(path) = pb.finish() {
            let mut s = Self::stroke_of(gc);
            // a thin line covers its last pixel too
            if gc.line_width == 0 && !closed {
                s.line_cap = tiny_skia::LineCap::Square;
            }
            self.img.stroke_path(&path, &paint, &s, t, mask.as_ref());
        }
    }

    pub fn segments(&mut self, gc: &Gc, segs: &[((i32, i32), (i32, i32))]) {
        for &(a, b) in segs {
            self.lines(gc, &[a, b], false);
        }
    }

    pub fn points(&mut self, gc: &Gc, pts: &[(i32, i32)]) {
        let rects: Vec<_> = pts.iter().map(|&(x, y)| (x, y, 1, 1)).collect();
        self.fill_rects(gc, &rects);
    }

    pub fn fill_poly(&mut self, gc: &Gc, pts: &[(i32, i32)]) {
        if pts.len() < 3 {
            return;
        }
        let mask = self.mask(gc);
        let paint = self.paint(gc, gc.fg);
        let t = self.t();
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0].0 as f32, pts[0].1 as f32);
        for p in &pts[1..] {
            pb.line_to(p.0 as f32, p.1 as f32);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            let rule = if gc.fill_rule_winding {
                FillRule::Winding
            } else {
                FillRule::EvenOdd
            };
            self.img.fill_path(&path, &paint, rule, t, mask.as_ref());
        }
    }

    /// An arc (x, y, w, h, start, extent in 64ths of a degree, counter-clockwise from 3 o'clock).
    pub fn arc(&mut self, gc: &Gc, a: (i32, i32, u32, u32, i32, i32), fill: bool) {
        let (x, y, w, h, a1, a2) = a;
        let (rx, ry) = (w as f32 / 2.0, h as f32 / 2.0);
        let (cx, cy) = (x as f32 + rx, y as f32 + ry);
        let full = a2.abs() >= 360 * 64;
        let mask = self.mask(gc);
        let paint = self.paint(gc, gc.fg);
        let t = self.t();
        let mut pb = PathBuilder::new();
        if full {
            if fill {
                if let Some(r) = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) {
                    pb.push_oval(r);
                }
            } else if let Some(r) =
                Rect::from_xywh(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32)
            {
                pb.push_oval(r);
            }
        } else {
            let (s, e) = (a1 as f32 / 64.0, (a1 + a2) as f32 / 64.0);
            let steps = ((a2.abs() as f32 / 64.0) / 4.0).ceil().max(4.0) as i32;
            let off = if fill { 0.0 } else { 0.5 };
            if fill && gc.arc_pie {
                pb.move_to(cx, cy);
            }
            for i in 0..=steps {
                let deg = s + (e - s) * i as f32 / steps as f32;
                let r = deg.to_radians();
                let (px, py) = (cx + rx * r.cos() + off, cy - ry * r.sin() + off);
                if i == 0 && !(fill && gc.arc_pie) {
                    pb.move_to(px, py);
                } else {
                    pb.line_to(px, py);
                }
            }
            if fill {
                pb.close();
            }
        }
        if let Some(path) = pb.finish() {
            if fill {
                self.img
                    .fill_path(&path, &paint, FillRule::Winding, t, mask.as_ref());
            } else {
                self.img
                    .stroke_path(&path, &paint, &Self::stroke_of(gc), t, mask.as_ref());
            }
        }
    }

    /// Pixels set one by one (images), inside the clip.
    pub fn put(
        &mut self,
        gc: &Gc,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        mut px: impl FnMut(u32, u32) -> Option<u32>,
    ) {
        let (iw, ih) = (self.img.width() as i32, self.img.height() as i32);
        let (cx0, cy0, cx1, cy1) = self.clip;
        let rects: Option<Vec<(i32, i32, i32, i32)>> = gc.clip.as_ref().map(|rs| {
            rs.iter()
                .map(|&(rx, ry, rw, rh)| {
                    (
                        self.ox + gc.clip_origin.0 + rx,
                        self.oy + gc.clip_origin.1 + ry,
                        self.ox + gc.clip_origin.0 + rx + rw as i32,
                        self.oy + gc.clip_origin.1 + ry + rh as i32,
                    )
                })
                .collect()
        });
        let depth = self.depth;
        let pixels = self.img.pixels_mut();
        for j in 0..h {
            let dy = self.oy + y + j as i32;
            if dy < cy0.max(0) || dy >= cy1.min(ih) {
                continue;
            }
            for i in 0..w {
                let dx = self.ox + x + i as i32;
                if dx < cx0.max(0) || dx >= cx1.min(iw) {
                    continue;
                }
                if let Some(rs) = &rects {
                    if !rs
                        .iter()
                        .any(|&(a, b, c, d)| dx >= a && dx < c && dy >= b && dy < d)
                    {
                        continue;
                    }
                }
                if let Some(p) = px(i, j) {
                    let c = color(p, depth).to_color_u8();
                    pixels[(dy * iw + dx) as usize] = c.premultiply();
                }
            }
        }
    }

    /// The pixel at drawable coordinates (images out, copies).
    pub fn get(&self, x: i32, y: i32) -> u32 {
        let (iw, ih) = (self.img.width() as i32, self.img.height() as i32);
        let (dx, dy) = (self.ox + x, self.oy + y);
        if dx < 0 || dy < 0 || dx >= iw || dy >= ih {
            return 0;
        }
        pixel_of(self.img.pixels()[(dy * iw + dx) as usize], self.depth)
    }
}

/// Image data (PutImage) as pixels: ZPixmap at 32 bits a pixel, or bitmaps (1 bit, LSB first, rows padded to 32 bits).
pub fn image_pixel(
    format: u8,
    depth: u8,
    data: &[u8],
    w: u32,
    left_pad: u8,
    gc: &Gc,
    i: u32,
    j: u32,
) -> Option<u32> {
    if format == 2 && depth == 8 {
        // a byte a pixel, rows padded to 32 bits
        return data.get((j * w.div_ceil(4) * 4 + i) as usize).map(|&b| b as u32);
    }
    if format == 2 && depth > 1 {
        let o = ((j * w + i) * 4) as usize;
        let b = data.get(o..o + 4)?;
        let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        return Some(if depth == 32 { v } else { v & 0xffffff });
    }
    let stride = (w + left_pad as u32).div_ceil(32) * 4;
    let bit = i + left_pad as u32;
    let byte = *data.get((j * stride + bit / 8) as usize)?;
    let on = byte >> (bit % 8) & 1 == 1;
    if format == 0 {
        // XYBitmap: ones in the foreground, zeros in the background
        Some(if on { gc.fg } else { gc.bg })
    } else {
        Some(on as u32)
    }
}

impl Target<'_> {
    /// A glyph's coverage blended in the foreground at pen (x, baseline y).
    pub fn glyph(&mut self, gc: &Gc, x: i32, y: i32, g: &crate::fonts::Glyph) {
        let (iw, ih) = (self.img.width() as i32, self.img.height() as i32);
        let (cx0, cy0, cx1, cy1) = self.clip;
        let rects: Option<Vec<(i32, i32, i32, i32)>> = gc.clip.as_ref().map(|rs| {
            rs.iter()
                .map(|&(rx, ry, rw, rh)| {
                    (
                        self.ox + gc.clip_origin.0 + rx,
                        self.oy + gc.clip_origin.1 + ry,
                        self.ox + gc.clip_origin.0 + rx + rw as i32,
                        self.oy + gc.clip_origin.1 + ry + rh as i32,
                    )
                })
                .collect()
        });
        let fg = color(gc.fg, self.depth).to_color_u8();
        let (x0, y0) = (self.ox + x + g.left, self.oy + y - g.top);
        let pixels = self.img.pixels_mut();
        for j in 0..g.h as i32 {
            let dy = y0 + j;
            if dy < cy0.max(0) || dy >= cy1.min(ih) {
                continue;
            }
            for i in 0..g.w as i32 {
                let dx = x0 + i;
                if dx < cx0.max(0) || dx >= cx1.min(iw) {
                    continue;
                }
                let a = g.alpha[(j * g.w as i32 + i) as usize] as u32;
                if a == 0 {
                    continue;
                }
                if let Some(rs) = &rects {
                    if !rs
                        .iter()
                        .any(|&(p, q, r, s)| dx >= p && dx < r && dy >= q && dy < s)
                    {
                        continue;
                    }
                }
                let o = &mut pixels[(dy * iw + dx) as usize];
                let mix = |f: u8, b: u8| ((f as u32 * a + b as u32 * (255 - a)) / 255) as u8;
                *o = PremultipliedColorU8::from_rgba(
                    mix(fg.red(), o.red()),
                    mix(fg.green(), o.green()),
                    mix(fg.blue(), o.blue()),
                    255,
                )
                .expect("opaque");
            }
        }
    }
}
