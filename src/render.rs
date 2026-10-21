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
}

pub fn elements<R>(state: &Loom, renderer: &mut R, output: &Output, now: Instant) -> Vec<R::Elem>
where
    R: LoomRenderer,
    <R as smithay::backend::renderer::RendererSuper>::TextureId: Clone + Texture + 'static,
{
    let mut out = Vec::new();
    // the output's scale (1.6 on the Z13 panel): positions are logical, drawing is in pixels
    let os = output.current_scale().fractional_scale();
    let map = layer_map_for_output(output);
    let layers = |out: &mut Vec<R::Elem>, renderer: &mut R, which: &[Layer]| {
        for l in which {
            for layer in map.layers_on(*l).rev() {
                let Some(geo) = map.layer_geometry(layer) else { continue };
                let loc: Point<i32, Physical> = geo.loc.to_f64().to_physical(os).to_i32_round();
                out.extend(AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(layer, renderer, loc, Scale::from(os), state.opacity_of(layer.wl_surface())).into_iter().map(R::plain));
            }
        }
    };
    layers(&mut out, renderer, &[Layer::Overlay, Layer::Top]);
    let look = &state.look;
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
        // grown or shrunk around its middle; the surface's own offset (its shadow, CSD) kept
        let (sw, sh) = (geo.size.w as f64, geo.size.h as f64);
        let (gx, gy) = (x + sw * (1.0 - scale) / 2.0, y + sh * (1.0 - scale) / 2.0);
        let loc: Point<i32, Physical> = Point::from((((gx - geo.loc.x as f64 * scale) * os).round() as i32, ((gy - geo.loc.y as f64 * scale) * os).round() as i32));
        // the window's box on screen, for its corners; none for a fullscreen window
        let boxed = Rectangle::<i32, Physical>::new(((gx * os).round() as i32, (gy * os).round() as i32).into(), ((sw * scale * os).round() as i32, (sh * scale * os).round() as i32).into());
        let radius = if state.fullscreen.as_ref() == Some(w) { 0.0 } else { look.rounding * (scale * os) as f32 };
        let a = alpha as f32 * smithay::wayland::seat::WaylandFocus::wl_surface(w).map_or(1.0, |s| state.opacity_of(&s));
        let es = AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(w, renderer, loc, Scale::from(scale * os), a);
        for e in es {
            // the window's own surfaces are rounded; a popup or menu reaching past its box is not
            let g = e.geometry(Scale::from(os));
            let inside = g.loc.x >= boxed.loc.x - 1 && g.loc.y >= boxed.loc.y - 1 && g.loc.x + g.size.w <= boxed.loc.x + boxed.size.w + 1 && g.loc.y + g.size.h <= boxed.loc.y + boxed.size.h + 1;
            out.push(if radius > 0.5 && inside { renderer.rounded(e, boxed, Scale::from(os), radius, look.rounding_power) } else { R::plain(e) });
        }
    }
    layers(&mut out, renderer, &[Layer::Bottom, Layer::Background]);
    out
}
