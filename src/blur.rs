//! Blur behind surfaces, as Hyprland's `decoration:blur` does it: what is behind is drawn to a
//! texture, run through dual-Kawase passes, and shown under the surface where its alpha is above
//! the layer's `ignore_alpha`. With `xray` (the default) only the wallpaper layers are behind, so
//! every blurred surface of an output shares one blurred wallpaper. The blur is kept and redone
//! only when what is behind changes. GL only.

use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{Hash, Hasher},
};

use regex::Regex;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{surface::WaylandSurfaceRenderElement, Element, Id, Kind, RenderElement, UnderlyingStorage},
            gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType},
            utils::{CommitCounter, DamageSet, OpaqueRegions},
            Bind, Color32F, Frame, Offscreen, Renderer, Texture,
        },
    },
    utils::{Buffer, Physical, Rectangle, Scale, Size, Transform},
};

/// Hyprland's blur values (decoration:blur:*), the shell's Glass overrides on top.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub size: f32,
    pub passes: u32,
    pub noise: f32,
    pub contrast: f32,
    pub brightness: f32,
    pub vibrancy: f32,
    pub xray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, size: 8.0, passes: 1, noise: 0.0117, contrast: 0.8916, brightness: 0.8172, vibrancy: 0.1696, xray: false }
    }
}

/// How a layer is blurred: `ignore` = blur only where its alpha is above this (None: all of it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerBlur {
    pub ignore: Option<f32>,
    pub popups: bool,
}

enum Effect {
    Blur(bool),
    Ignore(f32),
    Popups(bool),
}

/// The blur settings and rules, read from Hyprland's config and the shell's Glass files.
pub struct Config {
    pub settings: Settings,
    layers: Vec<(Regex, Effect)>,
    /// window class pattern, blurred or not (`no_blur`), later rules winning
    windows: Vec<(Regex, bool)>,
    layer_cache: RefCell<HashMap<String, Option<LayerBlur>>>,
    window_cache: RefCell<HashMap<String, bool>>,
}

fn whole(pattern: &str) -> Option<Regex> {
    Regex::new(&format!("^(?:{pattern})$")).ok()
}

fn on(v: &str) -> bool {
    !matches!(v.trim(), "off" | "false" | "0")
}

impl Config {
    pub fn load() -> Config {
        let lines = crate::config::hyprland_lines(&crate::config::hyprland_conf());
        let mut s = Settings::default();
        let mut layers = Vec::new();
        let mut windows = Vec::new();
        for (k, v) in &lines {
            let num = |d: f32| v.parse().unwrap_or(d);
            match k.as_str() {
                "decoration:blur:enabled" => s.enabled = on(v),
                "decoration:blur:size" => s.size = num(s.size),
                "decoration:blur:passes" => s.passes = v.parse().unwrap_or(s.passes),
                "decoration:blur:noise" => s.noise = num(s.noise),
                "decoration:blur:contrast" => s.contrast = num(s.contrast),
                "decoration:blur:brightness" => s.brightness = num(s.brightness),
                "decoration:blur:vibrancy" => s.vibrancy = num(s.vibrancy),
                "decoration:blur:xray" => s.xray = on(v),
                "layerrule" => layers.extend(layer_rule(v)),
                "windowrule" | "windowrulev2" => windows.extend(window_rule(v)),
                _ => {}
            }
        }
        // the shell's Glass: blur values and per-app rules (Lua, read without running it)
        let lua = crate::config::home().join(".config/hypr/lua");
        if let Ok(text) = std::fs::read_to_string(lua.join("glass_blur.lua")) {
            glass_blur(&text, &mut s);
        }
        if let Ok(text) = std::fs::read_to_string(lua.join("glass_rules.lua")) {
            for line in text.lines().filter(|l| !l.trim_start().starts_with("--")) {
                windows.extend(glass_rule(line));
            }
        }
        // LOOM_BLUR=0: none (to compare)
        if std::env::var("LOOM_BLUR").is_ok_and(|v| v == "0") {
            s.enabled = false;
        }
        Config { settings: s, layers, windows, layer_cache: Default::default(), window_cache: Default::default() }
    }

