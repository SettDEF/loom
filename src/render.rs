//! What one output shows, front to back: the layers above windows (overlay, top), the windows
//! topmost first (each where its animation has it, faded and scaled, its corners rounded), the
//! layers below (bottom, background). Any renderer (pixman headless, GL nested and on hardware),
//! at the output's scale; rounding needs GL (pixman draws square corners).

use std::time::Instant;

use smithay::{
    backend::renderer::{
        element::{surface::WaylandSurfaceRenderElement, AsRenderElements, Element, RenderElement},
        gles::GlesRenderer,
        pixman::PixmanRenderer,
        ImportAll, Renderer, Texture,
    },
    desktop::layer_map_for_output,
    output::Output,
    utils::{Physical, Point, Rectangle, Scale},
    wayland::shell::wlr_layer::Layer,
};

use crate::state::Loom;

smithay::backend::renderer::element::render_elements! {
    pub GlesElement<=GlesRenderer>;
    Plain=WaylandSurfaceRenderElement<GlesRenderer>,
    Rounded=crate::rounding::Rounded,
    Bar=smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>,
    Ghost=smithay::backend::renderer::element::texture::TextureRenderElement<smithay::backend::renderer::gles::GlesTexture>,
    Blurred=crate::blur::Blurred,
    Frame=smithay::backend::renderer::gles::element::PixelShaderElement,
    Shaded=smithay::backend::renderer::gles::element::TextureShaderElement,
}

/// How a renderer draws a window's surfaces: GL rounds their corners, pixman does not.
pub trait LoomRenderer: Renderer + ImportAll + Sized
where
    <Self as smithay::backend::renderer::RendererSuper>::TextureId: Clone + Texture + 'static,
{
    type Elem: RenderElement<Self>;
    fn plain(e: WaylandSurfaceRenderElement<Self>) -> Self::Elem;
    fn rounded(&mut self, e: WaylandSurfaceRenderElement<Self>, _geo: Rectangle<i32, Physical>, _scale: Scale<f64>, _radius: f32, _power: f32) -> Self::Elem {
        Self::plain(e)
    }
    /// A picture loom drew (a title bar): GL only.
    fn picture(&mut self, _buf: &smithay::backend::renderer::element::memory::MemoryRenderBuffer, _at: Point<f64, Physical>, _size: smithay::utils::Size<i32, smithay::utils::Logical>, _px: (i32, i32), _alpha: f32) -> Option<Self::Elem> {
        None
    }
    /// A closed window's last frame (KDE's close effects): GL only.
    fn ghost(&mut self, _g: &crate::kwin_fx::glue::Ghost, _at: Point<f64, Physical>, _size: smithay::utils::Size<i32, smithay::utils::Logical>, _alpha: f32) -> Option<Self::Elem> {
        None
    }
    /// The surfaces in `targets` drawn over a blur of what is behind them (`wall`: where the
    /// wallpaper layers start): GL only.
    fn blur(&mut self, out: Vec<Self::Elem>, _targets: &[(std::ops::Range<usize>, crate::blur::Mask)], _wall: usize, _output: &Output, _s: &crate::blur::Settings) -> Vec<Self::Elem> {
        out
    }
    /// A window's border and shadow (frames.rs): GL only.
    #[allow(clippy::too_many_arguments)]
    fn frame(&mut self, _cfg: &crate::frames::Config, _addr: u64, _boxed: Rectangle<i32, Physical>, _radius: f32, _scale: f64, _active: bool, _bordered: bool, _alpha: f32) -> Option<Self::Elem> {
        None
    }
    /// The screen drawn through the screen shader at `path` (shader.rs): GL only.
    fn shade(&mut self, out: Vec<Self::Elem>, _output: &Output, _path: &str) -> Vec<Self::Elem> {
        out
    }
}

impl LoomRenderer for PixmanRenderer {
    type Elem = WaylandSurfaceRenderElement<PixmanRenderer>;
    fn plain(e: WaylandSurfaceRenderElement<Self>) -> Self::Elem {
        e
    }
}

