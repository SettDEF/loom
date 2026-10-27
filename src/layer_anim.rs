//! Layer animations, as Hyprland's config sets them: `animation = layersIn|layersOut|fadeLayersIn|
//! fadeLayersOut, on, speed, curve[, style]` with `bezier` curves, and per layer
//! `layerrule = match:namespace RE, animation slide right` (slide [dir], popin N%, fade) or
//! `no_anim on`. A layer opens with its first frame; closing, its last frame stays drawn and
//! animates out.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use regex::Regex;
use smithay::utils::{Logical, Rectangle, Size};

use crate::anim::Bezier;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    /// grown from (or shrunk to) this much of its size
    Popin(f64),
    /// in from (out to) an edge; None: the nearest one
    Slide(Option<Edge>),
    Fade,
}

impl Style {
    fn parse(s: &str) -> Option<Style> {
        let mut w = s.split_whitespace();
        Some(match w.next()? {
            "popin" => Style::Popin(w.next().and_then(|p| p.trim_end_matches('%').parse::<f64>().ok()).map_or(0.8, |p| p / 100.0)),
            "slide" | "slidevert" => Style::Slide(match w.next() {
                Some("left") => Some(Edge::Left),
                Some("right") => Some(Edge::Right),
                Some("top") => Some(Edge::Top),
                Some("bottom") => Some(Edge::Bottom),
                _ => None,
            }),
            "fade" => Style::Fade,
            _ => return None,
        })
    }
}

/// One of Hyprland's `animation` lines: on, how long, along which curve, in which style.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    pub on: bool,
    pub dur: Duration,
    pub curve: Bezier,
    pub style: Option<Style>,
}

impl Motion {
    fn new(ms: u64, curve: Bezier) -> Motion {
        Motion { on: true, dur: Duration::from_millis(ms), curve, style: None }
    }
}

enum Rule {
    Style(Style),
    NoAnim(bool),
}

/// The settings: in and out (move/scale and fade), and the per-layer rules.
pub struct Config {
    pub enabled: bool,
    pub layers_in: Motion,
    pub layers_out: Motion,
    pub fade_in: Motion,
    pub fade_out: Motion,
    /// windows: opening, closing, moving, and their fades (windowsIn/Out/Move, fadeIn/Out)
    pub windows_in: Motion,
    pub windows_out: Motion,
    pub windows_move: Motion,
    pub win_fade_in: Motion,
    pub win_fade_out: Motion,
    rules: Vec<(Regex, Rule)>,
}