    /// How the layer named `ns` is blurred, None if it is not.
    pub fn layer(&self, ns: &str) -> Option<LayerBlur> {
        if !self.settings.enabled {
            return None;
        }
        if let Some(r) = self.layer_cache.borrow().get(ns) {
            return *r;
        }
        let (mut blur, mut lb) = (false, LayerBlur { ignore: None, popups: false });
        for (re, e) in &self.layers {
            if re.is_match(ns) {
                match e {
                    Effect::Blur(b) => blur = *b,
                    Effect::Ignore(a) => lb.ignore = Some(*a),
                    Effect::Popups(p) => lb.popups = *p,
                }
            }
        }
        let r = blur.then_some(lb);
        self.layer_cache.borrow_mut().insert(ns.to_string(), r);
        r
    }

    /// Whether a window of class `class` is blurred behind.
    pub fn window(&self, class: &str) -> bool {
        if !self.settings.enabled {
            return false;
        }
        if let Some(r) = self.window_cache.borrow().get(class) {
            return *r;
        }
        // Hyprland blurs every window unless a rule says no_blur
        let r = self.windows.iter().filter(|(re, _)| re.is_match(class)).last().is_none_or(|(_, b)| *b);
        self.window_cache.borrow_mut().insert(class.to_string(), r);
        r
    }
}

/// `match:namespace RE, blur on` (and the older `blur, RE` / `ignorealpha 0.5, RE`).
fn layer_rule(v: &str) -> Option<(Regex, Effect)> {
    let (a, b) = v.split_once(',')?;
    let (a, b) = (a.trim(), b.trim());
    let (pattern, effect) = match a.strip_prefix("match:namespace") {
        Some(p) => (p.trim(), b),
        None => (b.strip_prefix("namespace:").unwrap_or(b), a),
    };
    let mut w = effect.split_whitespace();
    let name = w.next()?;
    let arg = w.next().unwrap_or("on");
    let e = match name {
        "blur" => Effect::Blur(on(arg)),
        "noblur" | "no_blur" => Effect::Blur(!on(arg)),
        "ignore_alpha" | "ignorealpha" => Effect::Ignore(arg.parse().ok()?),
        "ignorezero" | "ignore_zero" => Effect::Ignore(0.0),
        "blur_popups" | "blurpopups" => Effect::Popups(on(arg)),
        _ => return None,
    };
    Some((whole(pattern)?, e))
}

/// `match:class RE, no_blur on`: the class pattern and whether it is blurred.
fn window_rule(v: &str) -> Option<(Regex, bool)> {
    let mut class = None;
    let mut blur = None;
    for part in v.split(',').map(str::trim) {
        if let Some(p) = part.strip_prefix("match:class") {
            class = Some(p.trim());
        } else if let Some(p) = part.strip_prefix("class:") {
            class = Some(p.trim());
        } else {
            let mut w = part.split_whitespace();
            match w.next() {
                Some("no_blur" | "noblur") => blur = Some(!on(w.next().unwrap_or("on"))),
                Some("blur") => blur = Some(on(w.next().unwrap_or("on"))),
                _ => {}
            }
        }
    }
    Some((whole(class?)?, blur?))
}

/// `hl.window_rule({ match = { class = '…' }, no_blur = false })`
fn glass_rule(line: &str) -> Option<(Regex, bool)> {
    use crate::hypr_lua::Val;
    let (name, args) = crate::hypr_lua::call(line, "hl.")?;
    if name != "window_rule" {
        return None;
    }
    let Some(Val::Table(t)) = args.first() else { return None };
    let Some(Val::Table(m)) = t.get("match") else { return None };
    let class = m.get("class")?.text();
    let blur = match t.get("no_blur")? {
        Val::Bool(b) => !b,
        _ => return None,
    };
    // the shell's own patterns are full matches already
    Some((Regex::new(&class).ok()?, blur))
}

