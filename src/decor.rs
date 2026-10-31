//! The user's ii-decor title bars, native: the same ~/.config/hypr/ii-decor.json (and the
//! ii-decor.d/*.json integrations) the Hyprland plugin reads, drawn by loom. A bar above the window
//! (its colour, the app icon and title centred, the dots: close outermost), or for an integrated app
//! a pill of buttons over a corner of its own content, or nothing (excluded classes, fullscreen).
//! Buttons act on release over the same dot; the bar drags the window; a double click maximizes.
//! Each bar is drawn once (text by cosmic-text with the user's font, shapes by tiny-skia) and
//! cached until its size, title, hover or colours change.

use std::{collections::HashMap, path::PathBuf};

use cosmic_text::{Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use regex::Regex;
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Transform as SkTransform};

pub type Rgba = [f32; 4];

/// A pill of buttons over a corner of an app's own content (ii-decor.d "overlay").
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub right: bool,
    pub top: bool,
    pub x: f32,
    pub y: f32,
    pub height: f32,
    pub padding: f32,
    pub background: Option<Rgba>,
}

impl Default for Overlay {
    fn default() -> Self {
        Overlay { right: true, top: true, x: 6.0, y: 4.0, height: 22.0, padding: 8.0, background: None }
    }
}

#[derive(Clone, Debug, Default)]
struct ClassOv {
    bar: Option<Rgba>,
    text: Option<Rgba>,
    height: Option<f32>,
    title: Option<bool>,
}

/// What a window gets.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    None,
    Bar(f32),
    Overlay(Overlay),
}

pub struct Conf {
    enabled: bool,
    include: bool,
    classes: Vec<Regex>,
    /// ii-decor.d: classes that draw their own title bar, and the pill they may want
    integrated: Vec<(Regex, Option<Overlay>)>,
    pub height: f32,
    button_size: f32,
    button_gap: f32,
    padding: f32,
    title: bool,
    title_size: f32,
    title_weight: u16,
    title_font: String,
    pub double_click: bool,
    /// glass: the window behind the bar blurred (ii-decor's `blur`)
    pub blur: bool,
    pub minimize_to: String,
    icon: bool,
    icon_size: f32,
    icons: HashMap<String, PathBuf>,
    per_class: HashMap<String, ClassOv>,
    hover_darken: f32,
    separator: Rgba,
    bar: Rgba,
    text: Rgba,
    close: Rgba,
    minimize: Rgba,
    maximize: Rgba,
    left: Vec<String>,
    right: Vec<String>,
}

