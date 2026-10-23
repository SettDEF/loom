//! Window borders and shadows, from Hyprland's `general { border_size, col.active_border,
//! col.inactive_border }` and `decoration { shadow { … } }`: one shader per window draws its
//! border ring (just outside its rounded box) and its shadow under it. GL only. A window with no
//! border by the shell's Glass rules (the see-through apps) keeps its shadow.

use std::{cell::RefCell, collections::HashMap};

use smithay::{
    backend::renderer::{
        element::Kind,
        gles::{element::PixelShaderElement, GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType},
    },
    utils::{Logical, Physical, Rectangle, Scale},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub border: f32,
    pub active: [f32; 4],
    pub inactive: [f32; 4],
    pub shadow: bool,
    pub range: f32,
    pub power: f32,
    pub offset: (f32, f32),
    pub shadow_color: [f32; 4],
}

impl Config {
    pub fn load() -> Config {
        let mut c = Config { border: 1.0, active: [0.2, 0.6, 0.9, 1.0], inactive: [0.3, 0.3, 0.3, 1.0], shadow: true, range: 4.0, power: 3.0, offset: (0.0, 0.0), shadow_color: [0.1, 0.1, 0.1, 0.93] };
        for (k, v) in crate::config::hyprland_lines(&crate::config::hyprland_conf()) {
            let num = |d: f32| v.trim().parse().unwrap_or(d);
            match k.as_str() {
                "general:border_size" => c.border = num(c.border).max(0.0),
                "general:col.active_border" => c.active = color(&v).unwrap_or(c.active),
                "general:col.inactive_border" => c.inactive = color(&v).unwrap_or(c.inactive),
                "decoration:shadow:enabled" => c.shadow = !matches!(v.trim(), "false" | "0" | "no"),
                "decoration:shadow:range" => c.range = num(c.range).max(0.0),
                "decoration:shadow:render_power" => c.power = num(c.power).clamp(1.0, 4.0).max(1.0),
                "decoration:shadow:offset" => {
                    let p: Vec<f32> = v.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                    if let [x, y] = p[..] {
                        c.offset = (x, y);
                    }
                }
                "decoration:shadow:color" => c.shadow_color = color(&v).unwrap_or(c.shadow_color),
                _ => {}
            }
        }
        c
    }
}

/// `rgba(RRGGBBAA)`, `rgb(RRGGBB)` or `0xAARRGGBB` (a gradient: its first colour), premultiplied.
pub fn color(v: &str) -> Option<[f32; 4]> {
    let t = v.split_whitespace().next()?;
    let hex = |s: &str| u32::from_str_radix(s, 16).ok();
    let (r, g, b, a) = if let Some(h) = t.strip_prefix("rgba(").and_then(|s| s.strip_suffix(')')) {
        let n = hex(h)?;
        (n >> 24, (n >> 16) & 255, (n >> 8) & 255, n & 255)
    } else if let Some(h) = t.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let n = hex(h)?;
        (n >> 16, (n >> 8) & 255, n & 255, 255)
    } else if let Some(h) = t.strip_prefix("0x") {
        let n = hex(h)?;
        ((n >> 16) & 255, (n >> 8) & 255, n & 255, n >> 24)
    } else {
        return None;
    };
    let a = a as f32 / 255.0;
    Some([r as f32 / 255.0 * a, g as f32 / 255.0 * a, b as f32 / 255.0 * a, a])
}

const SHADER: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;
// the window's box in this element (pixels), its corner radius and curve
uniform vec2 box_off;
uniform vec2 box_size;
uniform float radius;
uniform float border;
uniform vec4 border_col;
uniform float range;
uniform float power;
uniform vec2 shadow_off;
uniform vec4 shadow_col;

// how far outside the rounded box (negative inside)
float sd(vec2 p, vec2 off, vec2 bs, float r) {
    vec2 c = off + bs * 0.5;
    vec2 q = abs(p - c) - (bs * 0.5 - vec2(r));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 p = v_coords * size;
    float d = sd(p, box_off, box_size, radius);
    // nothing over the window itself
    if (d < -0.5) {
        discard;
    }
    float inside = clamp(d + 0.5, 0.0, 1.0);
    // the border ring, just outside the box
    float b = border > 0.0 ? inside * clamp(border - d + 0.5, 0.0, 1.0) : 0.0;
    // the shadow: falling off over its range, outside the window only
    float ds = sd(p, box_off + shadow_off, box_size, radius);
    float s = range > 0.0 ? pow(clamp(1.0 - ds / range, 0.0, 1.0), power) * inside : 0.0;
    vec4 col = border_col * b + shadow_col * s * (1.0 - border_col.a * b);
    gl_FragColor = col * alpha;
}
"#;