/// `hl.config({ decoration = { blur = { size = 8, … } } })`
fn glass_blur(text: &str, s: &mut Settings) {
    use crate::hypr_lua::Val;
    let src: String = text.lines().filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join(" ");
    let Some((name, args)) = crate::hypr_lua::call(&src, "hl.") else { return };
    if name != "config" {
        return;
    }
    let Some(Val::Table(t)) = args.first() else { return };
    let Some(Val::Table(d)) = t.get("decoration") else { return };
    let Some(Val::Table(b)) = d.get("blur") else { return };
    let num = |k: &str| match b.get(k) {
        Some(Val::Num(n)) => Some(*n as f32),
        _ => None,
    };
    if let Some(v) = num("size") {
        s.size = v;
    }
    if let Some(v) = num("passes") {
        s.passes = v as u32;
    }
    if let Some(v) = num("noise") {
        s.noise = v;
    }
    if let Some(v) = num("contrast") {
        s.contrast = v;
    }
    if let Some(v) = num("brightness") {
        s.brightness = v;
    }
    if let Some(v) = num("vibrancy") {
        s.vibrancy = v;
    }
    if let Some(Val::Bool(x)) = b.get("xray") {
        s.xray = *x;
    }
}

// ---- GL ----

const HEAD: &str = r#"
#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
"#;

