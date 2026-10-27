//! X per screen, as Hyprland's force_zero_scaling: X sees each screen at its real pixels, side by
//! side, and each X window is drawn at the scale of the screen its middle is on (1:1 there).

use std::sync::Arc;

use smithay::utils::{Logical, Physical, Rectangle};

use crate::state::Loom;

/// One screen: where it is (logical), its scale, where X sees it (pixels).
#[derive(Debug, Clone, Copy)]
struct Screen {
    at: Rectangle<i32, Logical>,
    scale: f64,
    x: Rectangle<i32, Physical>,
}

/// The X layout: smithay's xwm maps every X rectangle through it.
#[derive(Debug, Default)]
pub struct XLayout {
    screens: Vec<Screen>,
}

/// squared distance from `p` to the rectangle (0 inside)
fn dist(p: (f64, f64), r: (f64, f64, f64, f64)) -> f64 {
    let dx = (r.0 - p.0).max(0.0).max(p.0 - (r.0 + r.2));
    let dy = (r.1 - p.1).max(0.0).max(p.1 - (r.1 + r.3));
    dx * dx + dy * dy
}

impl XLayout {
    /// screens left to right (logical x), each placed in X right after the last at its pixel size
    fn new(mut v: Vec<(Rectangle<i32, Logical>, f64)>) -> Self {
        v.sort_by_key(|(r, _)| (r.loc.x, r.loc.y));
        let mut x = 0;
        let screens = v
            .into_iter()
            .map(|(at, scale)| {
                let size = at.size.to_f64().upscale(scale).to_i32_round();
                let s = Screen { at, scale, x: Rectangle::new((x, (at.loc.y as f64 * scale).round() as i32).into(), (size.w, size.h).into()) };
                x += size.w;
                s
            })
            .collect();
        XLayout { screens }
    }

    /// the screen a logical rectangle's middle is on (or the nearest)
    fn by_logical(&self, r: Rectangle<i32, Logical>) -> Option<&Screen> {
        let m = (r.loc.x as f64 + r.size.w as f64 / 2.0, r.loc.y as f64 + r.size.h as f64 / 2.0);
        self.screens.iter().min_by(|a, b| {
            let d = |s: &Screen| dist(m, (s.at.loc.x as f64, s.at.loc.y as f64, s.at.size.w as f64, s.at.size.h as f64));
            d(a).total_cmp(&d(b))
        })
    }

    /// the screen an X rectangle's middle is on (or the nearest)
    fn by_x(&self, r: Rectangle<i32, Physical>) -> Option<&Screen> {
        let m = (r.loc.x as f64 + r.size.w as f64 / 2.0, r.loc.y as f64 + r.size.h as f64 / 2.0);
        self.screens.iter().min_by(|a, b| {
            let d = |s: &Screen| dist(m, (s.x.loc.x as f64, s.x.loc.y as f64, s.x.size.w as f64, s.x.size.h as f64));
            d(a).total_cmp(&d(b))
        })
    }

    /// the scale a window at `r` (logical) is drawn at
    pub fn scale_at(&self, r: Rectangle<i32, Logical>) -> f64 {
        self.by_logical(r).map(|s| s.scale).unwrap_or(1.0)
    }
}

impl smithay::xwayland::XMap for XLayout {
    fn to_x(&self, r: Rectangle<i32, Logical>) -> Rectangle<i32, Physical> {
        let Some(s) = self.by_logical(r) else { return Rectangle::new((r.loc.x, r.loc.y).into(), (r.size.w, r.size.h).into()) };
        let x = s.x.loc.x as f64 + (r.loc.x - s.at.loc.x) as f64 * s.scale;
        let y = s.x.loc.y as f64 + (r.loc.y - s.at.loc.y) as f64 * s.scale;
        Rectangle::new((x.round() as i32, y.round() as i32).into(), ((r.size.w as f64 * s.scale).round() as i32, (r.size.h as f64 * s.scale).round() as i32).into())
    }

    fn from_x(&self, r: Rectangle<i32, Physical>) -> Rectangle<i32, Logical> {
        let Some(s) = self.by_x(r) else { return Rectangle::new((r.loc.x, r.loc.y).into(), (r.size.w, r.size.h).into()) };
        let x = s.at.loc.x as f64 + (r.loc.x - s.x.loc.x) as f64 / s.scale;
        let y = s.at.loc.y as f64 + (r.loc.y - s.x.loc.y) as f64 / s.scale;
        Rectangle::new((x.round() as i32, y.round() as i32).into(), ((r.size.w as f64 / s.scale).round() as i32, (r.size.h as f64 / s.scale).round() as i32).into())
    }
}

