//! The real screen: loom as the desktop on a TTY. libseat for the session (the GPU and input from
//! logind; Ctrl+Alt+F-keys switch away and back), DRM/KMS through Smithay's DrmCompositor (frames on
//! the panel's vblank, only when something changed; fullscreen windows and the cursor straight to
//! planes when they can be), libinput for keyboard, touchpad and mouse. Mode, refresh and scale from
//! the user's Hyprland `monitor=` lines, matched by name or by the panel's EDID name (`desc:`).
//! Always: Ctrl+Alt+Backspace quits (no bind can take it); `--for MINUTES` quits by itself.

use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};

use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
        },
        egl::{EGLContext, EGLDisplay},
        input::{AbsolutePositionEvent, Axis, ButtonState, InputEvent, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind,
            },
            gles::GlesRenderer,
            ImportDma,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu},
    },
    input::pointer::CursorImageStatus,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{generic::Generic, timer::{TimeoutAction, Timer}, EventLoop, Interest, PostAction},
        drm::control::{connector, crtc, Device as ControlDevice, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Point, Transform},
    wayland::socket::ListeningSocketSource,
};

use crate::state::{client_data, Loom};

smithay::backend::renderer::element::render_elements! {
    pub TtyElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Cursor=MemoryRenderBufferRenderElement<GlesRenderer>,
}

type Compositor = DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

pub struct Tty {
    pub session: LibSeatSession,
    pub drm: DrmDevice,
    pub compositor: Compositor,
    pub libinput: Libinput,
    /// a frame is on its way to the screen (the next waits for its vblank)
    pub pending: bool,
    /// the session is ours (not switched away)
    pub active: bool,
    /// the theme's arrow, and its hotspot (pixels)
    pub cursor: Option<(MemoryRenderBuffer, (i32, i32))>,
    /// the refresh period, for frame callbacks when nothing reached the screen
    pub period: Duration,
    /// Ctrl+Alt+F<n> asked for: switched to after this input
    pub vt_switch: Option<i32>,
    /// when a frame showed nothing new: the programs' frame callbacks go out a refresh later
    pub owed_at: std::time::Instant,
}

pub struct Options {
    pub socket: Option<String>,
    pub run: Vec<String>,
    /// quit by itself after this long (first tries: a dead man's switch)
    pub for_minutes: Option<u64>,
}

/// The Hyprland `monitor=` line for this screen: (mode width, height, refresh Hz, scale).
fn monitor_rule(name: &str, edid_name: Option<&str>) -> Option<(u16, u16, f64, f64)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = PathBuf::from(&home).join(".config/hypr");
    let mut text = String::new();
    for f in ["monitors.conf", "hyprland.conf", "custom/general.conf"] {
        text.push_str(&std::fs::read_to_string(dir.join(f)).unwrap_or_default());
        text.push('\n');
    }
    for line in text.lines() {
        let Some((k, v)) = line.split('#').next()?.split_once('=') else { continue };
        if k.trim() != "monitor" {
            continue;
        }
        let parts: Vec<&str> = v.split(',').map(str::trim).collect();
        let sel = parts.first().copied().unwrap_or("");
        let hit = sel == name || sel.strip_prefix("desc:").is_some_and(|d| edid_name.is_some_and(|e| !e.is_empty() && d.contains(e)));
        if !hit {
            continue;
        }
        let res = parts.get(1).copied().unwrap_or("preferred");
        let (wh, hz) = res.split_once('@').unwrap_or((res, "0"));
        let (w, h) = wh.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or((0, 0));
        let scale = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(1.0);
        return Some((w, h, hz.parse().unwrap_or(0.0), scale));
    }
    None
}

/// The monitor name from an EDID (its 0xFC descriptor), as Hyprland's `desc:` holds it.
fn edid_name(edid: &[u8]) -> Option<String> {
    (0..4).map(|i| 54 + i * 18).find_map(|o| {
        let d = edid.get(o..o + 18)?;
        (d[0] == 0 && d[1] == 0 && d[3] == 0xFC).then(|| String::from_utf8_lossy(&d[5..]).trim_end_matches(['\n', ' ', '\0']).trim().to_string())
    })
}

