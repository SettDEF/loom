//! ext-image-copy-capture-v1 (+ ext-image-capture-source): the standard way to copy a screen, which
//! grim and Quickshell try first. Screens only (loom has no ext-foreign-toplevel-list yet). The
//! copy itself is screencopy.rs's: drawn offscreen when asked, into the client's shm or GPU buffer.

use std::sync::{Arc, Mutex};

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols::ext::{
            image_capture_source::v1::server::{
                ext_image_capture_source_v1::{self as source, ExtImageCaptureSourceV1},
                ext_output_image_capture_source_manager_v1::{self as out_m, ExtOutputImageCaptureSourceManagerV1},
            },
            image_copy_capture::v1::server::{
                ext_image_copy_capture_cursor_session_v1::{self as cursor, ExtImageCopyCaptureCursorSessionV1},
                ext_image_copy_capture_frame_v1::{self as frame, ExtImageCopyCaptureFrameV1, FailureReason},
                ext_image_copy_capture_manager_v1::{self as manager, ExtImageCopyCaptureManagerV1},
                ext_image_copy_capture_session_v1::{self as session, ExtImageCopyCaptureSessionV1},
            },
        },
        wayland_server::{protocol::wl_buffer::WlBuffer, protocol::wl_shm, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource},
    },
    utils::Rectangle,
};

use crate::{
    screencopy::{Answer, ScreenCapture},
    state::Loom,
};

/// A source: the screen it is (by name); none when it was gone.
pub struct Source(Option<String>);

/// A session: its screen, and the frame it showed last (a copy waits for the screen to change).
pub struct Session {
    output: Option<String>,
    pub last: Arc<Mutex<Option<u64>>>,
}

/// A frame: the buffer given, and whether it was captured.
#[derive(Default)]
pub struct Frame {
    buffer: Mutex<Option<WlBuffer>>,
    captured: Mutex<bool>,
}

pub fn init(dh: &DisplayHandle) {
    dh.create_global::<Loom, ExtImageCopyCaptureManagerV1, ()>(1, ());
    dh.create_global::<Loom, ExtOutputImageCaptureSourceManagerV1, ()>(1, ());
}

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ()> for Loom {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, r: New<ExtImageCopyCaptureManagerV1>, _: &(), d: &mut DataInit<'_, Self>) {
        d.init(r, ());
    }
}

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Loom {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, r: New<ExtOutputImageCaptureSourceManagerV1>, _: &(), d: &mut DataInit<'_, Self>) {
        d.init(r, ());
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &ExtOutputImageCaptureSourceManagerV1, request: out_m::Request, _: &(), _: &DisplayHandle, d: &mut DataInit<'_, Self>) {
        if let out_m::Request::CreateSource { source, output } = request {
            d.init(source, Source(Output::from_resource(&output).map(|o| o.name())));
        }
    }
}

impl Dispatch<ExtImageCaptureSourceV1, Source> for Loom {
    fn request(_: &mut Self, _: &Client, _: &ExtImageCaptureSourceV1, _: source::Request, _: &Source, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

/// A new session's constraints: the screen's size in pixels, shm, and GPU buffers where there is a GPU;
/// stopped at once without a screen.
fn start(state: &Loom, s: &ExtImageCopyCaptureSessionV1, output: Option<&str>) {
    let size = output.and_then(|n| state.output_named(n)).and_then(|o| o.current_mode()).map(|m| m.size);
    let Some(size) = size else {
        s.stopped();
        return;
    };
    s.buffer_size(size.w as u32, size.h as u32);
    s.shm_format(wl_shm::Format::Argb8888);
    s.shm_format(wl_shm::Format::Xrgb8888);
    if let Some((dev, formats)) = &state.copy_dmabuf {
        s.dmabuf_device(dev.to_ne_bytes().to_vec());
        for (f, mods) in formats.iter().filter(|(_, m)| !m.is_empty()) {
            s.dmabuf_format(*f, mods.iter().flat_map(|m| m.to_ne_bytes()).collect());
        }
    }
    s.done();
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, _: &ExtImageCopyCaptureManagerV1, request: manager::Request, _: &(), _: &DisplayHandle, d: &mut DataInit<'_, Self>) {
        match request {
            manager::Request::CreateSession { session, source, .. } => {
                let output = source.data::<Source>().and_then(|s| s.0.clone());
                let s = d.init(session, Session { output: output.clone(), last: Arc::default() });
                start(state, &s, output.as_deref());
            }
            // the cursor on its own is not offered: its session stops at once
            manager::Request::CreatePointerCursorSession { session, .. } => drop(d.init(session, ())),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &ExtImageCopyCaptureCursorSessionV1, request: cursor::Request, _: &(), _: &DisplayHandle, d: &mut DataInit<'_, Self>) {
        if let cursor::Request::GetCaptureSession { session } = request {
            d.init(session, Session { output: None, last: Arc::default() }).stopped();
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, Session> for Loom {
    fn request(_: &mut Self, _: &Client, s: &ExtImageCopyCaptureSessionV1, request: session::Request, _: &Session, _: &DisplayHandle, d: &mut DataInit<'_, Self>) {
        if let session::Request::CreateFrame { frame } = request {
            d.init(frame, (s.clone(), Frame::default()));
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, (ExtImageCopyCaptureSessionV1, Frame)> for Loom {
    fn request(state: &mut Self, client: &Client, f: &ExtImageCopyCaptureFrameV1, request: frame::Request, data: &(ExtImageCopyCaptureSessionV1, Frame), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let (s, fr) = data;
        match request {
            frame::Request::AttachBuffer { buffer } => *fr.buffer.lock().unwrap() = Some(buffer),
            frame::Request::Capture => {
                if std::mem::replace(&mut *fr.captured.lock().unwrap(), true) {
                    f.post_error(frame::Error::AlreadyCaptured, "this frame was captured already");
                    return;
                }
                let Some(buffer) = fr.buffer.lock().unwrap().clone() else {
                    f.post_error(frame::Error::NoBuffer, "capture without a buffer");
                    return;
                };
                let Some(sess) = s.data::<Session>() else { return };
                let Some(o) = sess.output.as_deref().and_then(|n| state.output_named(n)) else {
                    f.failed(FailureReason::Stopped);
                    return;
                };
                let Some(size) = o.current_mode().map(|m| m.size) else { return };
                // a buffer of another size or kind than the session said
                let fits = match smithay::wayland::dmabuf::get_dmabuf(&buffer) {
                    Ok(d) => state.copy_dmabuf.is_some() && smithay::backend::allocator::Buffer::size(d) == size.to_logical(1).to_buffer(1, smithay::utils::Transform::Normal),
                    Err(_) => smithay::wayland::shm::with_buffer_contents(&buffer, |_, _, b| b.width == size.w && b.height == size.h && matches!(b.format, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)).unwrap_or(false),
                };
                if !fits {
                    f.failed(FailureReason::BufferConstraints);
                    return;
                }
                let after = *sess.last.lock().unwrap();
                state.screen_captures.push(ScreenCapture { frame: Answer::Ext(f.clone(), sess.last.clone()), rect: Rectangle::from_size(size), output: o.name(), buffer, after, client: Some(client.id()) });
                state.wake_in(std::time::Duration::ZERO);
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _: smithay::reexports::wayland_server::backend::ClientId, f: &ExtImageCopyCaptureFrameV1, _: &(ExtImageCopyCaptureSessionV1, Frame)) {
        state.screen_captures.retain(|c| !matches!(&c.frame, Answer::Ext(x, _) if x == f));
    }
}