/// contrast, brightness and vibrancy (more saturation), before blurring
const PREPARE: &str = r#"
uniform float contrast;
uniform float brightness;
uniform float vibrancy;
void main() {
    vec4 c = texture2D(tex, v_coords);
    vec3 rgb = (c.rgb - 0.5) * contrast + 0.5;
    rgb *= brightness;
    float grey = dot(rgb, vec3(0.299, 0.587, 0.114));
    rgb = mix(vec3(grey), rgb, 1.0 + vibrancy);
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
"#;

const DOWN: &str = r#"
uniform vec2 halfpixel;
uniform float offset;
void main() {
    vec2 o = halfpixel * offset;
    vec4 sum = texture2D(tex, v_coords) * 4.0;
    sum += texture2D(tex, v_coords - o);
    sum += texture2D(tex, v_coords + o);
    sum += texture2D(tex, v_coords + vec2(o.x, -o.y));
    sum += texture2D(tex, v_coords - vec2(o.x, -o.y));
    gl_FragColor = sum / 8.0;
}
"#;

const UP: &str = r#"
uniform vec2 halfpixel;
uniform float offset;
void main() {
    vec2 o = halfpixel * offset;
    vec4 sum = texture2D(tex, v_coords + vec2(-o.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(-o.x, o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, o.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(o.x, o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(o.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(o.x, -o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, -o.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(-o.x, -o.y)) * 2.0;
    gl_FragColor = sum / 12.0;
}
"#;

/// The surface over its blurred background (premultiplied): the background where the surface's
/// alpha is above `ignore` (all of it when `ignore` < 0), rounded like the window when `radius` > 0.
const COMPOSITE: &str = r#"
uniform sampler2D blurred;
uniform vec2 out_size;
uniform vec2 elem_pos;
uniform vec2 elem_size;
uniform float ignore;
uniform float noise;
uniform vec2 geo_size;
uniform vec2 elem_off;
uniform float radius;
uniform float power;
void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif
    vec2 screen = elem_pos + v_coords * elem_size;
    vec3 bg = texture2D(blurred, screen / out_size).rgb;
    // grain from a small tile: mediump can be 16-bit (AMD), where a big dot() overflows to NaN (black)
    vec2 cell = mod(floor(screen), 64.0);
    bg += (fract(sin(dot(cell, vec2(12.9898, 78.233)) * 0.01) * 437.585) - 0.5) * noise;
    float m = (ignore < 0.0 || color.a > ignore) ? 1.0 : 0.0;
    // the outer edge's partly covered pixels (a 1 px border at 1.6) got the blurred wallpaper
    // (xray) as their backdrop over the window really under them: a bright 1 px line round an
    // opaque panel. An edge pixel (a neighbour 1.5 px away is see-through) keeps what is under it.
    // The same for a rounded window's corner (a title bar's: a light arc round it), where nothing is
    // see-through but the edge itself.
    if (m == 1.0 && color.a < 0.999 && (ignore >= 0.0 || radius > 0.0)) {
        vec2 d = 1.5 / elem_size;
        float n = min(min(texture2D(tex, v_coords + vec2(d.x, 0.0)).a, texture2D(tex, v_coords - vec2(d.x, 0.0)).a),
                      min(texture2D(tex, v_coords + vec2(0.0, d.y)).a, texture2D(tex, v_coords - vec2(0.0, d.y)).a));
        if (n <= max(ignore, 0.004)) m = 0.0;
    }
    // the rounded box's coverage of this pixel (1 inside, less on a corner's edge)
    float e = 1.0;
    if (radius > 0.0) {
        vec2 p = elem_off + v_coords * elem_size;
        vec2 q = abs(p - geo_size * 0.5) - (geo_size * 0.5 - vec2(radius));
        if (q.x > 0.0 && q.y > 0.0) {
            float d = pow(pow(q.x, power) + pow(q.y, power), 1.0 / power);
            e = clamp(radius - d + 0.5, 0.0, 1.0);
        }
    }
    // the blur fills only the part of the box the element leaves see-through: (1 - a) inside, and
    // on a corner's edge (where the element is cut round too) its coverage less the element's. As
    // (1 - a) * e an opaque title bar's half-covered corner pixels got a quarter of blurred wallpaper
    // (xray): a light arc round the corner
    vec4 outc = color * e + vec4(clamp(bg, 0.0, 1.0), 1.0) * m * max(e - color.a, 0.0);
    outc *= alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        outc = vec4(0.0, 0.2, 0.0, 0.2) + outc * 0.8;
#endif
    gl_FragColor = outc;
}
"#;

#[derive(Clone)]
struct Programs {
    prepare: GlesTexProgram,
    down: GlesTexProgram,
    up: GlesTexProgram,
    composite: GlesTexProgram,
}

/// One blurred background: what it was made from (`key`), the textures, a counter bumped each
/// time it is made again (so the surfaces over it are drawn anew).
#[derive(Default)]
struct Cached {
    key: u64,
    size: (i32, i32),
    src: Option<GlesTexture>,
    levels: Vec<GlesTexture>,
    counter: CommitCounter,
    used: bool,
}

#[derive(Default)]
struct Store {
    programs: Option<Programs>,
    failed: bool,
    cache: HashMap<String, Cached>,
}

fn store<R>(renderer: &mut GlesRenderer, f: impl FnOnce(&mut GlesRenderer, &mut Store) -> R) -> R {
    // kept outside the renderer while `f` uses it (it renders)
    let ctx = renderer.egl_context().user_data();
    ctx.insert_if_missing(|| RefCell::new(Store::default()));
    let mut s = std::mem::take(&mut *ctx.get::<RefCell<Store>>().expect("blur store").borrow_mut());
    let r = f(renderer, &mut s);
    *renderer.egl_context().user_data().get::<RefCell<Store>>().expect("blur store").borrow_mut() = s;
    r
}

fn programs(renderer: &mut GlesRenderer, s: &mut Store) -> Option<Programs> {
    if s.failed {
        return None;
    }
    if let Some(p) = &s.programs {
        return Some(p.clone());
    }
    let f = |n: &'static str| UniformName::new(n, UniformType::_1f);
    let v2 = |n: &'static str| UniformName::new(n, UniformType::_2f);
    let mut compile = |body: &str, u: &[UniformName<'_>]| match renderer.compile_custom_texture_shader(format!("{HEAD}{body}"), u) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("loom: a blur shader did not compile: {e:?}");
            None
        }
    };
    let p = (|| {
        Some(Programs {
            prepare: compile(PREPARE, &[f("contrast"), f("brightness"), f("vibrancy")])?,
            down: compile(DOWN, &[v2("halfpixel"), f("offset")])?,
            up: compile(UP, &[v2("halfpixel"), f("offset")])?,
            composite: compile(COMPOSITE, &[UniformName::new("blurred", UniformType::_1i), v2("out_size"), v2("elem_pos"), v2("elem_size"), f("ignore"), f("noise"), v2("geo_size"), v2("elem_off"), f("radius"), f("power")])?,
        })
    })();
    s.failed = p.is_none();
    s.programs = p.clone();
    p
}

/// What a blurred surface is: where it is and how its background shows.
#[derive(Clone, Copy, Debug)]
pub struct Mask {
    pub ignore: Option<f32>,
    /// the window's box and corners (radius 0: none)
    pub geo: Rectangle<i32, Physical>,
    pub radius: f32,
    pub power: f32,
}

/// Makes the blurred background behind each of `targets` (ranges of `out`, front to back, with
/// their masks) and puts each surface there over it. `wall` = where the wallpaper layers start
/// in `out`. Anything that fails leaves the surfaces as they were.
pub fn apply(renderer: &mut GlesRenderer, out: Vec<crate::render::GlesElement>, targets: &[(std::ops::Range<usize>, Mask)], wall: usize, output: &smithay::output::Output, settings: &Settings) -> Vec<crate::render::GlesElement> {
    if targets.is_empty() {
        return out;
    }
    let Some(mode) = output.current_mode() else { return out };
    let size = output.current_transform().transform_size(mode.size);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let made = store(renderer, |renderer, s| {
        let progs = programs(renderer, s)?;
        for c in s.cache.values_mut() {
            c.used = false;
        }
        // each target's background: its cache name, texture and counter
        let mut made = Vec::new();
        for (range, _) in targets {
            let behind = if settings.xray { wall.max(range.end) } else { range.end };
            // nothing behind (the wallpaper itself): drawn as it is
            if behind >= out.len() {
                made.push(None);
                continue;
            }
            // one blur per set of things behind: with xray every surface above the wallpaper shares one
            let name = if settings.xray { format!("{}:{}", output.name(), out.len() - behind) } else { format!("{}:{:?}", output.name(), out[range.start].id()) };
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (size.w, size.h, format!("{settings:?}")).hash(&mut h);
            for e in &out[behind..] {
                e.id().hash(&mut h);
                format!("{:?}", e.current_commit()).hash(&mut h);
                let g = e.geometry(scale);
                (g.loc.x, g.loc.y, g.size.w, g.size.h, e.alpha().to_bits()).hash(&mut h);
            }
            let key = h.finish();
            let c = s.cache.entry(name.clone()).or_default();
            c.used = true;
            if c.key != key || c.levels.is_empty() {
                // LOOM_BLUR_TRACE: how long each blur takes, GPU included
                let trace = std::env::var_os("LOOM_BLUR_TRACE").map(|_| std::time::Instant::now());
                if let Err(e) = blur_into(renderer, &progs, c, &out[behind..], (size.w, size.h), scale, settings) {
                    eprintln!("loom: blur: {e:?}");
                    return None;
                }
                if let Some(t) = trace {
                    let _ = renderer.with_context(|gl| unsafe { gl.Finish() });
                    eprintln!("loom: blur {name}: {} behind, {:.2} ms", out.len() - behind, t.elapsed().as_secs_f64() * 1000.0);
                }
                c.key = key;
                c.counter.increment();
            }
            made.push(Some((c.levels[0].clone(), c.counter, progs.composite.clone())));
        }
        s.cache.retain(|_, c| c.used);
        Some(made)
    });
    let Some(made) = made else { return out };
    let mut which = vec![None; out.len()];
    for (k, (range, mask)) in targets.iter().enumerate() {
        for i in range.clone() {
            which[i] = Some((k, *mask));
        }
    }
    out.into_iter()
        .zip(which)
        .map(|(e, w)| {
            let Some((k, mask)) = w else { return e };
            let Some((tex, counter, program)) = made[k].clone() else { return e };
            let inner = match e {
                crate::render::GlesElement::Plain(i) => Over::Surface(i),
                crate::render::GlesElement::Rounded(r) => Over::Surface(r.inner),
                crate::render::GlesElement::Bar(b) => Over::Bar(b),
                other => return other,
            };
            crate::render::GlesElement::Blurred(Blurred::new(inner, tex, counter, program, mask, (size.w, size.h), scale, settings.noise))
        })
        .collect()
}

fn blur_into(renderer: &mut GlesRenderer, p: &Programs, c: &mut Cached, behind: &[crate::render::GlesElement], size: (i32, i32), scale: Scale<f64>, s: &Settings) -> Result<(), GlesError> {
    let passes = s.passes.clamp(1, 8) as usize;
    if c.size != size || c.levels.len() != passes + 1 || c.src.is_none() {
        c.src = Some(renderer.create_buffer(Fourcc::Abgr8888, Size::<i32, Buffer>::from(size))?);
        c.levels = (0..=passes).map(|i| renderer.create_buffer(Fourcc::Abgr8888, Size::<i32, Buffer>::from(((size.0 >> i).max(1), (size.1 >> i).max(1))))).collect::<Result<_, _>>()?;
        c.size = size;
    }
    let full = Rectangle::<i32, Physical>::from_size(size.into());
    // what is behind, drawn as on the screen
    {
        let src = c.src.as_mut().expect("src");
        let mut target = renderer.bind(src)?;
        let mut frame = renderer.render(&mut target, size.into(), Transform::Normal)?;
        frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[full])?;
        for e in behind.iter().rev() {
            let dst = e.geometry(scale);
            e.draw(&mut frame, e.src(), dst, &[Rectangle::from_size(dst.size)], &[])?;
        }
        let _ = frame.finish()?;
    }
    let src = c.src.clone().expect("src");
    pass(renderer, &src, &mut c.levels[0], &p.prepare, vec![Uniform::new("contrast", s.contrast), Uniform::new("brightness", s.brightness), Uniform::new("vibrancy", s.vibrancy)])?;
    for i in 0..passes {
        let (a, b) = c.levels.split_at_mut(i + 1);
        let d = b[0].size();
        pass(renderer, &a[i], &mut b[0], &p.down, vec![Uniform::new("halfpixel", (0.5 / d.w as f32, 0.5 / d.h as f32)), Uniform::new("offset", s.size)])?;
    }
    for i in (0..passes).rev() {
        let (a, b) = c.levels.split_at_mut(i + 1);
        let d = a[i].size();
        pass(renderer, &b[0], &mut a[i], &p.up, vec![Uniform::new("halfpixel", (0.5 / d.w as f32, 0.5 / d.h as f32)), Uniform::new("offset", s.size)])?;
    }
    // LOOM_BLUR_DUMP=dir: each texture as raw RGBA (debugging)
    if let Ok(dir) = std::env::var("LOOM_BLUR_DUMP") {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        eprintln!("loom: blur {n}: {} behind", behind.len());
        let src = c.src.as_mut().expect("src");
        dump(renderer, src, &format!("{dir}/{n:03}-src"));
        for (i, t) in c.levels.iter_mut().enumerate().take(1) {
            dump(renderer, t, &format!("{dir}/{n:03}-level{i}"));
        }
    }
    Ok(())
}

/// `from` drawn over all of `to` through `program`.
fn pass(renderer: &mut GlesRenderer, from: &GlesTexture, to: &mut GlesTexture, program: &GlesTexProgram, uniforms: Vec<Uniform<'static>>) -> Result<(), GlesError> {
    let ts = to.size();
    let fs = from.size();
    let dst = Rectangle::<i32, Physical>::from_size((ts.w, ts.h).into());
    let mut target = renderer.bind(to)?;
    let mut frame = renderer.render(&mut target, dst.size, Transform::Normal)?;
    frame.render_texture_from_to(from, Rectangle::from_size((fs.w as f64, fs.h as f64).into()), dst, &[dst], &[], Transform::Normal, 1.0, Some(program), &uniforms)?;
    let _ = frame.finish()?;
    Ok(())
}

/// A surface drawn over its blurred background.
/// What is drawn over the blur: a window's surface, or its title bar (ii-decor's glass).
pub enum Over {
    Surface(WaylandSurfaceRenderElement<GlesRenderer>),
    Bar(smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>),
}

macro_rules! over {
    ($s:expr, $e:ident => $body:expr) => {
        match $s {
            Over::Surface($e) => $body,
            Over::Bar($e) => $body,
        }
    };
}

pub struct Blurred {
    inner: Over,
    tex: GlesTexture,
    counter: CommitCounter,
    program: GlesTexProgram,
    mask: Mask,
    out_size: (i32, i32),
    noise: f32,
    scale: Scale<f64>,
}

impl Blurred {
    #[allow(clippy::too_many_arguments)]
    fn new(inner: Over, tex: GlesTexture, counter: CommitCounter, program: GlesTexProgram, mask: Mask, out_size: (i32, i32), scale: Scale<f64>, noise: f32) -> Blurred {
        Blurred { inner, tex, counter, program, mask, out_size, noise, scale }
    }
}

impl Element for Blurred {
    fn id(&self) -> &Id {
        over!(&self.inner, e => e.id())
    }
    // a new background counts as a change of the surface: both counters in one
    fn current_commit(&self) -> CommitCounter {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}{:?}", over!(&self.inner, e => e.current_commit()), self.counter).hash(&mut h);
        CommitCounter::from(h.finish() as usize)
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        over!(&self.inner, e => e.src())
    }
    fn transform(&self) -> Transform {
        over!(&self.inner, e => e.transform())
    }
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        over!(&self.inner, e => e.geometry(scale))
    }
    // all of it again on any change: the background under it is part of the picture
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit == Some(self.current_commit()) {
            return DamageSet::default();
        }
        DamageSet::from_slice(&[Rectangle::from_size(self.geometry(scale).size)])
    }
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }
    fn alpha(&self) -> f32 {
        over!(&self.inner, e => e.alpha())
    }
    fn kind(&self) -> Kind {
        over!(&self.inner, e => e.kind())
    }
}