/// `rgba(RRGGBBAA)`, `rgb(RRGGBB)`, `#RRGGBB[AA]`, `0xAARRGGBB`, `rgba(r, g, b, a)`.
pub fn color(s: &str) -> Option<Rgba> {
    let s = s.trim();
    let hex = |h: &str| -> Option<Rgba> {
        let v = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|b| b as f32 / 255.0);
        match h.len() {
            6 => Some([v(0)?, v(2)?, v(4)?, 1.0]),
            8 => Some([v(0)?, v(2)?, v(4)?, v(6)?]),
            _ => None,
        }
    };
    if let Some(h) = s.strip_prefix('#') {
        return hex(h);
    }
    if let Some(h) = s.strip_prefix("0x") {
        let c = hex(h)?;
        return Some([c[1], c[2], c[3], c[0]]);
    }
    let inner = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb("))?.strip_suffix(')')?;
    if inner.contains(',') {
        let p: Vec<f32> = inner.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        return match p.as_slice() {
            [r, g, b] => Some([r / 255.0, g / 255.0, b / 255.0, 1.0]),
            [r, g, b, a] => Some([r / 255.0, g / 255.0, b / 255.0, *a]),
            _ => None,
        };
    }
    hex(inner)
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn conf_path() -> PathBuf {
    std::env::var_os("IIDECOR_CONFIG").map(PathBuf::from).unwrap_or_else(|| home().join(".config/hypr/ii-decor.json"))
}

impl Conf {
    /// ii-decor.json and ii-decor.d/; the plugin's defaults for whatever is missing.
    pub fn load() -> Conf {
        let v: serde_json::Value = std::fs::read_to_string(conf_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let num = |k: &str, d: f32| v[k].as_f64().map(|x| x as f32).unwrap_or(d);
        let flag = |k: &str, d: bool| v[k].as_bool().unwrap_or(d);
        let col = |k: &str, d: Rgba| v["colors"][k].as_str().and_then(color).unwrap_or(d);
        let left_side = v["buttons"].as_str() == Some("left");
        let list = |k: &str| v["layout"][k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>());
        let (left, right) = match (list("left"), list("right")) {
            (None, None) if left_side => (vec!["close".into(), "minimize".into(), "maximize".into()], vec![]),
            (None, None) => (vec![], vec!["minimize".into(), "maximize".into(), "close".into()]),
            (l, r) => (l.unwrap_or_default(), r.unwrap_or_default()),
        };
        let mut integrated = Vec::new();
        if let Ok(rd) = std::fs::read_dir(home().join(".config/hypr/ii-decor.d")) {
            let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
            files.sort();
            for f in files {
                let Ok(j) = std::fs::read_to_string(&f).map(|t| serde_json::from_str::<serde_json::Value>(&t).unwrap_or_default()) else { continue };
                let ov = match &j["overlay"] {
                    serde_json::Value::Bool(true) => Some(Overlay::default()),
                    serde_json::Value::Object(m) => {
                        let d = Overlay::default();
                        let c = m.get("corner").and_then(|c| c.as_str()).unwrap_or("top-right");
                        let n = |k: &str, d: f32| m.get(k).and_then(|x| x.as_f64()).map(|x| x as f32).unwrap_or(d);
                        Some(Overlay { right: !c.ends_with("left"), top: c.starts_with("top"), x: n("x", d.x), y: n("y", d.y), height: n("height", d.height), padding: n("padding", d.padding), background: m.get("background").and_then(|b| b.as_str()).and_then(color) })
                    }
                    _ => None,
                };
                for re in j["classes"].as_array().into_iter().flatten().filter_map(|c| c.as_str()).filter_map(|c| Regex::new(c).ok()) {
                    integrated.push((re, ov.clone()));
                }
            }
        }
        let per_class = v["perClass"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, o)| (k.clone(), ClassOv { bar: o["bar"].as_str().and_then(color), text: o["text"].as_str().and_then(color), height: o["height"].as_f64().map(|h| h as f32), title: o["title"].as_bool() }))
                    .collect()
            })
            .unwrap_or_default();
        Conf {
            enabled: flag("enabled", true),
            include: v["mode"].as_str() == Some("include"),
            classes: v["classes"].as_array().into_iter().flatten().filter_map(|c| c.as_str()).filter_map(|c| Regex::new(c).ok()).collect(),
            integrated,
            height: num("height", 28.0),
            button_size: num("buttonSize", 12.0),
            button_gap: num("buttonGap", 8.0),
            padding: num("padding", 10.0),
            title: flag("title", true),
            title_size: num("titleSize", 11.0),
            title_weight: num("titleWeight", 500.0) as u16,
            title_font: v["titleFont"].as_str().unwrap_or("sans-serif").to_string(),
            double_click: flag("doubleClick", true),
            blur: flag("blur", false),
            minimize_to: v["minimizeTo"].as_str().unwrap_or("special").to_string(),
            icon: flag("icon", true),
            icon_size: num("iconSize", 16.0),
            icons: v["icons"].as_object().map(|m| m.iter().filter_map(|(k, p)| Some((k.clone(), PathBuf::from(p.as_str()?)))).collect()).unwrap_or_default(),
            per_class,
            hover_darken: num("hoverDarken", 0.25),
            separator: v["separator"].as_str().and_then(color).unwrap_or([0.0; 4]),
            bar: col("bar", [0.1, 0.1, 0.12, 0.85]),
            text: col("text", [0.88, 0.89, 0.93, 1.0]),
            close: col("close", [1.0, 0.71, 0.67, 1.0]),
            minimize: col("minimize", [0.93, 0.69, 1.0, 1.0]),
            maximize: col("maximize", [0.41, 0.81, 0.64, 1.0]),
            left,
            right,
        }
    }

    /// What a window of `class` gets (fullscreen windows nothing).
    pub fn kind(&self, class: &str, fullscreen: bool) -> Kind {
        if !self.enabled || fullscreen {
            return Kind::None;
        }
        if let Some((_, ov)) = self.integrated.iter().find(|(re, _)| re.is_match(class)) {
            return ov.clone().map(Kind::Overlay).unwrap_or(Kind::None);
        }
        let matched = self.classes.iter().any(|re| re.is_match(class));
        if matched != self.include {
            return Kind::None;
        }
        let h = self.per_class.get(class).and_then(|o| o.height).unwrap_or(self.height);
        if h >= 1.0 { Kind::Bar(h) } else { Kind::None }
    }

    fn button_color(&self, id: &str) -> Rgba {
        match id {
            "close" => self.close,
            "minimize" => self.minimize,
            _ => self.maximize,
        }
    }
}