/// The theme's default arrow (XCURSOR_THEME, else "default") at `size` pixels, ARGB, and its hotspot.
fn load_cursor(size: u32) -> Option<(MemoryRenderBuffer, (i32, i32))> {
    let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
    let path = xcursor::CursorTheme::load(&theme).load_icon("default").or_else(|| xcursor::CursorTheme::load("Adwaita").load_icon("default"))?;
    let images = xcursor::parser::parse_xcursor(&std::fs::read(path).ok()?)?;
    let img = images.iter().min_by_key(|i| (i.size as i64 - size as i64).abs())?;
    let buf = MemoryRenderBuffer::from_slice(&img.pixels_rgba.chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect::<Vec<u8>>(), Fourcc::Argb8888, (img.width as i32, img.height as i32), 1, Transform::Normal, None);
    Some((buf, (img.xhot as i32, img.yhot as i32)))
}

pub fn run(opts: Options) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<Loom> = EventLoop::try_new()?;
    let display: Display<Loom> = Display::new()?;
    let mut state = Loom::new(display.handle(), event_loop.get_signal());
    state.loop_handle = Some(event_loop.handle());

    // the session: the GPU and input devices, from logind (seatd)
    let (session, session_events) = LibSeatSession::new().map_err(|e| format!("no seat (run it from a TTY, logged in): {e:?}"))?;
    eprintln!("loom: seat {}", session.seat());

    // the GPU and its screen
    let path = primary_gpu(session.seat())?.or_else(|| all_gpus(session.seat()).ok()?.into_iter().next()).ok_or("no GPU")?;
    let fd = session.clone().open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (mut drm, drm_events) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    // Safety: the display and context live as long as the renderer
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let renderer = unsafe { GlesRenderer::new(EGLContext::new(&egl)?)? };
    eprintln!("loom: GPU {}", path.display());

    // the screen: the built-in panel first, else the first one connected
    let res = drm.resource_handles()?;
    let mut conns: Vec<connector::Info> = res.connectors().iter().filter_map(|c| drm.get_connector(*c, true).ok()).filter(|i| i.state() == connector::State::Connected).collect();
    conns.sort_by_key(|i| !matches!(i.interface(), connector::Interface::EmbeddedDisplayPort | connector::Interface::LVDS));
    let conn = conns.into_iter().next().ok_or("no screen connected")?;
    let name = format!("{}-{}", conn.interface().as_str(), conn.interface_id());
    let edid = drm.get_properties(conn.handle()).ok().and_then(|props| {
        props.iter().find_map(|(h, v)| (drm.get_property(*h).ok()?.name().to_str().ok()? == "EDID").then(|| drm.get_property_blob(*v).ok()).flatten())
    });
    let edid_name = edid.as_deref().and_then(edid_name);
    let rule = monitor_rule(&name, edid_name.as_deref());
    // the mode the rule names (size and refresh), else the panel's preferred one
    let mode = rule
        .and_then(|(w, h, hz, _)| conn.modes().iter().filter(|m| m.size() == (w, h)).min_by_key(|m| ((m.vrefresh() as f64 - hz).abs() * 100.0) as i64).copied())
        .or_else(|| conn.modes().iter().find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED)).copied())
        .or_else(|| conn.modes().first().copied())
        .ok_or("the screen has no mode")?;
    let scale = rule.map(|r| r.3).unwrap_or(1.0);
    let crtc: crtc::Handle = conn
        .encoders()
        .iter()
        .filter_map(|e| drm.get_encoder(*e).ok())
        .find_map(|e| res.filter_crtcs(e.possible_crtcs()).into_iter().next())
        .ok_or("no CRTC for the screen")?;
    eprintln!("loom: screen {name} ({}) {}x{}@{} scale {scale}", edid_name.as_deref().unwrap_or("?"), mode.size().0, mode.size().1, mode.vrefresh());
    let surface = drm.create_surface(crtc, mode, &[conn.handle()])?;

    let (mw, mh) = conn.size().unwrap_or((0, 0));
    let output = Output::new(name.clone(), PhysicalProperties { size: (mw as i32, mh as i32).into(), subpixel: Subpixel::Unknown, make: edid_name.clone().unwrap_or_default(), model: name.clone() });
    let _global = output.create_global::<Loom>(&state.dh);
    let wl_mode = Mode::from(mode);
    let out_scale = if (scale - scale.round()).abs() < 1e-6 { Scale::Integer(scale.round() as i32) } else { Scale::Fractional(scale) };
    output.change_current_state(Some(wl_mode), Some(Transform::Normal), Some(out_scale), Some((0, 0).into()));
    output.set_preferred(wl_mode);
    state.space.map_output(&output, (0, 0));
    state.output = Some(output.clone());

    let node = DrmNode::from_file(fd.clone()).ok();
    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), node.map(Into::into));
    let formats = renderer.dmabuf_formats();
    let compositor = DrmCompositor::new(&output, surface, None, allocator, exporter, [Fourcc::Argb8888, Fourcc::Xrgb8888], formats, drm.cursor_size(), Some(gbm.clone())).map_err(|e| format!("DRM compositor: {e:?}"))?;

    // programs' GPU buffers (linux-dmabuf with feedback), the renderer shared with the import
    crate::gpu::offer(&mut state, &renderer);
    let renderer = Rc::new(RefCell::new(renderer));
    state.gpu = Some(renderer.clone());

    // input
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&session.seat()).map_err(|_| "libinput: no seat")?;
    event_loop.handle().insert_source(LibinputInputBackend::new(libinput.clone()), |ev, _, state: &mut Loom| state.tty_input(ev))?;

    let refresh = (mode.vrefresh().max(1)) as u64;
    let cursor_px = (24.0 * scale).round() as u32;
    state.tty = Some(Tty { session: session.clone(), drm, compositor, libinput, pending: false, active: true, cursor: load_cursor(cursor_px), period: Duration::from_micros(1_000_000 / refresh), vt_switch: None, owed_at: std::time::Instant::now() });

    // switched away (Ctrl+Alt+F-key) and back
    event_loop.handle().insert_source(session_events, |ev, _, state: &mut Loom| {
        let Some(t) = state.tty.as_mut() else { return };
        match ev {
            SessionEvent::PauseSession => {
                eprintln!("loom: session paused (switched away)");
                t.libinput.suspend();
                t.drm.pause();
                t.active = false;
                t.pending = false;
            }
            SessionEvent::ActivateSession => {
                eprintln!("loom: session back");
                let _ = t.libinput.resume();
                let _ = t.drm.activate(false);
                let _ = t.compositor.reset_state();
                t.active = true;
                t.pending = false;
                state.dirty = true;
            }
        }
    })?;

    // the screen took the frame: the next may be drawn, the programs may draw theirs
    event_loop.handle().insert_source(drm_events, |ev, _, state: &mut Loom| match ev {
        DrmEvent::VBlank(_) => {
            if let Some(t) = state.tty.as_mut() {
                let _ = t.compositor.frame_submitted();
                t.pending = false;
            }
            state.send_frames();
        }
        DrmEvent::Error(e) => eprintln!("loom: DRM error: {e}"),
    })?;

    // the Wayland socket and the rest, as the other backends
    let socket = match &opts.socket {
        Some(n) => ListeningSocketSource::with_name(n)?,
        None => ListeningSocketSource::new_auto()?,
    };
    let socket_name = socket.socket_name().to_string_lossy().to_string();
    state.socket_name = Some(socket_name.clone());
    let conf = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/hypr/hyprland.conf");
    state.binds = crate::binds::Binds::load(&conf);
    // the config's `env =` lines, as Hyprland sets them for everything it starts (themes, cursor …)
    for (k, v) in state.binds.env.clone() {
        if !matches!(k.as_str(), "WAYLAND_DISPLAY" | "DISPLAY" | "HYPRLAND_INSTANCE_SIGNATURE" | "XDG_RUNTIME_DIR") {
            // Safety: one thread so far (before the event loop runs)
            unsafe { std::env::set_var(&k, &v) };
        }
    }
    eprintln!("loom: {} env lines from the Hyprland config", state.binds.env.len());
    state.hypr = Some(crate::hypr_ipc::start(&event_loop.handle())?);
    match crate::xwayland::start(&event_loop.handle(), &mut state) {
        Ok(n) => state.x_display = Some(n),
        Err(e) => eprintln!("loom: no Xwayland: {e}"),
    }
    event_loop.handle().insert_source(socket, |stream, _, state: &mut Loom| {
        let _ = state.dh.insert_client(stream, client_data());
    })?;
    event_loop.handle().insert_source(Generic::new(display, Interest::READ, smithay::reexports::calloop::Mode::Level), |_, display, state: &mut Loom| {
        // Safety: the display is not dropped while the source lives
        unsafe { display.get_mut().dispatch_clients(state).ok() };
        Ok(PostAction::Continue)
    })?;
    eprintln!("loom: on the screen, WAYLAND_DISPLAY={socket_name}; Ctrl+Alt+Backspace quits");

    if let Some(m) = opts.for_minutes {
        eprintln!("loom: quits by itself in {m} min (--for)");
        event_loop.handle().insert_source(Timer::from_duration(Duration::from_secs(m * 60)), |_, _, state: &mut Loom| {
            eprintln!("loom: --for time is up, quitting");
            state.signal.stop();
            TimeoutAction::Drop
        })?;
    }

    // the programs it starts get its display and signature
    for cmd in [opts.run.join(" ")].into_iter().filter(|c| !c.is_empty()) {
        state.spawn(&cmd);
    }

    // the first tries: a stats line a second while anything happens (frames, render time, GPU vs CPU buffers)
    state.start_stats();
    state.dirty = true;
    event_loop.run(None, &mut state, |state| {
        state.tty_frame(&renderer);
        state.after_dispatch();
    })?;
    eprintln!("loom: bye");
    Ok(())
}