impl RenderElement<GlesRenderer> for Blurred {
    fn draw(&self, frame: &mut GlesFrame<'_, '_>, src: Rectangle<f64, Buffer>, dst: Rectangle<i32, Physical>, damage: &[Rectangle<i32, Physical>], opaque: &[Rectangle<i32, Physical>]) -> Result<(), GlesError> {
        let tex = self.tex.tex_id();
        frame.with_context(|gl| unsafe {
            use smithay::backend::renderer::gles::ffi;
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, tex);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.ActiveTexture(ffi::TEXTURE0);
        })?;
        let m = &self.mask;
        let at = over!(&self.inner, e => e.geometry(self.scale)).loc;
        let uniforms = vec![
            Uniform::new("blurred", 1i32),
            Uniform::new("out_size", (self.out_size.0 as f32, self.out_size.1 as f32)),
            Uniform::new("elem_pos", (dst.loc.x as f32, dst.loc.y as f32)),
            Uniform::new("elem_size", (dst.size.w as f32, dst.size.h as f32)),
            Uniform::new("ignore", m.ignore.unwrap_or(-1.0)),
            Uniform::new("noise", self.noise),
            Uniform::new("geo_size", (m.geo.size.w as f32, m.geo.size.h as f32)),
            Uniform::new("elem_off", ((at.x - m.geo.loc.x) as f32, (at.y - m.geo.loc.y) as f32)),
            Uniform::new("radius", m.radius),
            Uniform::new("power", m.power),
        ];
        frame.override_default_tex_program(self.program.clone(), uniforms);
        let r = over!(&self.inner, e => e.draw(frame, src, dst, damage, opaque));
        frame.clear_tex_program_override();
        frame.with_context(|gl| unsafe {
            use smithay::backend::renderer::gles::ffi;
            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.ActiveTexture(ffi::TEXTURE0);
        })?;
        r
    }
    // never straight onto a plane: the shader would be skipped
    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_users_rules() {
        let (re, e) = layer_rule("match:namespace quickshell:.*, ignore_alpha 0.79").unwrap();
        assert!(re.is_match("quickshell:bar") && matches!(e, Effect::Ignore(a) if (a - 0.79).abs() < 1e-6));
        let (re, e) = layer_rule("blur, waybar").unwrap();
        assert!(re.is_match("waybar") && !re.is_match("waybar2") && matches!(e, Effect::Blur(true)));
        assert!(matches!(layer_rule("match:namespace quickshell:screenDrawFull, blur off").unwrap().1, Effect::Blur(false)));
        let (re, b) = window_rule("match:class .*, no_blur on").unwrap();
        assert!(re.is_match("zash") && !b);
        let (re, b) = glass_rule("hl.window_rule({ name = 'glass-zash', match = { class = '^(zash)$' }, no_blur = false, border_size = 0 })").unwrap();
        assert!(re.is_match("zash") && b);
        let mut s = Settings::default();
        glass_blur("-- x\nhl.config({ decoration = {\n  blur = { size = 8, passes = 3, noise = 0.060, xray = true },\n} })", &mut s);
        assert_eq!((s.size, s.passes, s.xray), (8.0, 3, true));
    }
}

/// `t` as raw RGBA to `path`.WxH.rgba (debugging).
fn dump(renderer: &mut GlesRenderer, t: &mut GlesTexture, path: &str) {
    use smithay::backend::renderer::ExportMem;
    let s = t.size();
    let Ok(target) = renderer.bind(t) else { return };
    let Ok(m) = renderer.copy_framebuffer(&target, Rectangle::from_size(s), Fourcc::Abgr8888) else { return };
    drop(target);
    if let Ok(px) = renderer.map_texture(&m) {
        let _ = std::fs::write(format!("{path}.{}x{}.rgba", s.w, s.h), px);
    }
}
