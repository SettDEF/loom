//! hyprland-toplevel-export-v1: one window drawn into a buffer the client gives (the shell's
//! window previews: overview, alt-tab). Drawn only when asked, only that window, on the next
//! frame, by whichever renderer the backend has (pixman headless, GL nested).

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{surface::WaylandSurfaceRenderElement, AsRenderElements},
            Bind, ExportMem, Offscreen, Texture,
        },
    },
    reexports::wayland_server::{backend::ClientId, protocol::wl_buffer::WlBuffer, Client, DataInit, Dispatch, DisplayHandle, Resource},
    utils::{Scale, Transform},
};

use crate::state::Loom;

pub mod export {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables, non_upper_case_globals, non_snake_case, unused_imports, missing_docs, clippy::all)]
    use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::*;
    use wayland_server;
    use wayland_server::protocol::*;
    pub mod __interfaces {
        use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::__interfaces::*;
        use wayland_server::backend as wayland_backend;
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-export-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/hyprland-toplevel-export-v1.xml");
}

use export::{
    hyprland_toplevel_export_frame_v1::{self as frame, HyprlandToplevelExportFrameV1},
    hyprland_toplevel_export_manager_v1::{self as manager, HyprlandToplevelExportManagerV1},
};

/// A copy asked for: drawn on the next frame.
pub struct Capture {
    pub frame: HyprlandToplevelExportFrameV1,
    pub addr: u64,
    pub buffer: WlBuffer,
    /// copy now, changed or not (a first copy); otherwise only once the window drew again
    pub ignore_damage: bool,
}

pub fn init(dh: &DisplayHandle) {
    dh.create_global::<Loom, HyprlandToplevelExportManagerV1, ()>(2, ());
}

impl smithay::reexports::wayland_server::GlobalDispatch<HyprlandToplevelExportManagerV1, ()> for Loom {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: smithay::reexports::wayland_server::New<HyprlandToplevelExportManagerV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        data_init.init(resource, ());
    }
}

impl Dispatch<HyprlandToplevelExportManagerV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, _: &HyprlandToplevelExportManagerV1, request: manager::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        let (new, addr) = match request {
            // the Hyprland address (low 32 bits, as Hyprland takes it)
            manager::Request::CaptureToplevel { frame, handle, .. } => (frame, state.managed.iter().find(|m| m.addr as u32 == handle).map(|m| m.addr)),
            manager::Request::CaptureToplevelWithWlrToplevelHandle { frame, handle, .. } => (frame, handle.data::<u64>().copied()),
            _ => return,
        };
        let f = data_init.init(new, addr.unwrap_or(0));
        let Some(size) = addr.and_then(|a| state.managed.iter().find(|m| m.addr == a)).map(|m| m.window.geometry().size).filter(|s| s.w > 0 && s.h > 0) else {
            f.failed();
            return;
        };
        // shm ARGB8888 (0), packed rows
        f.buffer(smithay::reexports::wayland_server::protocol::wl_shm::Format::Argb8888, size.w as u32, size.h as u32, size.w as u32 * 4);
        // a GPU buffer too, when the renderer can draw into one: the copy stays on the GPU (no
        // read back and copy per frame while the overview shows it); the shell found none before
        if state.copy_dmabuf.is_some() {
            f.linux_dmabuf(Fourcc::Argb8888 as u32, size.w as u32, size.h as u32);
        }
        if f.version() >= 2 {
            f.buffer_done();
        }
    }
}

impl Dispatch<HyprlandToplevelExportFrameV1, u64> for Loom {
    fn request(state: &mut Self, _: &Client, resource: &HyprlandToplevelExportFrameV1, request: frame::Request, addr: &u64, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let frame::Request::Copy { buffer, ignore_damage } = request {
            state.captures.push(Capture { frame: resource.clone(), addr: *addr, buffer, ignore_damage: ignore_damage != 0 });
        }
    }
    fn destroyed(state: &mut Self, _: ClientId, resource: &HyprlandToplevelExportFrameV1, _: &u64) {
        state.captures.retain(|c| &c.frame != resource);
    }
}