impl Config {
    pub fn load() -> Config {
        let lines = crate::config::hyprland_lines(&crate::config::hyprland_conf());
        let mut curves: HashMap<String, Bezier> = HashMap::from([("default".to_string(), Bezier::DEFAULT), ("linear".to_string(), Bezier { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 })]);
        // Hyprland's defaults (speed is in tenths of a second)
        let mut c = Config {
            enabled: true,
            layers_in: Motion::new(400, Bezier::DEFAULT),
            layers_out: Motion::new(400, Bezier::DEFAULT),
            fade_in: Motion::new(400, Bezier::DEFAULT),
            fade_out: Motion::new(400, Bezier::DEFAULT),
            windows_in: Motion::new(400, Bezier::DEFAULT),
            windows_out: Motion::new(400, Bezier::DEFAULT),
            windows_move: Motion::new(400, Bezier::DEFAULT),
            win_fade_in: Motion::new(400, Bezier::DEFAULT),
            win_fade_out: Motion::new(400, Bezier::DEFAULT),
            rules: Vec::new(),
        };
        c.layers_in.style = Some(Style::Popin(0.8));
        c.layers_out.style = Some(Style::Popin(0.8));
        for (k, v) in &lines {
            match k.as_str() {
                "animations:enabled" => c.enabled = !matches!(v.trim(), "false" | "0" | "no"),
                "animations:bezier" | "bezier" => {
                    let p: Vec<&str> = v.split(',').map(str::trim).collect();
                    if let [name, a, b, x, y] = p[..] {
                        if let (Ok(x1), Ok(y1), Ok(x2), Ok(y2)) = (a.parse(), b.parse(), x.parse(), y.parse()) {
                            curves.insert(name.to_string(), Bezier { x1, y1, x2, y2 });
                        }
                    }
                }
                "animations:animation" | "animation" => {
                    let p: Vec<&str> = v.split(',').map(str::trim).collect();
                    let Some(name) = p.first() else { continue };
                    let slot = match *name {
                        // the wider ones set what is under them, the exact one after wins (lines in order)
                        "global" => vec![0, 1, 2, 3, 4, 5, 6, 7, 8],
                        "layers" => vec![0, 1],
                        "layersIn" => vec![0],
                        "layersOut" => vec![1],
                        "fade" => vec![2, 3, 7, 8],
                        "fadeLayers" => vec![2, 3],
                        "fadeLayersIn" => vec![2],
                        "fadeLayersOut" => vec![3],
                        "windows" => vec![4, 5, 6],
                        "windowsIn" => vec![4],
                        "windowsOut" => vec![5],
                        "windowsMove" => vec![6],
                        "fadeIn" => vec![7],
                        "fadeOut" => vec![8],
                        _ => continue,
                    };
                    let on = p.get(1).is_none_or(|s| *s != "0");
                    let ms = p.get(2).and_then(|s| s.parse::<f64>().ok()).map(|s| (s * 100.0) as u64);
                    let curve = p.get(3).and_then(|n| curves.get(*n)).copied();
                    let style = p.get(4).and_then(|s| Style::parse(s));
                    for i in slot {
                        let m = match i {
                            0 => &mut c.layers_in,
                            1 => &mut c.layers_out,
                            2 => &mut c.fade_in,
                            3 => &mut c.fade_out,
                            4 => &mut c.windows_in,
                            5 => &mut c.windows_out,
                            6 => &mut c.windows_move,
                            7 => &mut c.win_fade_in,
                            _ => &mut c.win_fade_out,
                        };
                        m.on = on;
                        if let Some(ms) = ms {
                            m.dur = Duration::from_millis(ms);
                        }
                        if let Some(cv) = curve {
                            m.curve = cv;
                        }
                        if style.is_some() && matches!(i, 0 | 1 | 4 | 5 | 6) {
                            m.style = style;
                        }
                    }
                }
                "layerrule" => c.rules.extend(rule(v)),
                _ => {}
            }
        }
        c
    }

    /// How the layer `ns` animates: None when it does not (no_anim, or animations off).
    pub fn style_for(&self, ns: &str, opening: bool) -> Option<Style> {
        if !self.enabled {
            return None;
        }
        let m = if opening { &self.layers_in } else { &self.layers_out };
        let mut style = if m.on { m.style.or(Some(Style::Popin(0.8))) } else { None };
        let mut no_anim = false;
        for (re, r) in &self.rules {
            if re.is_match(ns) {
                match r {
                    Rule::Style(s) => style = Some(*s),
                    Rule::NoAnim(b) => no_anim = *b,
                }
            }
        }
        if no_anim {
            return None;
        }
        style
    }
}

/// `match:namespace RE, animation slide right` / `no_anim on` (and the older `animation slide, NAME`, `noanim, NAME`).
fn rule(v: &str) -> Option<(Regex, Rule)> {
    let (a, b) = v.split_once(',')?;
    let (a, b) = (a.trim(), b.trim());
    let (pattern, effect) = match a.strip_prefix("match:namespace") {
        Some(p) => (p.trim(), b),
        None => (b.strip_prefix("namespace:").unwrap_or(b), a),
    };
    let effect = effect.split(" #").next().unwrap_or(effect).trim();
    let r = if let Some(s) = effect.strip_prefix("animation") {
        Rule::Style(Style::parse(s)?)
    } else if let Some(s) = effect.strip_prefix("no_anim").or_else(|| effect.strip_prefix("noanim")) {
        Rule::NoAnim(!matches!(s.trim(), "off" | "false" | "0"))
    } else {
        return None;
    };
    Some((Regex::new(&format!("^(?:{pattern})$")).ok()?, r))
}

/// A layer animating: opening (to its place) or closing (from it).
#[derive(Clone, Copy, Debug)]
pub struct LayerAnim {
    pub opening: bool,
    pub start: Instant,
    pub style: Style,
    pub motion: Motion,
    pub fade: Motion,
}

