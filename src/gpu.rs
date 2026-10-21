//! The GPU: buffers programs draw on the GPU (linux-dmabuf) taken by whichever renderer the
//! backend has (nested: its window's; headless --gpu: a surfaceless one on the render node).

use std::{cell::RefCell, rc::Rc};

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{gles::GlesRenderer, ImportDma},
    },
    delegate_dmabuf,
    wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::Loom;

/// A GL renderer with no screen, on the first GPU that has a render node.
pub fn headless_renderer() -> Result<GlesRenderer, Box<dyn std::error::Error>> {
    let device = EGLDevice::enumerate()?.find(|d| !d.is_software() && d.render_device_path().is_ok()).ok_or("no GPU with a render node")?;
    // Safety: the display and context live as long as the renderer that owns them
    let display = unsafe { EGLDisplay::new(device)? };
    let context = EGLContext::new(&display)?;
    Ok(unsafe { GlesRenderer::new(context)? })
}

/// Offers linux-dmabuf with the formats `renderer` can take.
/// Offers linux-dmabuf with the formats `renderer` can take, with default feedback naming its
/// GPU (version 4+: Quickshell asks for the feedback and dies on a v3 global).
pub fn offer(state: &mut Loom, renderer: &GlesRenderer) {
    let formats: Vec<_> = renderer.dmabuf_formats().into_iter().collect();
    let mut dmabuf_state = DmabufState::new();
    let device = EGLDevice::device_for_display(renderer.egl_context().display()).ok().and_then(|d| d.render_device_path().ok());
    let dev = device.and_then(|p| std::fs::metadata(p).ok()).map(|m| std::os::unix::fs::MetadataExt::rdev(&m));
    let feedback = dev.and_then(|d| smithay::wayland::dmabuf::DmabufFeedbackBuilder::new(d as libc::dev_t, formats.iter().copied()).build().ok());
    let global = match feedback {
        Some(f) => dmabuf_state.create_global_with_default_feedback::<Loom>(&state.dh, &f),
        None => dmabuf_state.create_global::<Loom>(&state.dh, formats),
    };
    state.dmabuf = Some((dmabuf_state, global));
}

impl DmabufHandler for Loom {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf.as_mut().expect("dmabuf offered").0
    }
    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        // a GPU buffer the client made: the renderer takes it, or the client hears it failed
        let ok = match (&mut self.nested, &self.gpu) {
            (Some(n), _) => n.backend.renderer().import_dmabuf(&dmabuf, None).is_ok(),
            (None, Some(r)) => r.borrow_mut().import_dmabuf(&dmabuf, None).is_ok(),
            _ => false,
        };
        if ok {
            let _ = notifier.successful::<Loom>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(Loom);

pub type Shared = Rc<RefCell<GlesRenderer>>;
