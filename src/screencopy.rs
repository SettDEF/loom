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

use smithay::output::Output;

use crate::state::Loom;

/// What a frame copies: a part of the output, in pixels.
#[derive(Clone)]
pub struct Region(pub Rectangle<i32, Physical>, pub String);

/// Who a copy answers: a wlr-screencopy frame, or an ext-image-copy-capture one (with its session's
/// last shown frame, so its next copy waits for a change).
pub enum Answer {
    Wlr(ZwlrScreencopyFrameV1),
    Ext(smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1, std::sync::Arc<std::sync::Mutex<Option<u64>>>),
}

impl Answer {
    fn failed(&self) {
        match self {
            Answer::Wlr(f) => f.failed(),
            Answer::Ext(f, _) => f.failed(smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::FailureReason::Unknown),
        }
    }

    /// Copied: what changed (the whole region; loom keeps no finer damage for copies), and when.
    fn ready(&self, waited: bool, size: (i32, i32), shown: u64) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let (hi, lo, ns) = ((now.as_secs() >> 32) as u32, now.as_secs() as u32, now.subsec_nanos());
        match self {
            Answer::Wlr(f) => {
                if waited && f.version() >= 2 {
                    f.damage(0, 0, size.0 as u32, size.1 as u32);
                }
                f.flags(Flags::empty());
                f.ready(hi, lo, ns);
            }
            Answer::Ext(f, last) => {
                *last.lock().unwrap() = Some(shown);
                f.transform(smithay::reexports::wayland_server::protocol::wl_output::Transform::Normal);
                f.damage(0, 0, size.0, size.1);
                f.presentation_time(hi, lo, ns);
                f.ready();
            }
        }
    }
}

pub struct ScreenCapture {
    pub frame: Answer,
    pub rect: Rectangle<i32, Physical>,
    /// the screen it copies (by name)
    pub output: String,
    pub buffer: WlBuffer,
    /// copy_with_damage: answered once a frame after this one has changed the screen
    pub after: Option<u64>,
    pub client: Option<ClientId>,
}

impl ScreenCapture {
    pub fn ready(&self, shown: u64) -> bool {
        self.after.is_none_or(|a| shown > a)
    }
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
        // the screen asked for; the whole of it in pixels; a region is given in logical units and scaled
        let wanted = match &request {
            manager::Request::CaptureOutput { output, .. } | manager::Request::CaptureOutputRegion { output, .. } => Output::from_resource(output),
            _ => None,
        };
        let Some(o) = wanted.or_else(|| state.output.clone()) else { return };
        let full = o.current_mode().map(|m| Rectangle::from_size(m.size)).unwrap_or_default();
        let scale = o.current_scale().fractional_scale();
        let (new, rect) = match request {
            manager::Request::CaptureOutput { frame, .. } => (frame, full),
            manager::Request::CaptureOutputRegion { frame, x, y, width, height, .. } => {
                let r = Rectangle::<i32, smithay::utils::Logical>::new((x, y).into(), (width, height).into()).to_f64().to_physical(scale).to_i32_round();
                (frame, r.intersection(full).unwrap_or_default())
            }
            _ => return,
        };
        let f = data_init.init(new, Region(rect, o.name()));
        if rect.size.w <= 0 || rect.size.h <= 0 {
            f.failed();
            return;
        }
        f.buffer(wl_shm::Format::Argb8888, rect.size.w as u32, rect.size.h as u32, rect.size.w as u32 * 4);
        if f.version() >= 3 {
            // on the GPU a GPU buffer may come instead, drawn straight into (portals, OBS). Opt-in for now
            // (LOOM_SCREENCOPY_DMABUF=1): wf-recorder with VA-API stalls after ~17 frames on it (2026-10-22)
            if (state.gpu.is_some() || state.nested.is_some()) && std::env::var("LOOM_SCREENCOPY_DMABUF").is_ok_and(|v| v == "1") {
                f.linux_dmabuf(smithay::backend::allocator::Fourcc::Xrgb8888 as u32, rect.size.w as u32, rect.size.h as u32);
            }
            f.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, Region> for Loom {
    fn request(state: &mut Self, client: &Client, resource: &ZwlrScreencopyFrameV1, request: frame::Request, data: &Region, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let (buffer, after) = match request {
            frame::Request::Copy { buffer } => (buffer, None),
            // its first copy at once, then only when the screen changed since its last one
            frame::Request::CopyWithDamage { buffer } => (buffer, state.copy_seen.get(&client.id()).copied()),
            _ => return,
        };
        state.screen_captures.push(ScreenCapture { frame: Answer::Wlr(resource.clone()), rect: data.0, output: data.1.clone(), buffer, after, client: Some(client.id()) });
        // the loop must turn once more to draw it
        state.wake_in(std::time::Duration::ZERO);
    }
    fn destroyed(state: &mut Self, _: ClientId, resource: &ZwlrScreencopyFrameV1, _: &Region) {
        // a gone client's mark goes with it
        state.copy_seen.retain(|id, _| state.screen_captures.iter().any(|c| c.client.as_ref() == Some(id)) || state.dh.backend_handle().get_client_credentials(id.clone()).is_ok());
        state.screen_captures.retain(|c| !matches!(&c.frame, Answer::Wlr(f) if f == resource));
    }
}

/// The screen copies asked for, drawn now (the output's elements moved so the region is at 0,0).
pub fn run<R, T>(state: &mut Loom, renderer: &mut R)
where
    R: crate::render::LoomRenderer + Offscreen<T> + Bind<T> + Bind<smithay::backend::allocator::dmabuf::Dmabuf> + ExportMem,
    R::TextureId: Texture + Clone + 'static,
    T: 'static,
{
    if state.screen_captures.is_empty() {
        return;
    }
    let (now_ready, later): (Vec<_>, Vec<_>) = std::mem::take(&mut state.screen_captures).into_iter().partition(|c| c.ready(state.shown));
    state.screen_captures = later;
    for c in now_ready {
        let Some(output) = state.output_named(&c.output) else {
            c.frame.failed();
            continue;
        };
        let ok = (|| {
            let size = (c.rect.size.w, c.rect.size.h);
            let elements: Vec<_> = crate::render::elements(state, renderer, &output, std::time::Instant::now())
                .into_iter()
                .map(|e| RelocateRenderElement::from_element(e, (-c.rect.loc.x, -c.rect.loc.y), Relocate::Relative))
                .collect();
            let os = output.current_scale().fractional_scale();
            // a GPU buffer (portals, OBS): drawn straight into it, nothing read back
            if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(&c.buffer) {
                let mut dmabuf = dmabuf.clone();
                let mut fb = renderer.bind(&mut dmabuf).ok()?;
                let mut damage = OutputDamageTracker::new(size, os, Transform::Normal);
                let r = damage.render_output(renderer, &mut fb, 0, &elements, [0.08, 0.08, 0.1, 1.0]).ok()?;
                let _ = r.sync.wait();
                return Some(());
            }
            let mut target = renderer.create_buffer(Fourcc::Argb8888, size.into()).ok()?;
            let mut fb = renderer.bind(&mut target).ok()?;
            // at the output's scale: the elements size themselves by it
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
            if let Some(id) = &c.client {
                state.copy_seen.insert(id.clone(), state.shown);
            }
            c.frame.ready(c.after.is_some(), (c.rect.size.w, c.rect.size.h), state.shown);
        } else {
            c.frame.failed();
        }
    }
}