/// Where and how a layer is drawn at a moment: moved by (dx, dy), scaled around its middle, faded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub dx: f64,
    pub dy: f64,
    pub scale: f64,
    pub alpha: f64,
}

impl Look {
    pub const STILL: Look = Look { dx: 0.0, dy: 0.0, scale: 1.0, alpha: 1.0 };
}

impl LayerAnim {
    pub fn new(cfg: &Config, style: Style, opening: bool, now: Instant) -> LayerAnim {
        let (motion, fade) = if opening { (cfg.layers_in, cfg.fade_in) } else { (cfg.layers_out, cfg.fade_out) };
        LayerAnim { opening, start: now, style, motion, fade }
    }

    pub fn done(&self, now: Instant) -> bool {
        let t = now.saturating_duration_since(self.start);
        t >= self.motion.dur && (t >= self.fade.dur || !self.fade.on)
    }

    /// `geo`: the layer's box, `screen`: the output's size (both logical).
    pub fn look(&self, now: Instant, geo: Rectangle<i32, Logical>, screen: Size<i32, Logical>) -> Look {
        let t = now.saturating_duration_since(self.start).as_secs_f64();
        let frac = |d: Duration| if d.is_zero() { 1.0 } else { (t / d.as_secs_f64()).min(1.0) };
        // how far from "away" (0) to "in place" (1)
        let p = self.motion.curve.at(frac(self.motion.dur));
        let p = if self.opening { p } else { 1.0 - p };
        let f = if self.fade.on { self.fade.curve.at(frac(self.fade.dur)) } else { 1.0 };
        let alpha = if self.opening { f } else { 1.0 - f }.clamp(0.0, 1.0);
        let mut l = Look { alpha, ..Look::STILL };
        match self.style {
            Style::Fade => {}
            Style::Popin(s) => l.scale = s + (1.0 - s) * p,
            Style::Slide(edge) => {
                let edge = edge.unwrap_or_else(|| nearest(geo, screen));
                // the distance that takes it just off the screen
                let away = match edge {
                    Edge::Left => (-(geo.loc.x + geo.size.w) as f64, 0.0),
                    Edge::Right => ((screen.w - geo.loc.x) as f64, 0.0),
                    Edge::Top => (0.0, -(geo.loc.y + geo.size.h) as f64),
                    Edge::Bottom => (0.0, (screen.h - geo.loc.y) as f64),
                };
                l.dx = away.0 * (1.0 - p);
                l.dy = away.1 * (1.0 - p);
                // sliding layers stay opaque (Hyprland fades only popin and fade)
                l.alpha = 1.0;
            }
        }
        l
    }
}

/// The screen edge closest to the layer's middle.
fn nearest(geo: Rectangle<i32, Logical>, screen: Size<i32, Logical>) -> Edge {
    let (cx, cy) = ((geo.loc.x + geo.size.w / 2) as f64 / screen.w.max(1) as f64, (geo.loc.y + geo.size.h / 2) as f64 / screen.h.max(1) as f64);
    let d = [(cx, Edge::Left), (1.0 - cx, Edge::Right), (cy, Edge::Top), (1.0 - cy, Edge::Bottom)];
    d.iter().min_by(|a, b| a.0.total_cmp(&b.0)).map(|x| x.1).unwrap_or(Edge::Top)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rules_and_moves_layers() {
        let (re, r) = rule("match:namespace quickshell:sidebarRight, animation slide right").unwrap();
        assert!(re.is_match("quickshell:sidebarRight") && matches!(r, Rule::Style(Style::Slide(Some(Edge::Right)))));
        assert!(matches!(rule("match:namespace quickshell:overlay, no_anim on").unwrap().1, Rule::NoAnim(true)));
        assert!(matches!(rule("match:namespace quickshell:screenCorners, animation popin 120%").unwrap().1, Rule::Style(Style::Popin(p)) if (p - 1.2).abs() < 1e-9));
        let cfg = Config { enabled: true, layers_in: Motion::new(300, Bezier::DEFAULT), layers_out: Motion::new(300, Bezier::DEFAULT), fade_in: Motion::new(100, Bezier::DEFAULT), fade_out: Motion::new(100, Bezier::DEFAULT), windows_in: Motion::new(300, Bezier::DEFAULT), windows_out: Motion::new(300, Bezier::DEFAULT), windows_move: Motion::new(300, Bezier::DEFAULT), win_fade_in: Motion::new(100, Bezier::DEFAULT), win_fade_out: Motion::new(100, Bezier::DEFAULT), rules: vec![] };
        let now = Instant::now();
        let a = LayerAnim::new(&cfg, Style::Slide(Some(Edge::Right)), true, now);
        let geo = Rectangle::new((1200, 0).into(), (400, 1000).into());
        let screen = (1600, 1000).into();
        // starts just off the right edge, ends in place
        assert_eq!(a.look(now, geo, screen).dx, 400.0);
        assert_eq!(a.look(now + Duration::from_millis(400), geo, screen), Look::STILL);
        assert!(a.done(now + Duration::from_millis(300)));
    }
}