impl Loom {
    /// A frame for the screen when something changed and the last one is out.
    fn tty_frame(&mut self, renderer: &Rc<RefCell<GlesRenderer>>) {
        let Some(t) = self.tty.as_ref() else { return };
        if self.frames_owed && !t.pending && t.owed_at.elapsed() >= t.period {
            self.send_frames();
        }
        // screen or window copies asked for (grim, previews): drawn even when nothing else changed
        if self.captures_ready() {
            crate::capture::run::<_, smithay::backend::renderer::gles::GlesTexture>(self, &mut *renderer.borrow_mut());
        }
        let Some(t) = self.tty.as_ref() else { return };
        if !t.active || t.pending || !(self.dirty || self.animating()) {
            return;
        }
        let Some(output) = self.output.clone() else { return };
        self.dirty = false;
        let mut r = renderer.borrow_mut();
        let now = std::time::Instant::now();
        let mut elements: Vec<TtyElement> = Vec::new();
        // the cursor, front-most: the program's own image, the theme's arrow, or none
        let os = output.current_scale().fractional_scale();
        let at = self.pointer_at.to_physical(os);
        match &self.cursor_status {
            CursorImageStatus::Hidden => {}
            CursorImageStatus::Surface(s) => {
                let hot = smithay::wayland::compositor::with_states(s, |st| st.data_map.get::<smithay::input::pointer::CursorImageSurfaceData>().map(|d| d.lock().unwrap().hotspot)).unwrap_or_default();
                let loc = (at - hot.to_f64().to_physical(os)).to_i32_round();
                elements.extend(render_elements_from_surface_tree(&mut *r, s, loc, os, 1.0, Kind::Cursor).into_iter().map(TtyElement::Surface));
            }
            CursorImageStatus::Named(_) => {
                if let Some((buf, hot)) = &t.cursor {
                    let loc: Point<f64, smithay::utils::Physical> = (at.x - hot.0 as f64, at.y - hot.1 as f64).into();
                    if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(&mut *r, loc, buf, None, None, None, Kind::Cursor) {
                        elements.push(TtyElement::Cursor(e));
                    }
                }
            }
        }
        elements.extend(crate::render::elements(self, &mut *r, &output, now).into_iter().map(TtyElement::Surface));
        let t = self.tty.as_mut().expect("tty");
        let t0 = std::time::Instant::now();
        let rendered = t.compositor.render_frame(&mut *r, &elements, [0.08, 0.08, 0.1, 1.0], FrameFlags::DEFAULT);
        let us = t0.elapsed().as_micros() as u64;
        self.stats.render_us += us;
        self.stats.render_max_us = self.stats.render_max_us.max(us);
        let t = self.tty.as_mut().expect("tty");
        match rendered {
            Ok(res) if !res.is_empty => match t.compositor.queue_frame(()) {
                Ok(()) => {
                    t.pending = true;
                    self.stats.frames += 1;
                }
                Err(e) => eprintln!("loom: queue frame: {e:?}"),
            },
            // nothing changed on screen: the programs still get their frame, a refresh from now
            Ok(_) => {
                let period = t.period;
                t.owed_at = now;
                self.wake_in(period);
                self.frames_owed = true;
            }
            Err(e) => eprintln!("loom: render: {e:?}"),
        }
        drop(r);
        self.frames += 1;
        crate::capture::run::<_, smithay::backend::renderer::gles::GlesTexture>(self, &mut *renderer.borrow_mut());
    }

