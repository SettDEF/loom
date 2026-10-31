//! KDE's effects and loom's windows: events out (shown, closed, maximized, activated), each
//! window's shape in (applied in render.rs), and the last frames closing windows are drawn from.

use serde_json::{json, Value};
use smithay::{
    backend::renderer::{
        element::texture::TextureBuffer,
        gles::{GlesRenderer, GlesTexture},
        Texture,
    },
    desktop::Window,
    utils::{Logical, Rectangle, Transform},
};

use crate::state::Loom;

/// A closed window still drawn (its last frame) while its close animation runs.
pub struct Ghost {
    pub addr: u64,
    pub buffer: TextureBuffer<GlesTexture>,
    /// where its box was (logical)
    pub geo: Rectangle<f64, Logical>,
    /// its picture's size in pixels
    pub px: (i32, i32),
}

impl Loom {
    /// KDE's effects on (`loom.opt.animations = "kwin"`) or off, as the settings say.
    pub fn apply_animations_setting(&mut self) {
        let want = self.look.animations == "kwin";
        if want && self.fx.is_none() {
            self.fx = Some(crate::kwin_fx::Fx::load(None));
        } else if !want && self.fx.is_some() {
            self.fx = None;
            self.ghosts.clear();
            self.fx_last.clear();
        }
    }

    /// What an effect knows of a window (KWin's EffectWindow properties).
    pub fn fx_window(&self, m: &crate::wm::Managed) -> Value {
        let w = &m.window;
        let class = self.class_of(w);
        let loc = self.space.element_location(w).unwrap_or_default();
        let size = w.geometry().size;
        let dialog = m.floating && w.x11_surface().is_some_and(|x| x.is_transient_for().is_some());
        let geo = json!({ "x": loc.x, "y": loc.y, "width": size.w, "height": size.h });
        json!({
            "addr": m.addr,
            "windowClass": format!("{} {}", class.to_lowercase(), class),
            "caption": self.title_of(w),
            "normalWindow": !dialog, "dialog": dialog, "popupWindow": false, "managed": true,
            "visible": self.space.elements().any(|e| e == w), "minimized": crate::wm::is_special(m.ws),
            "hasDecoration": !matches!(self.decor_kind(w), crate::decor::Kind::None),
            "geometry": geo, "frameGeometry": geo, "expandedGeometry": geo,
            "fullScreen": self.is_full(w), "maximized": self.maximized.as_ref() == Some(w),
            "modal": false, "desktopWindow": false, "dock": false, "splash": false, "toolbar": false, "menu": false,
            "dropdownMenu": false, "popupMenu": false, "tooltip": false, "notification": false, "onScreenDisplay": false,
            "lockScreen": false, "outline": false, "skipsCloseAnimation": false, "deleted": false, "screen": 0,
            "x11Client": w.x11_surface().is_some(), "waylandClient": w.x11_surface().is_none(),
            "iconGeometry": Value::Null, "tag": "", "pid": m.pid, "unresponsive": false,
        })
    }

    /// An event for the effects about window `w` (none when they are off).
    pub fn fx_event(&self, name: &str, w: &Window, extra: &[Value]) {
        let Some(fx) = &self.fx else { return };
        let Some(m) = self.managed_of(w) else { return };
        let me = self.fx_window(m);
        let stacking: Vec<Value> = self.managed.iter().map(|m| self.fx_window(m)).collect();
        let active = self.focused.as_ref().and_then(|f| self.managed_of(f)).map(|m| m.addr);
        if let Some(o) = self.output.as_ref().and_then(|o| self.space.output_geometry(o)) {
            fx.set_display(o.size.w as f64, o.size.h as f64);
        }
        if let Some(g) = me["geometry"].as_object() {
            let n = |k: &str| g.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            fx.set_geometry(m.addr, (n("x"), n("y"), n("width"), n("height")));
        }
        fx.event(name, Some(&me), &stacking, active, extra);
    }

    /// A window committed a frame: its texture kept (a reference, nothing copied), since by the
    /// time it closes its surfaces are gone and a close animation draws from this.
    pub fn fx_keep(&mut self, w: &Window, root: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {
        let Some(addr) = self.managed_of(w).map(|m| m.addr) else { return };
        let geo_loc = w.geometry().loc;
        let kept = self.with_gles(|r| {
            smithay::backend::renderer::utils::import_surface_tree(r, root).ok()?;
            let id = smithay::backend::renderer::Renderer::context_id(r);
            smithay::backend::renderer::utils::with_renderer_surface_state(root, |s| Some((s.texture(id)?.clone(), s.surface_size()?))).flatten()
        });
        if let Some(Some((tex, size))) = kept {
            self.fx_last.insert(addr, (tex, size, geo_loc));
        }
    }

    /// A window closing: told to the effects; if one animates it, its last frame stays drawn.
    pub fn fx_closing(&mut self, w: &Window) {
        if self.fx.is_none() {
            return;
        }
        let Some(addr) = self.managed_of(w).map(|m| m.addr) else { return };
        self.fx_event("windowClosed", w, &[]);
        let last = self.fx_last.remove(&addr);
        if !self.fx.as_ref().is_some_and(|f| f.animates(addr)) {
            return;
        }
        let (Some((tex, size, geo_loc)), Some(loc)) = (last, self.space.element_location(w)) else { return };
        let px = (tex.width() as i32, tex.height() as i32);
        let Some(buffer) = self.with_gles(|r| TextureBuffer::from_texture(r, tex, 1, Transform::Normal, None)) else { return };
        // the whole surface (its shadow too), where it was
        let at = ((loc.x - geo_loc.x) as f64, (loc.y - geo_loc.y) as f64);
        self.ghosts.push(Ghost { addr, buffer, geo: Rectangle::new(at.into(), (size.w as f64, size.h as f64).into()), px });
        self.dirty = true;
    }

    /// Ghosts whose animation is over: gone (and the effects forget their window).
    pub fn fx_tick(&mut self) {
        let Some(fx) = &self.fx else { return };
        let now = std::time::Instant::now();
        fx.tick(now);
        let gone: Vec<u64> = self.ghosts.iter().filter(|g| !fx.animates(g.addr)).map(|g| g.addr).collect();
        if !gone.is_empty() {
            self.ghosts.retain(|g| !gone.contains(&g.addr));
            for a in gone {
                fx.forget(a);
            }
            self.dirty = true;
        }
    }

    /// `f` with the GL renderer, whichever backend has it (none: pixman headless).
    pub fn with_gles<T>(&mut self, f: impl FnOnce(&mut GlesRenderer) -> T) -> Option<T> {
        if let Some(g) = self.gpu.clone() {
            return Some(f(&mut g.borrow_mut()));
        }
        if let Some(mut n) = self.nested.take() {
            let r = f(n.backend.renderer());
            self.nested = Some(n);
            return Some(r);
        }
        None
    }
}
