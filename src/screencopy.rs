//! wlr-screencopy: the screen (or a region of it) copied into a client's buffer, for grim, the
//! shell's region selector and screenshot tools, portals and recorders. Drawn offscreen on the
//! next turn of the loop, only when asked, with the renderer the backend has.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::utils::{Relocate, RelocateRenderElement},
            Bind, ExportMem, Offscreen, Texture,
        },
    },
    reexports::{
        wayland_protocols_wlr::screencopy::v1::server::{
            zwlr_screencopy_frame_v1::{self as frame, Flags, ZwlrScreencopyFrameV1},
            zwlr_screencopy_manager_v1::{self as manager, ZwlrScreencopyManagerV1},
        },
        wayland_server::{backend::ClientId, protocol::wl_buffer::WlBuffer, protocol::wl_shm, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource},
    },
    utils::{Physical, Rectangle, Transform},
};

use crate::state::Loom;

/// What a frame copies: a part of the output, in pixels.
#[derive(Clone, Copy)]
pub struct Region(pub Rectangle<i32, Physical>);

pub struct ScreenCapture {
    pub frame: ZwlrScreencopyFrameV1,
    pub rect: Rectangle<i32, Physical>,
    pub buffer: WlBuffer,
}

pub fn init(dh: &DisplayHandle) {
    dh.create_global::<Loom, ZwlrScreencopyManagerV1, ()>(3, ());
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for Loom {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<ZwlrScreencopyManagerV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, _: &ZwlrScreencopyManagerV1, request: manager::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        // the whole output in pixels; a region is given in logical units and scaled
        let full = state.output.as_ref().and_then(|o| o.current_mode()).map(|m| Rectangle::from_size(m.size)).unwrap_or_default();
        let scale = state.output.as_ref().map(|o| o.current_scale().fractional_scale()).unwrap_or(1.0);
        let (new, rect) = match request {
            manager::Request::CaptureOutput { frame, .. } => (frame, full),
            manager::Request::CaptureOutputRegion { frame, x, y, width, height, .. } => {
                let r = Rectangle::<i32, smithay::utils::Logical>::new((x, y).into(), (width, height).into()).to_f64().to_physical(scale).to_i32_round();
                (frame, r.intersection(full).unwrap_or_default())
            }
            _ => return,
        };
        let f = data_init.init(new, Region(rect));
        if rect.size.w <= 0 || rect.size.h <= 0 {
            f.failed();
            return;
        }
        f.buffer(wl_shm::Format::Argb8888, rect.size.w as u32, rect.size.h as u32, rect.size.w as u32 * 4);
        if f.version() >= 3 {
            f.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, Region> for Loom {
    fn request(state: &mut Self, _: &Client, resource: &ZwlrScreencopyFrameV1, request: frame::Request, data: &Region, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let buffer = match request {
            frame::Request::Copy { buffer } | frame::Request::CopyWithDamage { buffer } => buffer,
            _ => return,
        };
        state.screen_captures.push(ScreenCapture { frame: resource.clone(), rect: data.0, buffer });
        // the loop must turn once more to draw it
        state.wake_in(std::time::Duration::ZERO);
    }
    fn destroyed(state: &mut Self, _: ClientId, resource: &ZwlrScreencopyFrameV1, _: &Region) {
        state.screen_captures.retain(|c| &c.frame != resource);
    }
}

/// The screen copies asked for, drawn now (the output's elements moved so the region is at 0,0).
pub fn run<R, T>(state: &mut Loom, renderer: &mut R)
where
    R: crate::render::LoomRenderer + Offscreen<T> + Bind<T> + ExportMem,
    R::TextureId: Texture + Clone + 'static,
    T: 'static,
{
    if state.screen_captures.is_empty() {
        return;
    }
    let Some(output) = state.output.clone() else { return };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    for c in std::mem::take(&mut state.screen_captures) {
        let ok = (|| {
            let size = (c.rect.size.w, c.rect.size.h);
            let elements: Vec<_> = crate::render::elements(state, renderer, &output, std::time::Instant::now())
                .into_iter()
                .map(|e| RelocateRenderElement::from_element(e, (-c.rect.loc.x, -c.rect.loc.y), Relocate::Relative))
                .collect();
            let mut target = renderer.create_buffer(Fourcc::Argb8888, size.into()).ok()?;
            let mut fb = renderer.bind(&mut target).ok()?;
            // at the output's scale: the elements size themselves by it
            let os = output.current_scale().fractional_scale();
            let mut damage = OutputDamageTracker::new(size, os, Transform::Normal);
            damage.render_output(renderer, &mut fb, 0, &elements, [0.08, 0.08, 0.1, 1.0]).ok()?;
            let mapping = renderer.copy_framebuffer(&fb, Rectangle::from_size(size.into()), Fourcc::Argb8888).ok()?;
            drop(fb);
            let pixels = renderer.map_texture(&mapping).ok()?.to_vec();
            smithay::wayland::shm::with_buffer_contents_mut(&c.buffer, |ptr, len, data| {
                let row = (size.0 as usize * 4).min(data.stride as usize);
                for y in 0..(data.height.min(size.1) as usize) {
                    let (dst, src) = (data.offset as usize + y * data.stride as usize, y * size.0 as usize * 4);
                    if dst + row <= len && src + row <= pixels.len() {
                        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr().add(src), ptr.add(dst), row) };
                    }
                }
            })
            .ok()
        })();
        if ok.is_some() {
            c.frame.flags(Flags::empty());
            c.frame.ready((now.as_secs() >> 32) as u32, now.as_secs() as u32, now.subsec_nanos());
        } else {
            c.frame.failed();
        }
    }
}