/// The copies asked for, drawn now with `renderer`: straight into a client's GPU buffer, or
/// offscreen and read back into its shm one.
pub fn run<R, T>(state: &mut Loom, renderer: &mut R)
where
    R: crate::render::LoomRenderer + Offscreen<T> + Bind<T> + Bind<smithay::backend::allocator::dmabuf::Dmabuf> + ExportMem,
    R::TextureId: Texture + Clone + 'static,
    T: 'static,
{
    crate::screencopy::run::<R, T>(state, renderer);
    if !state.captures_ready() {
        return;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let (ready, waiting): (Vec<Capture>, Vec<Capture>) = std::mem::take(&mut state.captures).into_iter().partition(|c| state.capture_ready(c));
    state.captures = waiting;
    for c in ready {
        let key = (c.frame.client().map(|cl| cl.id()), c.addr);
        let seen = state.commit_count.get(&c.addr).copied().unwrap_or(0);
        state.captured_at.insert(key, seen);
        let ok = (|| {
            let w = state.managed.iter().find(|m| m.addr == c.addr)?.window.clone();
            let geo = w.geometry();
            let size = (geo.size.w, geo.size.h);
            let elements: Vec<WaylandSurfaceRenderElement<R>> = AsRenderElements::<R>::render_elements(&w, renderer, (-geo.loc.x, -geo.loc.y).into(), Scale::from(1.0), 1.0);
            // the client's GPU buffer: drawn straight into it
            if let Ok(d) = smithay::wayland::dmabuf::get_dmabuf(&c.buffer) {
                if smithay::backend::allocator::Buffer::size(d) != smithay::utils::Size::from(size) {
                    return None;
                }
                let mut d = d.clone();
                let mut fb = renderer.bind(&mut d).ok()?;
                let mut damage = OutputDamageTracker::new(size, 1.0, Transform::Normal);
                let r = damage.render_output(renderer, &mut fb, 0, &elements, [0.0, 0.0, 0.0, 0.0]).ok()?;
                let _ = r.sync.wait();
                return Some(());
            }
            let mut target = renderer.create_buffer(Fourcc::Argb8888, size.into()).ok()?;
            let mut fb = renderer.bind(&mut target).ok()?;
            let mut damage = OutputDamageTracker::new(size, 1.0, Transform::Normal);
            damage.render_output(renderer, &mut fb, 0, &elements, [0.0, 0.0, 0.0, 0.0]).ok()?;
            let mapping = renderer.copy_framebuffer(&fb, smithay::utils::Rectangle::from_size(size.into()), Fourcc::Argb8888).ok()?;
            drop(fb);
            let pixels = renderer.map_texture(&mapping).ok()?.to_vec();
            // into the client's buffer, row by row (its stride may differ)
            smithay::wayland::shm::with_buffer_contents_mut(&c.buffer, |ptr, len, data| {
                let rows = data.height.min(size.1) as usize;
                let row = (size.0 as usize * 4).min(data.stride as usize);
                for y in 0..rows {
                    let dst = data.offset as usize + y * data.stride as usize;
                    let src = y * size.0 as usize * 4;
                    if dst + row <= len && src + row <= pixels.len() {
                        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr().add(src), ptr.add(dst), row) };
                    }
                }
            })
            .ok()
        })();
        if ok.is_some() {
            c.frame.flags(export::hyprland_toplevel_export_frame_v1::Flags::empty());
            c.frame.ready((now.as_secs() >> 32) as u32, now.as_secs() as u32, now.subsec_nanos());
        } else {
            c.frame.failed();
        }
    }
}

impl Loom {
    /// A copy is due: asked to ignore damage, or its window drew since this client's last copy.
    pub fn capture_ready(&self, c: &Capture) -> bool {
        if c.ignore_damage {
            return true;
        }
        let key = (c.frame.client().map(|cl| cl.id()), c.addr);
        self.commit_count.get(&c.addr).copied().unwrap_or(0) != self.captured_at.get(&key).copied().unwrap_or(u64::MAX)
    }
    pub fn captures_ready(&self) -> bool {
        self.screen_captures.iter().any(|c| c.ready(self.shown)) || self.captures.iter().any(|c| self.capture_ready(c))
    }
}