/// A button's place in its bar or pill (logical px from the top-left).
#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub size: f32,
}

/// The buttons of a bar `width` wide (each group from its edge inwards, stopping at the middle),
/// or of a pill (one row, left to right).
pub fn layout(c: &Conf, width: f32, height: f32, overlay: Option<&Overlay>) -> Vec<Button> {
    let known = |id: &str| matches!(id, "close" | "minimize" | "maximize");
    if let Some(ov) = overlay {
        let size = c.button_size.min(ov.height);
        let mut x = ov.padding;
        let mut out = Vec::new();
        for id in c.left.iter().chain(c.right.iter()).filter(|i| known(i)) {
            out.push(Button { id: id.clone(), x: x.floor(), y: ((ov.height - size) / 2.0).floor(), size });
            x += size + c.button_gap;
        }
        return out;
    }
    let mut out = Vec::new();
    let mut place = |ids: &[String], right: bool| {
        let mut off = c.padding;
        let n = ids.len();
        for i in 0..n {
            let id = if right { &ids[n - 1 - i] } else { &ids[i] };
            if !known(id) {
                continue;
            }
            let size = c.button_size;
            if off + size > width / 2.0 {
                break;
            }
            let x = if right { width - off - size } else { off };
            out.push(Button { id: id.clone(), x: x.floor(), y: ((height - size) / 2.0).floor(), size });
            off += size + c.button_gap;
        }
    };
    place(&c.left, false);
    place(&c.right, true);
    out
}

/// The width of a pill.
pub fn pill_width(c: &Conf, ov: &Overlay) -> f32 {
    layout(c, 0.0, ov.height, Some(ov)).last().map(|b| b.x + b.size + ov.padding).unwrap_or(0.0)
}

/// Everything a drawn bar depends on: the same key, the cached picture.
#[derive(Clone, PartialEq)]
struct Key {
    w: i32,
    h: i32,
    scale_milli: i32,
    title: String,
    class: String,
    hover: Option<String>,
    radius_milli: i32,
    generation: u64,
}

pub struct Decor {
    pub conf: Conf,
    pub generation: u64,
    fonts: Option<FontSystem>,
    swash: SwashCache,
    cache: HashMap<u64, (Key, MemoryRenderBuffer)>,
    icons: HashMap<String, Option<Pixmap>>,
    /// a press on a button (window address, id), and the last title click (for a double click)
    pub pressed: Option<(u64, String)>,
    pub last_click: Option<(u64, u32)>,
    /// the button under the pointer
    pub hover: Option<(u64, String)>,
}

impl Default for Decor {
    fn default() -> Self {
        Decor { conf: Conf::load(), generation: 1, fonts: None, swash: SwashCache::new(), cache: HashMap::new(), icons: HashMap::new(), pressed: None, last_click: None, hover: None }
    }
}

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, (c[3] * 255.0) as u8);
    p.anti_alias = true;
    p
}

/// A rectangle with its top corners (or all, `all`) rounded by `r`.
fn rounded(w: f32, h: f32, r: f32, all: bool) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / if all { 2.0 } else { 1.0 }).max(0.0);
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, r);
    pb.cubic_to(0.0, r - k, r - k, 0.0, r, 0.0);
    pb.line_to(w - r, 0.0);
    pb.cubic_to(w - r + k, 0.0, w, r - k, w, r);
    if all {
        pb.line_to(w, h - r);
        pb.cubic_to(w, h - r + k, w - r + k, h, w - r, h);
        pb.line_to(r, h);
        pb.cubic_to(r - k, h, 0.0, h - r + k, 0.0, h - r);
    } else {
        pb.line_to(w, h);
        pb.line_to(0.0, h);
    }
    pb.close();
    pb.finish()
}