/// A closed layer's last frame, drawn while it animates out.
pub struct LayerGhost {
    pub ghost: crate::kwin_fx::glue::Ghost,
    /// the screen it was on (its box is relative to it)
    pub output: String,
    pub layer: smithay::wayland::shell::wlr_layer::Layer,
    pub anim: LayerAnim,
    pub geo: Rectangle<i32, Logical>,
}

use smithay::reexports::wayland_server::{backend::ObjectId, protocol::wl_surface::WlSurface, Resource};

impl crate::state::Loom {
    /// A layer's commit: its first frame starts its opening; a commit taking its picture away
    /// (`unmapping`, checked before the buffer is let go) leaves its last frame to animate out.
    pub fn layer_committed(&mut self, surface: &WlSurface, unmapping: bool) {
        let Some((output, _)) = self.layer_output_of(surface) else { return };
        let map = smithay::desktop::layer_map_for_output(&output);
        let Some(layer) = map.layer_for_surface(surface, smithay::desktop::WindowSurfaceType::TOPLEVEL).cloned() else { return };
        let geo = map.layer_geometry(&layer).unwrap_or_default();
        drop(map);
        let id = surface.id();
        let ns = layer.namespace().to_string();
        if unmapping {
            self.layer_keyboard_changed(surface, false);
            if self.layer_shown.remove(&id) {
                self.layer_anims.remove(&id);
                self.layer_closing(surface, layer.layer(), &ns, geo);
            }
            return;
        }
        let has = smithay::backend::renderer::utils::with_renderer_surface_state(surface, |s| s.buffer().is_some()).unwrap_or(false);
        self.layer_keyboard_changed(surface, has);
        if has && self.layer_shown.insert(id.clone()) {
            // under the login screen's curtain no opening of its own: the curtain's fade is the one
            // (a layer fading in under a fading curtain let the empty screen through, 2 dark frames)
            if self.curtain_animating(Instant::now()) {
                return;
            }
            if let Some(style) = self.layer_cfg.style_for(&ns, true) {
                self.layer_anims.insert(id, LayerAnim::new(&self.layer_cfg, style, true, Instant::now()));
            }
        }
    }

    /// A layer going (unmapped or destroyed): its last frame kept, animating out.
    pub fn layer_closing(&mut self, surface: &WlSurface, layer: smithay::wayland::shell::wlr_layer::Layer, ns: &str, geo: Rectangle<i32, Logical>) {
        self.layer_shown.remove(&surface.id());
        let Some(style) = self.layer_cfg.style_for(ns, false) else { return };
        let kept = self.with_gles(|r| {
            let id = smithay::backend::renderer::Renderer::context_id(r);
            smithay::backend::renderer::utils::with_renderer_surface_state(surface, |s| Some((s.texture(id)?.clone(), s.surface_size()?))).flatten()
        });
        let Some(Some((tex, size))) = kept else { return };
        use smithay::backend::renderer::Texture;
        let px = (tex.width() as i32, tex.height() as i32);
        let Some(buffer) = self.with_gles(|r| smithay::backend::renderer::element::texture::TextureBuffer::from_texture(r, tex, 1, smithay::utils::Transform::Normal, None)) else { return };
        let ghost = crate::kwin_fx::glue::Ghost { addr: 0, buffer, geo: Rectangle::new(geo.loc.to_f64(), (size.w as f64, size.h as f64).into()), px };
        let output = self.layer_output_of(surface).map(|(o, _)| o.name()).or_else(|| self.output.as_ref().map(|o| o.name())).unwrap_or_default();
        self.layer_ghosts.push(LayerGhost { ghost, output, layer, anim: LayerAnim::new(&self.layer_cfg, style, false, Instant::now()), geo });
        self.dirty = true;
    }