    /// Frame callbacks: the programs may draw their next frame.
    pub fn send_frames(&mut self) {
        self.frames_owed = false;
        let Some(output) = self.output.clone() else { return };
        let ms = Duration::from_millis(self.now_ms() as u64);
        for w in self.space.elements() {
            if self.wants_frame(w) {
                w.send_frame(&output, ms, Some(Duration::ZERO), |_, _| Some(output.clone()));
            }
        }
        for l in smithay::desktop::layer_map_for_output(&output).layers() {
            l.send_frame(&output, ms, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        self.space.refresh();
        self.popups.cleanup();
    }

    /// One libinput event.
    fn tty_input(&mut self, ev: InputEvent<LibinputInputBackend>) {
        let time = self.now_ms();
        match ev {
            InputEvent::DeviceAdded { mut device } => {
                // touchpads: tap to click, as Hyprland's default
                if device.config_tap_finger_count() > 0 {
                    let _ = device.config_tap_set_enabled(true);
                }
            }
            InputEvent::Keyboard { event } => {
                self.key_input(event.key_code(), event.state(), time);
                if let Some(vt) = self.tty.as_mut().and_then(|t| t.vt_switch.take()) {
                    if let Some(t) = self.tty.as_mut() {
                        let _ = t.session.change_vt(vt);
                    }
                }
            }
            InputEvent::PointerMotion { event } => {
                let size = self.output.as_ref().and_then(|o| self.space.output_geometry(o)).map(|g| g.size).unwrap_or_default();
                let at = self.pointer_at + event.delta();
                let at = (at.x.clamp(0.0, size.w as f64 - 1.0), at.y.clamp(0.0, size.h as f64 - 1.0));
                self.pointer_move(at.into(), time);
                self.dirty = true;
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let size = self.output.as_ref().and_then(|o| self.space.output_geometry(o)).map(|g| g.size).unwrap_or_default();
                self.pointer_move(event.position_transformed(size), time);
                self.dirty = true;
            }
            InputEvent::PointerButton { event } => self.pointer_button(event.button_code(), event.state() == ButtonState::Pressed, time),
            InputEvent::PointerAxis { event } => {
                let v = event.amount(Axis::Vertical).or_else(|| event.amount_v120(Axis::Vertical).map(|v| v * 15.0 / 120.0)).unwrap_or(0.0);
                let h = event.amount(Axis::Horizontal).or_else(|| event.amount_v120(Axis::Horizontal).map(|v| v * 15.0 / 120.0)).unwrap_or(0.0);
                let notches = Some((event.amount_v120(Axis::Vertical).unwrap_or(0.0), event.amount_v120(Axis::Horizontal).unwrap_or(0.0)));
                self.pointer_scroll(event.source(), v, h, notches, time);
            }
            _ => {}
        }
    }
}