fn darken(c: Rgba, by: f32) -> Rgba {
    [c[0] * (1.0 - by), c[1] * (1.0 - by), c[2] * (1.0 - by), c[3]]
}

impl Decor {
    pub fn reload(&mut self) {
        self.conf = Conf::load();
        self.generation += 1;
        self.icons.clear();
    }

    fn icon(&mut self, class: &str) -> Option<Pixmap> {
        if let Some(p) = self.icons.get(class) {
            return p.clone();
        }
        let path = self.conf.icons.get(class).cloned().unwrap_or_else(|| home().join(format!(".cache/ii-decor/icons/{class}.png")));
        let pix = Pixmap::load_png(&path).ok();
        self.icons.insert(class.to_string(), pix.clone());
        pix
    }

    /// The bar (or pill) of window `addr`, drawn at `scale`, from the cache when nothing changed.
    #[allow(clippy::too_many_arguments)]
    pub fn picture(&mut self, addr: u64, class: &str, title: &str, kind: &Kind, width: f32, scale: f64, radius: f32) -> Option<&MemoryRenderBuffer> {
        let (w, h, ov) = match kind {
            Kind::None => return None,
            Kind::Bar(h) => (width, *h, None),
            Kind::Overlay(o) => (pill_width(&self.conf, o), o.height, Some(o.clone())),
        };
        let hover = self.hover.as_ref().filter(|(a, _)| *a == addr).map(|(_, id)| id.clone());
        let key = Key { w: (w * scale as f32).round() as i32, h: (h * scale as f32).round() as i32, scale_milli: (scale * 1000.0) as i32, title: title.to_string(), class: class.to_string(), hover: hover.clone(), radius_milli: (radius * 1000.0) as i32, generation: self.generation };
        if self.cache.get(&addr).is_none_or(|(k, _)| *k != key) {
            let pix = self.draw(class, title, w, h, scale as f32, radius, ov.as_ref(), hover.as_deref())?;
            // tiny-skia: premultiplied RGBA in memory, which is DRM's Abgr8888
            let buf = MemoryRenderBuffer::from_slice(pix.data(), Fourcc::Abgr8888, (pix.width() as i32, pix.height() as i32), 1, Transform::Normal, None);
            self.cache.insert(addr, (key, buf));
        }
        self.cache.get(&addr).map(|(_, b)| b)
    }