impl LoomRenderer for GlesRenderer {
    type Elem = GlesElement;
    fn plain(e: WaylandSurfaceRenderElement<Self>) -> Self::Elem {
        GlesElement::Plain(e)
    }
    fn rounded(&mut self, e: WaylandSurfaceRenderElement<Self>, geo: Rectangle<i32, Physical>, scale: Scale<f64>, radius: f32, power: f32) -> Self::Elem {
        match crate::rounding::program(self) {
            Some(p) => GlesElement::Rounded(crate::rounding::Rounded::new(e, geo, scale, radius, power, p)),
            None => GlesElement::Plain(e),
        }
    }
    // the picture is drawn at pixel density already: `size` (logical) keeps it from being scaled again
    fn picture(&mut self, buf: &smithay::backend::renderer::element::memory::MemoryRenderBuffer, at: Point<f64, Physical>, size: smithay::utils::Size<i32, smithay::utils::Logical>, px: (i32, i32), alpha: f32) -> Option<Self::Elem> {
        // the whole picture (`px`: its pixels, which are its buffer units at buffer scale 1) shown at `size`
        let src = smithay::utils::Rectangle::<f64, smithay::utils::Logical>::from_size((px.0 as f64, px.1 as f64).into());
        smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement::from_buffer(self, at, buf, Some(alpha), Some(src), Some(size), smithay::backend::renderer::element::Kind::Unspecified).ok().map(GlesElement::Bar)
    }
    fn ghost(&mut self, g: &crate::kwin_fx::glue::Ghost, at: Point<f64, Physical>, size: smithay::utils::Size<i32, smithay::utils::Logical>, alpha: f32) -> Option<Self::Elem> {
        let src = smithay::utils::Rectangle::<f64, smithay::utils::Logical>::from_size((g.px.0 as f64, g.px.1 as f64).into());
        Some(GlesElement::Ghost(smithay::backend::renderer::element::texture::TextureRenderElement::from_texture_buffer(at, &g.buffer, Some(alpha), Some(src), Some(size), smithay::backend::renderer::element::Kind::Unspecified)))
    }
    fn blur(&mut self, out: Vec<Self::Elem>, targets: &[(std::ops::Range<usize>, crate::blur::Mask)], wall: usize, output: &Output, s: &crate::blur::Settings) -> Vec<Self::Elem> {
        crate::blur::apply(self, out, targets, wall, output, s)
    }
    fn frame(&mut self, cfg: &crate::frames::Config, addr: u64, boxed: Rectangle<i32, Physical>, radius: f32, scale: f64, active: bool, bordered: bool, alpha: f32) -> Option<Self::Elem> {
        crate::frames::element(self, cfg, addr, boxed, radius, scale, active, bordered, alpha).map(GlesElement::Frame)
    }
    fn shade(&mut self, out: Vec<Self::Elem>, output: &Output, path: &str) -> Vec<Self::Elem> {
        match crate::shader::apply(self, out, output, path) {
            Ok(e) => vec![GlesElement::Shaded(e)],
            Err(out) => out,
        }
    }
}

