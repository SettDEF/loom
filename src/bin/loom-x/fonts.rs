//! X core fonts: any XLFD name opens the system's monospace, sans or serif font (as fontconfig picks them) at its pixel size, weight and slant; glyphs rasterized once with swash.

use std::{collections::HashMap, rc::Rc};

use swash::{
    FontRef,
    scale::{Render, ScaleContext, Source},
    zeno::Format,
};

pub struct Glyph {
    pub left: i32,
    pub top: i32,
    pub w: u32,
    pub h: u32,
    pub alpha: Vec<u8>,
}

/// What QueryFont tells about one character.
#[derive(Clone, Copy, Default)]
pub struct CharInfo {
    pub lsb: i16,
    pub rsb: i16,
    pub width: i16,
    pub ascent: i16,
    pub descent: i16,
}

pub struct XFont {
    pub name: String,
    pub ascent: i16,
    pub descent: i16,
    /// the widest advance (a cell, for monospace)
    pub advance: i16,
    pub chars: Vec<CharInfo>,
    pub glyphs: Vec<Option<Glyph>>,
}

impl XFont {
    /// Characters 0..=255 (Latin-1, as the core protocol's 8-bit text is)
    pub fn glyph(&self, c: u16) -> Option<&Glyph> {
        self.glyphs.get(c as usize).and_then(|g| g.as_ref())
    }
    /// how far the pen moves past `c`
    pub fn adv(&self, c: u16) -> i32 {
        self.chars.get(c as usize).map(|ci| ci.width as i32).unwrap_or(self.advance as i32)
    }
    pub fn width(&self, text: &[u16]) -> i32 {
        text.iter().map(|&c| self.adv(c)).sum()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Mono,
    Sans,
    Serif,
}

/// What an XLFD (or an alias like `fixed`, `9x15bold`) asks for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Want {
    pub px: u32,
    pub bold: bool,
    pub italic: bool,
    pub kind: Kind,
}

pub fn wanted(name: &str) -> Want {
    let n = name.to_ascii_lowercase();
    let mono = |px, bold| Want { px, bold, italic: false, kind: Kind::Mono };
    if let Some((_, h)) = n.split_once('x').filter(|(w, _)| w.parse::<u32>().is_ok()) {
        if let Ok(px) = h.trim_end_matches("bold").parse::<u32>() {
            return mono(px.clamp(6, 96), h.ends_with("bold"));
        }
    }
    let f: Vec<&str> = n.split('-').collect();
    if f.len() >= 9 && f[0].is_empty() {
        let family = f[2];
        let spacing = f.get(11).copied().unwrap_or("*");
        let kind = if ["fixed", "courier", "terminal", "mono", "console", "typewriter", "clean"].iter().any(|k| family.contains(k)) || spacing == "m" || spacing == "c" {
            Kind::Mono
        } else if ["times", "serif", "schoolbook", "charter", "utopia", "bookman", "palatino", "georgia"].iter().any(|k| family.contains(k)) && !family.contains("sans") {
            Kind::Serif
        } else if family == "*" && spacing == "*" {
            // unspecified: what `fixed` would be
            Kind::Mono
        } else {
            Kind::Sans
        };
        let bold = matches!(f[3], "bold" | "demibold" | "black" | "heavy" | "semibold");
        let italic = matches!(f[4], "i" | "o");
        let px = f[7].parse::<u32>().ok().filter(|v| *v > 0).or_else(|| f[8].parse::<u32>().ok().filter(|v| *v > 0).map(|pt| (pt as f32 / 10.0 * 96.0 / 72.0).round() as u32));
        return Want { px: px.unwrap_or(13).clamp(6, 96), bold, italic, kind };
    }
    mono(13, n.contains("bold"))
}

pub struct Fonts {
    /// font files read so far: (family kind, bold, italic) -> (bytes, face index)
    files: HashMap<(Kind, bool, bool), std::rc::Rc<(Vec<u8>, u32)>>,
    warm: Option<std::thread::JoinHandle<Warm>>,
    ctx: ScaleContext,
    by_want: HashMap<Want, Rc<XFont>>,
    pub open: HashMap<u32, Rc<XFont>>,
    scale: f64,
}

impl Fonts {
    /// `scale`: X pixels per logical pixel; core font sizes (meant for a 96 dpi screen) grow by it
    pub fn new(scale: f64) -> Fonts {
        // nothing scanned at start: each font is found through fontconfig when first asked for
        // the fonts nearly every program asks for, looked up while the first one connects
        let warm = std::thread::spawn(|| {
            let mut out = Vec::new();
            for key in [(Kind::Mono, false, false), (Kind::Mono, true, false), (Kind::Sans, false, false)] {
                if let Some(f) = find_file(key.0, key.1, key.2) {
                    out.push((key, f));
                }
            }
            out
        });
        Fonts { files: HashMap::new(), warm: Some(warm), ctx: ScaleContext::new(), by_want: HashMap::new(), open: HashMap::new(), scale }
    }