    pub fn forget(&mut self, addr: u64) {
        self.cache.remove(&addr);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(&mut self, class: &str, title: &str, w: f32, h: f32, s: f32, radius: f32, ov: Option<&Overlay>, hover: Option<&str>) -> Option<Pixmap> {
        let (pw, ph) = ((w * s).round().max(1.0) as u32, (h * s).round().max(1.0) as u32);
        let mut pix = Pixmap::new(pw, ph)?;
        let pc = self.conf.per_class.get(class).cloned().unwrap_or_default();
        let bar = pc.bar.unwrap_or(self.conf.bar);
        // the background: the bar with its top corners as round as the window's, or the pill
        let bg = match ov {
            Some(o) => o.background.unwrap_or(bar),
            None => bar,
        };
        let shape = if ov.is_some() { rounded(pw as f32, ph as f32, ph as f32 / 2.0, true) } else { rounded(pw as f32, ph as f32, radius * s, false) };
        if let Some(p) = shape {
            pix.fill_path(&p, &paint(bg), FillRule::Winding, SkTransform::identity(), None);
        }
        if ov.is_none() && self.conf.separator[3] > 0.0 {
            if let Some(r) = Rect::from_xywh(0.0, ph as f32 - s.max(1.0), pw as f32, s.max(1.0)) {
                pix.fill_rect(r, &paint(self.conf.separator), SkTransform::identity(), None);
            }
        }
        // the dots
        let buttons = layout(&self.conf, w, h, ov);
        for b in &buttons {
            let c = self.conf.button_color(&b.id);
            let c = if hover == Some(b.id.as_str()) { darken(c, self.conf.hover_darken) } else { c };
            let r = b.size * s / 2.0;
            if let Some(p) = PathBuilder::from_circle(b.x * s + r, b.y * s + r, r) {
                pix.fill_path(&p, &paint(c), FillRule::Winding, SkTransform::identity(), None);
            }
        }
        // the icon and title, centred together, clear of the wider button group
        if ov.is_none() {
            let show_title = pc.title.unwrap_or(self.conf.title) && !title.is_empty();
            let icon = if self.conf.icon { self.icon(class) } else { None };
            let isz = self.conf.icon_size.min(h) * s;
            let text = pc.text.unwrap_or(self.conf.text);
            let room = {
                let l = buttons.iter().filter(|b| b.x < w / 2.0).map(|b| b.x + b.size).fold(0.0f32, f32::max);
                let r = buttons.iter().filter(|b| b.x >= w / 2.0).map(|b| w - b.x).fold(0.0f32, f32::max);
                (w - 2.0 * l.max(r) - 2.0 * self.conf.padding).max(0.0) * s
            };
            let fonts = self.fonts.get_or_insert_with(FontSystem::new);
            let size = self.conf.title_size * s * 96.0 / 72.0;
            let mut buf = Buffer::new(fonts, Metrics::new(size, size * 1.25));
            let mut tw = 0.0;
            if show_title {
                let family = self.conf.title_font.clone();
                let attrs = Attrs::new().family(Family::Name(&family)).weight(Weight(self.conf.title_weight));
                buf.set_size(fonts, Some((room - if icon.is_some() { isz + 6.0 * s } else { 0.0 }).max(1.0)), Some(size * 1.4));
                buf.set_text(fonts, title, &attrs, Shaping::Advanced);
                buf.shape_until_scroll(fonts, false);
                tw = buf.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
            }
            let total = tw + if icon.is_some() { isz + if show_title { 6.0 * s } else { 0.0 } } else { 0.0 };
            let mut x = ((pw as f32 - total) / 2.0).round();
            if let Some(ic) = &icon {
                let k = isz / ic.width().max(1) as f32;
                let t = SkTransform::from_row(k, 0.0, 0.0, k, x, ((ph as f32 - isz) / 2.0).round());
                pix.draw_pixmap(0, 0, ic.as_ref(), &PixmapPaint { quality: tiny_skia::FilterQuality::Bicubic, ..Default::default() }, t, None);
                x += isz + 6.0 * s;
            }
            if show_title {
                let y0 = ((ph as f32 - size * 1.25) / 2.0).round();
                let tc = TextColor::rgba((text[0] * 255.0) as u8, (text[1] * 255.0) as u8, (text[2] * 255.0) as u8, (text[3] * 255.0) as u8);
                let (pwi, phi) = (pw as i32, ph as i32);
                let data = pix.data_mut();
                buf.draw(fonts, &mut self.swash, tc, |gx, gy, gw, gh, c| {
                    let a = c.a() as f32 / 255.0;
                    if a <= 0.0 {
                        return;
                    }
                    for yy in 0..gh as i32 {
                        for xx in 0..gw as i32 {
                            let (px, py) = (x as i32 + gx + xx, y0 as i32 + gy + yy);
                            if px < 0 || py < 0 || px >= pwi || py >= phi {
                                continue;
                            }
                            let i = ((py * pwi + px) * 4) as usize;
                            // premultiplied "over"
                            for (ch, v) in [c.r(), c.g(), c.b()].into_iter().enumerate() {
                                data[i + ch] = (v as f32 * a + data[i + ch] as f32 * (1.0 - a)).round() as u8;
                            }
                            data[i + 3] = (255.0 * a + data[i + 3] as f32 * (1.0 - a)).round() as u8;
                        }
                    }
                });
            }
        }
        Some(pix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_in_every_form() {
        assert_eq!(color("rgb(ff0000)"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(color("rgba(00ff0080)").map(|c| (c[1], (c[3] * 255.0).round())), Some((1.0, 128.0)));
        assert_eq!(color("#0000ff"), Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(color("0x80ff0000").map(|c| (c[0], (c[3] * 255.0).round())), Some((1.0, 128.0)));
        assert_eq!(color("rgba(255, 0, 0, 0.5)"), Some([1.0, 0.0, 0.0, 0.5]));
        assert_eq!(color("nonsense"), None);
    }

    #[test]
    fn buttons_from_the_edge_close_outermost() {
        let c = Conf { left: vec![], right: vec!["minimize".into(), "maximize".into(), "close".into()], button_size: 12.0, button_gap: 8.0, padding: 10.0, ..Conf::load() };
        let b = layout(&c, 400.0, 28.0, None);
        let close = b.iter().find(|x| x.id == "close").unwrap();
        let min = b.iter().find(|x| x.id == "minimize").unwrap();
        assert_eq!(close.x, 400.0 - 10.0 - 12.0, "close is outermost on the right");
        assert!(min.x < close.x);
        assert_eq!(close.y, 8.0);
        let ov = Overlay::default();
        let p = layout(&c, 0.0, ov.height, Some(&ov));
        assert_eq!(p[0].x, ov.padding, "a pill fills from its padding, left to right");
    }
}

impl crate::state::Loom {
    /// What window `w` gets from ii-decor (none while fullscreen).
    pub fn decor_kind(&self, w: &smithay::desktop::Window) -> Kind {
        // an X menu or dropdown (Bitwig's) gets no title bar
        if w.x11_surface().is_some_and(crate::xwayland::x_menu_like) {
            return Kind::None;
        }
        self.decor.borrow().conf.kind(&self.class_of(w), self.is_full(w))
    }

    /// The height its title bar takes above it (0: none, or a pill over its content).
    pub fn bar_height(&self, w: &smithay::desktop::Window) -> f32 {
        match self.decor_kind(w) {
            Kind::Bar(h) => h,
            _ => 0.0,
        }
    }

    /// The bar or pill under `at`: its window, the button there (None: the bar itself), and
    /// where the bar is. Topmost window first.
    pub fn decor_at(&self, at: smithay::utils::Point<f64, smithay::utils::Logical>) -> Option<(smithay::desktop::Window, Option<String>)> {
        // a layer over it there (a notification, a panel) has the pointer, not the bar under it: a
        // notification swiped away over Bitwig's title bar dragged Bitwig
        if self.surface_at(at).is_some_and(|(s, _)| self.layer_output_of(&s).is_some()) {
            return None;
        }
        for w in self.space.elements().rev() {
            let Some(loc) = self.space.element_location(w) else { continue };
            // a window higher up whose content is there (not one of its own bar's buttons) covers the
            // bars under it
            let covers = smithay::desktop::space::SpaceElement::is_in_input_region(w, &(at - loc.to_f64()));
            let size = w.geometry().size;
            let kind = self.decor_kind(w);
            let (bx, by, bw, bh, ov) = match &kind {
                Kind::None if covers => return None,
                Kind::None => continue,
                Kind::Bar(h) => (loc.x as f64, loc.y as f64 - *h as f64, size.w as f64, *h as f64, None),
                Kind::Overlay(o) => {
                    let pw = pill_width(&self.decor.borrow().conf, o) as f64;
                    let x = if o.right { loc.x as f64 + size.w as f64 - o.x as f64 - pw } else { loc.x as f64 + o.x as f64 };
                    let y = if o.top { loc.y as f64 + o.y as f64 } else { loc.y as f64 + size.h as f64 - o.y as f64 - o.height as f64 };
                    (x, y, pw, o.height as f64, Some(o.clone()))
                }
            };
            if at.x < bx || at.y < by || at.x >= bx + bw || at.y >= by + bh {
                if covers {
                    return None;
                }
                continue;
            }
            let (rx, ry) = ((at.x - bx) as f32, (at.y - by) as f32);
            // a little slack around each dot so small dots are still easy to hit
            let slack = (self.decor.borrow().conf.button_gap / 2.0).max(2.0);
            let hit = layout(&self.decor.borrow().conf, bw as f32, bh as f32, ov.as_ref()).into_iter().find(|b| rx >= b.x - slack && rx < b.x + b.size + slack && ry >= b.y - slack && ry < b.y + b.size + slack).map(|b| b.id);
            if ov.is_some() && hit.is_none() {
                // a pill only takes its buttons; the rest of it is the app's content
                if covers {
                    return None;
                }
                continue;
            }
            return Some((w.clone(), hit));
        }
        None
    }

    /// A button press or release on a bar: true when the bar took it.
    pub fn decor_button(&mut self, pressed: bool, button: u32, time: u32) -> bool {
        let Some((w, hit)) = self.decor_at(self.pointer_at) else {
            if !pressed {
                self.decor.borrow_mut().pressed = None;
            }
            return false;
        };
        let addr = self.managed_of(&w).map(|m| m.addr).unwrap_or(0);
        if pressed {
            self.focus_window(&w);
            match hit {
                Some(id) => self.decor.borrow_mut().pressed = Some((addr, id)),
                None if button == 0x110 => {
                    // the bar: a double click maximizes, a press drags the window
                    let double = { let d = self.decor.borrow(); d.conf.double_click && d.last_click.is_some_and(|(a, t)| a == addr && time.wrapping_sub(t) < 400) };
                    self.decor.borrow_mut().last_click = Some((addr, time));
                    if double {
                        self.toggle_maximize(&w);
                    } else {
                        self.drag_start_on(&w, false, button);
                    }
                }
                None => {}
            }
            return true;
        }
        // a button acts on release, only over the dot it was pressed on
        let pressed_on = self.decor.borrow_mut().pressed.take();
        if let (Some((pa, pid)), Some(id)) = (pressed_on, hit) {
            if pa == addr && pid == id {
                match id.as_str() {
                    "close" => {
                        if let Some(t) = w.toplevel() {
                            t.send_close();
                        } else if let Some(x) = w.x11_surface() {
                            let _ = x.close();
                        }
                    }
                    "minimize" => {
                        let to = { let m = self.decor.borrow().conf.minimize_to.clone(); match m.parse::<i32>() { Ok(n) => n, Err(_) => self.special_id(&m) } };
                        self.focus_window(&w);
                        self.move_to_workspace(to);
                    }
                    _ => self.toggle_maximize(&w),
                }
            }
        }
        true
    }

    /// The button under the pointer changed: its dot is drawn darker.
    pub fn decor_hover(&mut self) {
        let now = self.decor_at(self.pointer_at).and_then(|(w, id)| Some((self.managed_of(&w)?.addr, id?)));
        if now != self.decor.borrow().hover {
            self.decor.borrow_mut().hover = now;
            self.dirty = true;
        }
    }

    /// Maximized (the whole tiling area, over the others) or back into the tiling.
    pub fn toggle_maximize(&mut self, w: &smithay::desktop::Window) {
        // a floating window maximized fills its screen's area (the layout only sizes tiled ones), and gets its
        // place and size back after
        let was = self.maximized.clone();
        self.maximized = if self.maximized.as_ref() == Some(w) { None } else { Some(w.clone()) };
        if let Some(old) = was.filter(|o| self.managed_of(o).is_some_and(|m| m.floating)) {
            self.float_restore(&old);
        }
        if self.maximized.as_ref() == Some(w) && self.managed_of(w).is_some_and(|m| m.floating) {
            let addr = self.managed_of(w).map(|m| m.addr).unwrap_or(0);
            let area = self.space.outputs_for_element(w).first().cloned().or_else(|| self.outputs.first().cloned()).and_then(|o| self.area_of(&o));
            if let (Some(g), Some(a)) = (self.space.element_geometry(w), area) {
                self.full_restore.insert(addr, g);
                let gap = self.look.gaps_out.round() as i32;
                let r = smithay::utils::Rectangle::new((a.loc.x + gap, a.loc.y + gap).into(), ((a.size.w - 2 * gap).max(1), (a.size.h - 2 * gap).max(1)).into());
                if let Some(t) = w.toplevel() { t.with_pending_state(|s| s.size = Some(r.size)); if t.is_initial_configure_sent() { t.send_pending_configure(); } } else if let Some(x) = w.x11_surface() { let _ = x.configure(r); }
                self.space.map_element(w.clone(), r.loc, true);
            }
        }
        self.relayout();
        let on = self.maximized.as_ref() == Some(w);
        if !self.is_full(w) {
            self.hypr_event(&format!("fullscreen>>{}", u8::from(on)));
        }
        self.fx_event("windowMaximizedStateChanged", w, &[on.into(), on.into()]);
        self.foreign_all_changed();
    }
}