pub fn elements<R>(state: &Loom, renderer: &mut R, output: &Output, now: Instant) -> Vec<R::Elem>
where
    R: LoomRenderer,
    <R as smithay::backend::renderer::RendererSuper>::TextureId: Clone + Texture + 'static,
{
    let mut out = Vec::new();
    // the output's scale (1.6 on the Z13 panel): positions are logical, drawing is in pixels
    let os = output.current_scale().fractional_scale();
    // where this screen is in compositor space, and how big (logical): windows are drawn relative to it
    let og = state.space.output_geometry(output).unwrap_or_default();
    let (ox, oy) = (og.loc.x as f64, og.loc.y as f64);
    // the login screen's last picture, over everything until the shell is up (curtain.rs)
    if let Some((c, a)) = state.curtain.as_ref().and_then(|c| Some((c, c.alpha(now)?))) {
        if let Some((buf, px)) = c.picture(&output.name()) {
            out.extend(renderer.picture(&buf, (0.0, 0.0).into(), og.size, px, a));
        }
    }
    if state.locked() {
        let lock = state.lock_surface_on(&output.name());
        if let Some(s) = lock {
            out.extend(smithay::backend::renderer::element::surface::render_elements_from_surface_tree(renderer, s, Point::<i32, Physical>::from((0, 0)), Scale::from(os), 1.0, smithay::backend::renderer::element::Kind::Unspecified).into_iter().map(R::plain));
        }
        // misc:session_lock_xray: the desktop under the lock screen, as Hyprland draws it; only
        // under a lock surface: a screen without one (just plugged, the locker crashed) stays black
        if !state.look.lock_xray || lock.is_none() {
            return out;
        }
    }

    let map = layer_map_for_output(output);
    // what gets drawn over a blur of what is behind it (ranges of `out`)
    let mut targets: Vec<(std::ops::Range<usize>, crate::blur::Mask)> = Vec::new();
    let layers = |out: &mut Vec<R::Elem>, targets: &mut Vec<(std::ops::Range<usize>, crate::blur::Mask)>, renderer: &mut R, which: &[Layer]| {
        let screen = output.current_mode().map(|m| output.current_transform().transform_size(m.size).to_f64().to_logical(os).to_i32_round::<i32>()).unwrap_or_default();
        for l in which {
            // closed layers animating out, in front of their level
            for g in state.layer_ghosts.iter().filter(|g| g.layer == *l && g.output == output.name()) {
                let lk = g.anim.look(now, g.geo, screen);
                let (w, h) = (g.ghost.geo.size.w * lk.scale, g.ghost.geo.size.h * lk.scale);
                let at = (g.ghost.geo.loc.x + lk.dx + (g.ghost.geo.size.w - w) / 2.0, g.ghost.geo.loc.y + lk.dy + (g.ghost.geo.size.h - h) / 2.0);
                if lk.alpha > 0.0 && w >= 1.0 && h >= 1.0 {
                    out.extend(renderer.ghost(&g.ghost, ((at.0 * os).round(), (at.1 * os).round()).into(), (w.round() as i32, h.round() as i32).into(), lk.alpha as f32));
                }
            }
            for layer in map.layers_on(*l).rev() {
                let Some(geo) = map.layer_geometry(layer) else { continue };
                // its opening animation: moved, grown around its middle, faded in
                let lk = state.layer_look(&smithay::reexports::wayland_server::Resource::id(layer.wl_surface()), geo, now, output);
                let (gx, gy) = (geo.loc.x as f64 + lk.dx + geo.size.w as f64 * (1.0 - lk.scale) / 2.0, geo.loc.y as f64 + lk.dy + geo.size.h as f64 * (1.0 - lk.scale) / 2.0);
                let loc: Point<i32, Physical> = Point::from(((gx * os).round() as i32, (gy * os).round() as i32));
                let start = out.len();
                out.extend(AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(layer, renderer, loc, Scale::from(os * lk.scale), state.opacity_of(layer.wl_surface()) * lk.alpha as f32).into_iter().map(R::plain));
                if let Some(b) = state.blur.layer(layer.namespace()) {
                    let g = Rectangle::new(loc, geo.size.to_f64().to_physical(os).to_i32_round());
                    targets.push((start..out.len(), crate::blur::Mask { ignore: b.ignore, geo: g, radius: 0.0, power: 2.0 }));
                }
            }
        }
    };
    // a fullscreen window here covers the bar and dock (Top), as on Hyprland; Overlay stays above it
    let covered = state.fullscreen_output().as_deref() == Some(output.name().as_str());
    layers(&mut out, &mut targets, renderer, if covered { &[Layer::Overlay] } else { &[Layer::Overlay, Layer::Top] });
    let look = &state.look;
    // closed windows KDE's effects still animate: in front, as their last frame
    for g in &state.ghosts {
        let Some(fx) = &state.fx else { break };
        let (x, y, gw, gh) = (g.geo.loc.x, g.geo.loc.y, g.geo.size.w, g.geo.size.h);
        let s = fx.shape(g.addr, (x, y, gw, gh), now);
        let (gx, gy) = (x - ox + s.translate.0 + gw * (1.0 - s.scale.0) / 2.0, y - oy + s.translate.1 + gh * (1.0 - s.scale.1) / 2.0);
        let size = ((gw * s.scale.0).round() as i32, (gh * s.scale.1).round() as i32);
        if size.0 > 0 && size.1 > 0 && s.opacity > 0.0 {
            out.extend(renderer.ghost(g, ((gx * os).round(), (gy * os).round()).into(), size.into(), s.opacity as f32));
        }
    }
    // closed windows animating out (loom's own), in front, as their last frame
    for g in &state.win_ghosts {
        let screen = og.size;
        let lk = g.anim.look(now, g.geo, screen);
        let (gw, gh) = (g.ghost.geo.size.w * lk.scale, g.ghost.geo.size.h * lk.scale);
        let at = (g.ghost.geo.loc.x - ox + lk.dx + (g.ghost.geo.size.w - gw) / 2.0, g.ghost.geo.loc.y - oy + lk.dy + (g.ghost.geo.size.h - gh) / 2.0);
        if lk.alpha > 0.0 && gw >= 1.0 && gh >= 1.0 && at.0 + gw > 0.0 && at.1 + gh > 0.0 && at.0 < screen.w as f64 && at.1 < screen.h as f64 {
            out.extend(renderer.ghost(&g.ghost, ((at.0 * os).round(), (at.1 * os).round()).into(), (gw.round() as i32, gh.round() as i32).into(), lk.alpha as f32));
        }
    }
    for w in state.space.elements().rev() {
        let geo = w.geometry();
        let (x, y, alpha, scale) = match state.anim_of(w) {
            Some(a) if a.waiting => continue,
            Some(a) => (a.x.value(now), a.y.value(now), a.alpha.value(now), a.scale.value(now)),
            None => {
                let loc = state.space.element_location(w).unwrap_or_default();
                (loc.x as f64, loc.y as f64, 1.0, 1.0)
            }
        };
        // KDE's effects on top of loom's own: scaled per axis, faded, moved
        let (sw, sh) = (geo.size.w as f64, geo.size.h as f64);
        let fx = match (&state.fx, state.managed_of(w)) {
            (Some(f), Some(m)) => f.shape(m.addr, (x, y, sw, sh), now),
            _ => Default::default(),
        };
        let (x, y, alpha) = (x + fx.translate.0 - ox, y + fx.translate.1 - oy, alpha * fx.opacity);
        // not on this screen: not drawn here
        if x + sw <= 0.0 || y + sh <= 0.0 || x >= og.size.w as f64 || y >= og.size.h as f64 {
            continue;
        }
        let (sx, sy) = (scale * fx.scale.0, scale * fx.scale.1);
        // grown or shrunk around its middle; the surface's own offset (its shadow, CSD) kept
        let (gx, gy) = (x + sw * (1.0 - sx) / 2.0, y + sh * (1.0 - sy) / 2.0);
        let loc: Point<i32, Physical> = Point::from((((gx - geo.loc.x as f64 * sx) * os).round() as i32, ((gy - geo.loc.y as f64 * sy) * os).round() as i32));
        // its title bar (ii-decor) above it, or a pill over a corner of it: drawn in front of it
        let kind = state.decor_kind(w);
        let bh = match &kind { crate::decor::Kind::Bar(h) => *h as f64, _ => 0.0 };
        let radius_l = if state.fullscreen.as_ref() == Some(w) { 0.0 } else { look.rounding };
        if !matches!(kind, crate::decor::Kind::None) {
            let addr = state.managed_of(w).map(|m| m.addr).unwrap_or(0);
            let (class, title) = (state.class_of(w), state.title_of(w));
            let mut d = state.decor.borrow_mut();
            // where it goes and how big it is (logical, the animation's scale applied)
            let (at, size_l) = match &kind {
                crate::decor::Kind::Overlay(o) => {
                    let pw = crate::decor::pill_width(&d.conf, o) as f64;
                    let px = if o.right { gx + sw * sx - (o.x as f64 + pw) * sx } else { gx + o.x as f64 * sx };
                    let py = if o.top { gy + o.y as f64 * sy } else { gy + sh * sy - (o.y as f64 + o.height as f64) * sy };
                    ((px, py), (pw * sx, o.height as f64 * sy))
                }
                _ => ((gx, gy - bh * sy), (sw * sx, bh * sy)),
            };
            if let Some(buf) = d.picture(addr, &class, &title, &kind, sw as f32, os * sx.max(sy), radius_l) {
                // its logical size, so the picture (drawn at pixel density) is not scaled twice
                let logical: smithay::utils::Size<i32, smithay::utils::Logical> = (size_l.0.round() as i32, size_l.1.round() as i32).into();
                if let Some(e) = renderer.picture(buf, ((at.0 * os).round(), (at.1 * os).round()).into(), logical, ((size_l.0 * os).round() as i32, (size_l.1 * os).round() as i32), alpha as f32) {
                    // glass (ii-decor's blur): the window behind the bar blurred, in the window's rounded box
                    if d.conf.blur && matches!(kind, crate::decor::Kind::Bar(_)) && state.fullscreen.as_ref() != Some(w) {
                        let boxed = Rectangle::<i32, Physical>::new(((gx * os).round() as i32, ((gy - bh * sy) * os).round() as i32).into(), ((sw * sx * os).round() as i32, ((sh + bh) * sy * os).round() as i32).into());
                        let r = look.rounding * (sx.min(sy) * os) as f32;
                        targets.push((out.len()..out.len() + 1, crate::blur::Mask { ignore: None, geo: boxed, radius: if r > 0.5 { r } else { 0.0 }, power: look.rounding_power }));
                    }
                    out.push(e);
                }
            }
        }
        // the window's box on screen (its bar included), for its corners; none for a fullscreen window
        let boxed = Rectangle::<i32, Physical>::new(((gx * os).round() as i32, ((gy - bh * sy) * os).round() as i32).into(), ((sw * sx * os).round() as i32, ((sh + bh) * sy * os).round() as i32).into());
        let radius = if state.fullscreen.as_ref() == Some(w) { 0.0 } else { look.rounding * (sx.min(sy) * os) as f32 };
        let a = alpha as f32 * smithay::wayland::seat::WaylandFocus::wl_surface(w).map_or(1.0, |s| state.opacity_of(&s));
        let es = AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(w, renderer, loc, Scale::from((sx * os, sy * os)), a);
        let main_id = smithay::wayland::seat::WaylandFocus::wl_surface(w).map(|s| smithay::backend::renderer::element::Id::from_wayland_resource(&*s));
        // see-through apps the Glass rules name: blurred behind (their own surfaces, not popups)
        let blurred = state.blur.window(&state.class_of(w)) && state.fullscreen.as_ref() != Some(w);
        for e in es {
            // the window's own surfaces are rounded; a popup or menu reaching past its box is not
            let g = e.geometry(Scale::from(os));
            let inside = g.loc.x >= boxed.loc.x - 1 && g.loc.y >= boxed.loc.y - 1 && g.loc.x + g.size.w <= boxed.loc.x + boxed.size.w + 1 && g.loc.y + g.size.h <= boxed.loc.y + boxed.size.h + 1;
            // the window's own surface drawn bigger than its box (a client's shadow margin; Zen fills it
            // black with a video): cut to the box too, not shown around the window
            let own = main_id.as_ref() == Some(smithay::backend::renderer::element::Element::id(&e));
            let inside = inside || own;
            if blurred && inside {
                let at = out.len();
                targets.push((at..at + 1, crate::blur::Mask { ignore: None, geo: boxed, radius: if radius > 0.5 { radius } else { 0.0 }, power: look.rounding_power }));
            }
            out.push(if radius > 0.5 && inside { renderer.rounded(e, boxed, Scale::from(os), radius, look.rounding_power) } else { R::plain(e) });
        }
        // its border and shadow, behind it (none fullscreen; no border for the Glass apps)
        if state.fullscreen.as_ref() != Some(w) {
            if let Some(m) = state.managed_of(w) {
                let active = state.focused.as_ref() == Some(w);
                let bordered = !state.blur.window(&state.class_of(w));
                out.extend(renderer.frame(&state.frames_cfg, m.addr, boxed, radius, os * sx.min(sy), active, bordered, a));
            }
        }
    }
    let wall = out.len();
    layers(&mut out, &mut targets, renderer, &[Layer::Bottom, Layer::Background]);
    let out = if targets.is_empty() { out } else { renderer.blur(out, &targets, wall, output, &state.blur.settings) };
    // the screen shader last, over all of it (the cursor stays outside it)
    renderer.shade(out, output, &state.screen_shader())
}