    /// The font file (and face index) fontconfig picks for a family, weight and slant, read once.
    fn file_for(&mut self, kind: Kind, bold: bool, italic: bool) -> Option<std::rc::Rc<(Vec<u8>, u32)>> {
        if let Some(f) = self.files.get(&(kind, bold, italic)) {
            return Some(f.clone());
        }
        // the ones looked up in the background at start
        if let Some(h) = self.warm.take() {
            for (key, file) in h.join().unwrap_or_default() {
                self.files.insert(key, std::rc::Rc::new(file));
            }
            if let Some(f) = self.files.get(&(kind, bold, italic)) {
                return Some(f.clone());
            }
        }
        let f = std::rc::Rc::new(find_file(kind, bold, italic)?);
        self.files.insert((kind, bold, italic), f.clone());
        Some(f)
    }

    /// The font for XLFD `name` (loaded once per size, weight, slant and family).
    pub fn load(&mut self, name: &str) -> Option<Rc<XFont>> {
        let mut w = wanted(name);
        let asked = w.px;
        w.px = ((w.px as f64 * self.scale).round() as u32).max(6);
        if let Some(f) = self.by_want.get(&w) {
            return Some(f.clone());
        }
        let file = self.file_for(w.kind, w.bold, w.italic)?;
        let font = raster(&mut self.ctx, &file.0, file.1 as usize, w, asked)?;
        let f = Rc::new(font);
        self.by_want.insert(w, f.clone());
        Some(f)
    }
}

fn raster(ctx: &mut ScaleContext, data: &[u8], index: usize, w: Want, asked: u32) -> Option<XFont> {
    let font = FontRef::from_index(data, index)?;
    let size = w.px as f32;
    let m = font.metrics(&[]).scale(size);
    let gm = font.glyph_metrics(&[]).scale(size);
    // descent as a distance below the baseline (fonts store it negative)
    let (ascent, descent) = (m.ascent.ceil() as i16, m.descent.abs().ceil() as i16);
    let mut chars = vec![CharInfo::default(); 256];
    let mut glyphs: Vec<Option<Glyph>> = (0..256).map(|_| None).collect();
    let mono = w.kind == Kind::Mono;
    let cell = gm.advance_width(font.charmap().map('M')).round().max(1.0) as i16;
    // hinting is the slow part: the 256 glyphs go to a few threads, each with its own scaler
    let _ = ctx;
    let parts = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(1, 8));
    let per = 256usize.div_ceil(parts);
    std::thread::scope(|sc| {
        for (k, (cs, gs)) in chars.chunks_mut(per).zip(glyphs.chunks_mut(per)).enumerate() {
            sc.spawn(move || {
                let mut ctx = ScaleContext::new();
                let mut scaler = ctx.builder(font).size(size).hint(true).build();
                for (j, (ci, g)) in cs.iter_mut().zip(gs.iter_mut()).enumerate() {
                    let c = (k * per + j) as u32;
                    let ch = char::from_u32(c).filter(|ch| !ch.is_control());
                    let gid = ch.map(|ch| font.charmap().map(ch)).unwrap_or(0);
                    // monospace keeps one cell for everything; others advance by each glyph
                    let adv = if mono || ch.is_none() { cell } else { gm.advance_width(gid).round() as i16 };
                    let img = ch.and_then(|_| Render::new(&[Source::Outline]).format(Format::Alpha).render(&mut scaler, gid));
                    match img {
                        Some(i) if i.placement.width > 0 => {
                            let p = i.placement;
                            *ci = CharInfo { lsb: p.left as i16, rsb: (p.left + p.width as i32) as i16, width: adv, ascent: p.top as i16, descent: (p.height as i32 - p.top) as i16 };
                            *g = Some(Glyph { left: p.left, top: p.top, w: p.width, h: p.height, alpha: i.data });
                        }
                        // blank (space) or not in the font: just its advance
                        _ => *ci = CharInfo { width: adv, ..Default::default() },
                    }
                }
            });
        }
    });
    let advance = chars.iter().map(|c| c.width).max().unwrap_or(cell);
    let avg = chars[32..127].iter().map(|c| c.width as u32).sum::<u32>() * 10 / 95;
    let pt = asked * 10 * 72 / 96;
    let (foundry, family, spacing) = match w.kind {
        Kind::Mono => ("misc", "fixed", "c"),
        Kind::Sans => ("loom", "sans", "p"),
        Kind::Serif => ("loom", "serif", "p"),
    };
    let name = format!("-{foundry}-{family}-{}-{}-normal--{}-{pt}-96-96-{spacing}-{avg}-iso10646-1", if w.bold { "bold" } else { "medium" }, if w.italic { "i" } else { "r" }, asked);
    Some(XFont { name, ascent, descent, advance, chars, glyphs })
}

/// The font file fontconfig picks for a family, weight and slant: its bytes and face index.
fn find_file(kind: Kind, bold: bool, italic: bool) -> Option<(Vec<u8>, u32)> {
    let family = match kind {
        Kind::Mono => "monospace",
        Kind::Sans => "sans-serif",
        Kind::Serif => "serif",
    };
    let pattern = format!("{family}:weight={}:slant={}", if bold { 200 } else { 80 }, if italic { 100 } else { 0 });
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}\n%{index}", &pattern]).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines();
    let path = lines.next().filter(|p| !p.is_empty())?;
    let index = lines.next().and_then(|i| i.parse().ok()).unwrap_or(0);
    Some((std::fs::read(path).ok()?, index))
}

type Warm = Vec<((Kind, bool, bool), (Vec<u8>, u32))>;