impl Loom {
    /// X told the screens again (start, hotplug, a screen's mode/scale/place changed). Without
    /// force_zero_scaling X sees the logical layout at scale 1, as before.
    pub fn x_layout_apply(&mut self) {
        self.x.zero_scaling = crate::config::xwayland_zero_scaling();
        let layout = self.x.zero_scaling.then(|| {
            Arc::new(XLayout::new(self.outputs.iter().filter_map(|o| Some((self.space.output_geometry(o)?, o.current_scale().fractional_scale()))).collect()))
        });
        for o in &self.outputs {
            let r = layout.as_ref().and_then(|l| l.screens.iter().find(|s| Some(s.at) == self.space.output_geometry(o))).map(|s| Rectangle::new((s.x.loc.x, s.x.loc.y).into(), (s.x.size.w, s.x.size.h).into()));
            o.set_x_geometry(r);
        }
        if let Some(c) = &self.x.client {
            if let Some(d) = c.get_data::<smithay::xwayland::XWaylandClientData>() {
                d.compositor_state.set_client_scale(1.0);
            }
        }
        if let Some(wm) = &self.xwm {
            wm.set_x_map(layout.clone().map(|l| l as Arc<dyn smithay::xwayland::XMap>));
        }
        self.x.layout = layout;
        self.x_scales();
    }

    /// The scale an X window at `r` (logical) is drawn at: its screen's (1 without force_zero_scaling).
    pub fn x_scale_at(&self, r: Rectangle<i32, Logical>) -> f64 {
        self.x.layout.as_ref().map(|l| l.scale_at(r)).unwrap_or(1.0)
    }

    /// Where an X window moved to `to` goes, keeping its X pixels (as Hyprland): a fixed-size one (a
    /// Wine plugin at 500x700) taken to a screen of another scale was cut to its old logical size.
    pub fn x_moved(&self, x: &smithay::xwayland::X11Surface, to: smithay::utils::Point<i32, Logical>) -> Rectangle<i32, Logical> {
        let old = x.geometry();
        let ratio = self.x_scale_at(old) / self.x_scale_at(Rectangle::new(to, old.size));
        Rectangle::new(to, old.size.to_f64().upscale(ratio).to_i32_round())
    }

    /// The scale of the X window with this surface (1 when it is none).
    pub fn x_surface_scale(&self, s: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> f64 {
        smithay::wayland::compositor::surface_scale(s, 1.0)
    }

    /// Every X window's surface at the scale of the screen X was told it is on (its configured
    /// place: the mapper sized it for that screen).
    pub fn x_scales(&self) {
        if self.x.client.is_none() {
            return;
        }
        for x in self.space.elements().filter_map(|w| w.x11_surface()) {
            let Some(s) = x.wl_surface() else { continue };
            let scale = self.x_scale_at(x.geometry());
            if (smithay::wayland::compositor::surface_scale(&s, 0.0) - scale).abs() > 1e-6 {
                smithay::wayland::compositor::set_surface_scale(&s, Some(scale));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::xwayland::XMap;

    fn two() -> XLayout {
        // the Z13 (2560x1600 at 1.6 = 1600x1000) left, the AOC (1920x1080 at 1) right
        XLayout::new(vec![(Rectangle::new((1600, 0).into(), (1920, 1080).into()), 1.0), (Rectangle::new((0, 0).into(), (1600, 1000).into()), 1.6)])
    }

    #[test]
    fn screens_side_by_side_at_their_pixels() {
        let l = two();
        assert_eq!(l.screens[0].x, Rectangle::new((0, 0).into(), (2560, 1600).into()));
        assert_eq!(l.screens[1].x, Rectangle::new((2560, 0).into(), (1920, 1080).into()));
    }

    #[test]
    fn windows_map_by_their_screen() {
        let l = two();
        // on the laptop: 1.6×
        let r = Rectangle::new((100, 50).into(), (500, 400).into());
        assert_eq!(l.to_x(r), Rectangle::new((160, 80).into(), (800, 640).into()));
        assert_eq!(l.from_x(l.to_x(r)), r);
        // on the AOC: 1:1, shifted
        let r = Rectangle::new((1700, 100).into(), (800, 600).into());
        assert_eq!(l.to_x(r), Rectangle::new((2660, 100).into(), (800, 600).into()));
        assert_eq!(l.from_x(l.to_x(r)), r);
        // a menu X puts at the AOC's pixels lands on the AOC
        let m = l.from_x(Rectangle::new((3000, 500).into(), (200, 300).into()));
        assert_eq!(m, Rectangle::new((2040, 500).into(), (200, 300).into()));
        assert_eq!(l.scale_at(m), 1.0);
    }
}