    /// Finished animations and ghosts: gone.
    pub fn layer_anims_tick(&mut self, now: Instant) {
        self.layer_anims.retain(|_, a| !a.done(now));
        self.layer_ghosts.retain(|g| !g.anim.done(now));
        self.win_ghosts.retain(|g| !g.anim.done(now));
    }

    pub fn layers_animating(&self) -> bool {
        !self.layer_anims.is_empty() || !self.layer_ghosts.is_empty() || !self.win_ghosts.is_empty()
    }

    /// How the layer with root surface `id` is drawn now.
    pub fn layer_look(&self, id: &ObjectId, geo: Rectangle<i32, Logical>, now: Instant, output: &smithay::output::Output) -> Look {
        let Some(a) = self.layer_anims.get(id) else { return Look::STILL };
        let screen = Some(output).and_then(|o| {
            let m = o.current_mode()?;
            Some(o.current_transform().transform_size(m.size).to_f64().to_logical(o.current_scale().fractional_scale()).to_i32_round::<i32>())
        });
        a.look(now, geo, screen.unwrap_or_default())
    }
}

/// A closed window's last frame, drawn while it animates out (windowsOut, fadeOut).
pub struct WinGhost {
    pub ghost: crate::kwin_fx::glue::Ghost,
    pub anim: LayerAnim,
    /// its box in compositor space
    pub geo: Rectangle<i32, Logical>,
}

impl LayerAnim {
    /// A window closing, as windowsOut and fadeOut say (popin by default).
    pub fn window_close(cfg: &Config, now: Instant) -> Option<LayerAnim> {
        if !cfg.enabled || !(cfg.windows_out.on || cfg.win_fade_out.on) {
            return None;
        }
        let style = match cfg.windows_out.style {
            Some(Style::Slide(e)) => Style::Slide(e),
            Some(Style::Fade) => Style::Fade,
            Some(Style::Popin(s)) => Style::Popin(s),
            None => Style::Popin(0.8),
        };
        Some(LayerAnim { opening: false, start: now, style, motion: cfg.windows_out, fade: cfg.win_fade_out })
    }
}

impl crate::state::Loom {
    /// A window going: its last frame kept, animating out where it was.
    pub fn window_closing(&mut self, w: &smithay::desktop::Window) {
        use smithay::wayland::seat::WaylandFocus;
        let Some(anim) = LayerAnim::window_close(&self.layer_cfg, Instant::now()) else { return };
        let (Some(root), Some(loc)) = (w.wl_surface().map(|s| s.into_owned()), self.space.element_location(w)) else { return };
        let kept = self.with_gles(|r| {
            let id = smithay::backend::renderer::Renderer::context_id(r);
            smithay::backend::renderer::utils::with_renderer_surface_state(&root, |s| Some((s.texture(id)?.clone(), s.surface_size()?))).flatten()
        });
        let Some(Some((tex, size))) = kept else { return };
        use smithay::backend::renderer::Texture;
        let px = (tex.width() as i32, tex.height() as i32);
        let Some(buffer) = self.with_gles(|r| smithay::backend::renderer::element::texture::TextureBuffer::from_texture(r, tex, 1, smithay::utils::Transform::Normal, None)) else { return };
        // the whole surface (its shadow too), where it was
        let at = loc - w.geometry().loc;
        let geo = Rectangle::new(at, (size.w, size.h).into());
        let ghost = crate::kwin_fx::glue::Ghost { addr: 0, buffer, geo: geo.to_f64(), px };
        self.win_ghosts.push(WinGhost { ghost, anim, geo });
        self.dirty = true;
    }
}
