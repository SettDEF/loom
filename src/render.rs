//! What one output shows, front to back: the layers above windows (overlay, top), the windows
//! topmost first (each where its animation has it, faded and scaled), the layers below (bottom,
//! background). Any renderer: pixman headless, GL on hardware later.

use std::time::Instant;

use smithay::{
    backend::renderer::{
        element::{surface::WaylandSurfaceRenderElement, AsRenderElements},
        ImportAll, Renderer, Texture,
    },
    desktop::layer_map_for_output,
    output::Output,
    utils::{Physical, Point, Scale},
    wayland::shell::wlr_layer::Layer,
};

use crate::state::Loom;

pub fn elements<R>(state: &Loom, renderer: &mut R, output: &Output, now: Instant) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + Texture + 'static,
{
    let mut out = Vec::new();
    let map = layer_map_for_output(output);
    let layers = |out: &mut Vec<WaylandSurfaceRenderElement<R>>, renderer: &mut R, which: &[Layer]| {
        for l in which {
            for layer in map.layers_on(*l).rev() {
                let Some(geo) = map.layer_geometry(layer) else { continue };
                let loc: Point<i32, Physical> = geo.loc.to_physical(1);
                out.extend(AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(layer, renderer, loc, Scale::from(1.0), state.opacity_of(layer.wl_surface())));
            }
        }
    };
    layers(&mut out, renderer, &[Layer::Overlay, Layer::Top]);
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
        let x = x + sw * (1.0 - scale) / 2.0 - geo.loc.x as f64 * scale;
        let y = y + sh * (1.0 - scale) / 2.0 - geo.loc.y as f64 * scale;
        let loc: Point<i32, Physical> = Point::from((x.round() as i32, y.round() as i32));
        out.extend(AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(w, renderer, loc, Scale::from(scale), alpha as f32 * smithay::wayland::seat::WaylandFocus::wl_surface(w).map_or(1.0, |s| state.opacity_of(&s))));
    }
    layers(&mut out, renderer, &[Layer::Bottom, Layer::Background]);
    out
}