thread_local! {
    static PROGRAM: RefCell<Option<Option<GlesPixelProgram>>> = const { RefCell::new(None) };
}

fn program(r: &mut GlesRenderer) -> Option<GlesPixelProgram> {
    PROGRAM.with(|p| {
        let mut p = p.borrow_mut();
        if p.is_none() {
            let f = |n: &'static str| UniformName::new(n, UniformType::_1f);
            let v2 = |n: &'static str| UniformName::new(n, UniformType::_2f);
            let v4 = |n: &'static str| UniformName::new(n, UniformType::_4f);
            let compiled = r.compile_custom_pixel_shader(SHADER, &[v2("box_off"), v2("box_size"), f("radius"), f("border"), v4("border_col"), f("range"), f("power"), v2("shadow_off"), v4("shadow_col")]);
            if let Err(e) = &compiled {
                eprintln!("loom: the border/shadow shader did not compile: {e:?}");
            }
            *p = Some(compiled.ok());
        }
        p.clone().flatten()
    })
}

/// One window's frame, kept between frames (the same element: only what changed is drawn again).
type Key = (Rectangle<i32, Physical>, u32, bool, bool);

thread_local! {
    static CACHE: RefCell<HashMap<u64, (PixelShaderElement, Key, u32)>> = RefCell::new(HashMap::new());
}

/// The frame element for window `addr`: its box on this screen (physical), corner radius, focus,
/// whether it has a border, the frame's alpha. None when there is nothing to draw.
#[allow(clippy::too_many_arguments)]
pub fn element(r: &mut GlesRenderer, cfg: &Config, addr: u64, boxed: Rectangle<i32, Physical>, radius: f32, scale: f64, active: bool, bordered: bool, alpha: f32) -> Option<PixelShaderElement> {
    let border = if bordered { cfg.border * scale as f32 } else { 0.0 };
    let range = if cfg.shadow { cfg.range * scale as f32 } else { 0.0 };
    if border <= 0.0 && range <= 0.0 {
        return None;
    }
    let prog = program(r)?;
    // room round the box for the ring and the shadow
    let off = ((cfg.offset.0 * scale as f32).abs().max((cfg.offset.1 * scale as f32).abs()) + range + border).ceil() as i32 + 1;
    let phys = Rectangle::<i32, Physical>::new((boxed.loc.x - off, boxed.loc.y - off).into(), (boxed.size.w + 2 * off, boxed.size.h + 2 * off).into());
    let area: Rectangle<i32, Logical> = phys.to_f64().to_logical(scale).to_i32_round();
    // where the element really lands (Smithay rounds the logical area back to pixels)
    let lands: Rectangle<i32, Physical> = area.to_physical_precise_round(Scale::from(scale));
    let key: Key = (boxed, radius.to_bits(), active, bordered);
    let col = if active { cfg.active } else { cfg.inactive };
    let uniforms = vec![
        Uniform::new("box_off", ((boxed.loc.x - lands.loc.x) as f32, (boxed.loc.y - lands.loc.y) as f32)),
        Uniform::new("box_size", (boxed.size.w as f32, boxed.size.h as f32)),
        Uniform::new("radius", radius),
        Uniform::new("border", border),
        Uniform::new("border_col", (col[0], col[1], col[2], col[3])),
        Uniform::new("range", range),
        Uniform::new("power", cfg.power),
        Uniform::new("shadow_off", (cfg.offset.0 * scale as f32, cfg.offset.1 * scale as f32)),
        Uniform::new("shadow_col", (cfg.shadow_color[0], cfg.shadow_color[1], cfg.shadow_color[2], cfg.shadow_color[3])),
    ];
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        // its alpha is fixed when made: a fading window gets a new one
        let a = (alpha * 1000.0) as u32;
        let fresh = || (PixelShaderElement::new(prog.clone(), area, None, alpha, uniforms.clone(), Kind::Unspecified), key, a);
        let e = c.entry(addr).or_insert_with(fresh);
        if e.2 != a {
            *e = fresh();
        } else if e.1 != key {
            e.0.resize(area, None);
            e.0.update_uniforms(uniforms);
            e.1 = key;
        }
        Some(e.0.clone())
    })
}

/// A window gone: its frame element forgotten.
pub fn forget(addr: u64) {
    CACHE.with(|c| c.borrow_mut().remove(&addr));
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_colours() {
        assert_eq!(super::color("rgba(00000020)").map(|c| (c[3] * 255.0).round()), Some(32.0));
        let c = super::color("rgba(0DB7D455) rgba(ff00ffaa) 45deg").unwrap();
        assert!((c[3] - 0x55 as f32 / 255.0).abs() < 1e-6);
        assert_eq!(super::color("0xff112233").map(|c| c[3]), Some(1.0));
    }
}
