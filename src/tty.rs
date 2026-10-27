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
        input::{AbsolutePositionEvent, Axis, ButtonState, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _, GestureSwipeUpdateEvent as _, InputEvent, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent},
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
};

use crate::state::Loom;

smithay::backend::renderer::element::render_elements! {
    pub TtyElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Window=crate::render::GlesElement,
    Cursor=MemoryRenderBufferRenderElement<GlesRenderer>,
}

type Compositor = DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// One connected screen: its output, where KMS shows it, its frames.
pub struct Screen {
    pub output: Output,
    pub global: smithay::reexports::wayland_server::backend::GlobalId,
    pub conn: connector::Handle,
    pub crtc: crtc::Handle,
    pub compositor: Compositor,
    /// a frame is on its way to the screen (the next waits for its vblank)
    pub pending: bool,
    /// something changed since its last frame
    pub needs: bool,
    /// the refresh period, for frame callbacks when nothing reached the screen
    pub period: Duration,
    /// switched off (dpms off): nothing drawn until it is on again
    pub dark: bool,
    /// the connector's modes (a mode change picks one)
    pub modes: Vec<smithay::reexports::drm::control::Mode>,
    /// adaptive sync as the rule wants it (0 off, 1 on, 2 fullscreen only), whether the screen can, and on now
    pub vrr: u8,
    pub vrr_can: bool,
    pub vrr_on: bool,
    /// the theme's arrow at this screen's scale, and its hotspot (pixels)
    pub cursor: Option<(MemoryRenderBuffer, (i32, i32))>,
    /// named cursors (cursor-shape: text, pointer, resize …) loaded as asked for, at `cursor_px`
    pub cursors: std::collections::HashMap<&'static str, Option<(MemoryRenderBuffer, (i32, i32))>>,
    pub cursor_px: u32,
}

pub struct Tty {
    pub session: LibSeatSession,
    pub drm: DrmDevice,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub node: Option<DrmNode>,
    pub formats: smithay::backend::allocator::format::FormatSet,
    pub libinput: Libinput,
    pub screens: Vec<Screen>,
    /// the session is ours (not switched away)
    pub active: bool,
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
    /// the login session: the Hyprland config's exec-once programs started, as Hyprland does
    pub session: bool,
}

/// A Hyprland monitor rule for one screen: its mode (0 = preferred), refresh, scale, position.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rule {
    w: u16,
    h: u16,
    hz: f64,
    scale: f64,
    /// logical, None = "auto" (right of the others)
    pos: Option<(i32, i32)>,
    /// adaptive sync: 0 off, 1 on, 2 only while a window is fullscreen there (`vrr,2`)
    vrr: u8,
}

/// The rule for screen `name` (its description `desc`, "Make Model" as Hyprland's): `monitor =`
/// lines in the config, then `hl.monitor({ … })` in its Lua files (the shell's MonitorAutoMode
/// writes those); the last one that matches wins, as in Hyprland.
fn monitor_rule(name: &str, desc: Option<&str>) -> Option<Rule> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = PathBuf::from(&home).join(".config/hypr");
    // (selector, mode, position, scale, vrr) in the order they apply
    let mut lines: Vec<(String, String, String, String, String)> = Vec::new();
    for (k, v) in crate::config::hyprland_lines(&dir.join("hyprland.conf")) {
        if k == "monitor" {
            let p: Vec<String> = v.split(',').map(|s| s.trim().to_string()).collect();
            let at = |i: usize| p.get(i).cloned().unwrap_or_default();
            // the extra `key, value` pairs after the scale: vrr
            let vrr = p.iter().skip(4).position(|s| s == "vrr").map(|i| at(i + 5)).unwrap_or_default();
            lines.push((at(0), at(1), at(2), at(3), vrr));
        }
    }
    for f in ["lua/monitors.lua", "lua/monitor_auto.lua"] {
        for l in std::fs::read_to_string(dir.join(f)).unwrap_or_default().lines().filter(|l| !l.trim_start().starts_with("--")) {
            let Some((call, args)) = crate::hypr_lua::call(l, "hl.") else { continue };
            let Some(crate::hypr_lua::Val::Table(t)) = args.first().filter(|_| call == "monitor") else { continue };
            let get = |k: &str| t.get(k).map(|v| v.text()).unwrap_or_default();
            lines.push((get("output"), get("mode"), get("position"), get("scale"), get("vrr")));
        }
    }
    // desc: the start of the description, as Hyprland. Matching the model anywhere in it also
    // took a rule the shell had saved for loom's old "TL134ADXP03 eDP-1" at 60 Hz: the 180 Hz
    // panel ran at 60
    let hit = |sel: &str| sel == name || sel.strip_prefix("desc:").map(str::trim).is_some_and(|d| !d.is_empty() && desc.is_some_and(|e| e.starts_with(d)));
    let (_, mode, pos, scale, vrr) = lines.into_iter().filter(|l| hit(&l.0)).last()?;
    let (wh, hz) = mode.split_once('@').unwrap_or((&mode, "0"));
    let (w, h) = wh.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or((0, 0));
    let pos = pos.split_once('x').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)));
    let vrr = match vrr.trim() { "1" | "true" | "on" => 1, "2" | "3" => 2, _ => 0 };
    Some(Rule { w, h, hz: hz.parse().unwrap_or(0.0), scale: scale.parse().unwrap_or(1.0), pos, vrr })
}

/// The maker and model from an EDID, as Hyprland's description has them: the maker's name for
/// its PNP code (hwdata's pnp.ids; the code itself without it), the model from the 0xFC descriptor.
fn edid_make_model(edid: &[u8]) -> (String, String) {
    let model = edid_name(edid).unwrap_or_default();
    let code: String = edid
        .get(8..10)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .map(|v| [(v >> 10) & 31, (v >> 5) & 31, v & 31].iter().filter(|c| (1..=26).contains(*c)).map(|c| (b'A' + *c as u8 - 1) as char).collect())
        .unwrap_or_default();
    let tab = format!("{code}\t");
    let make = std::fs::read_to_string("/usr/share/hwdata/pnp.ids").ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix(tab.as_str()).map(|m| m.trim().to_string()))).unwrap_or(code);
    (make, model)
}

/// The monitor name from an EDID (its 0xFC descriptor).
fn edid_name(edid: &[u8]) -> Option<String> {
    (0..4).map(|i| 54 + i * 18).find_map(|o| {
        let d = edid.get(o..o + 18)?;
        (d[0] == 0 && d[1] == 0 && d[3] == 0xFC).then(|| String::from_utf8_lossy(&d[5..]).trim_end_matches(['\n', ' ', '\0']).trim().to_string())
    })
}

/// The theme's default arrow (XCURSOR_THEME, else "default") at `size` pixels, ARGB, and its hotspot.
fn load_cursor(name: &str, size: u32) -> Option<(MemoryRenderBuffer, (i32, i32))> {
    let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
    // the name or one of its old X names in the theme (Oxygen has closedhand, not grabbing), else in Adwaita, else the arrow
    let icon = name.parse::<smithay::input::pointer::CursorIcon>().ok();
    let names: Vec<&str> = std::iter::once(name).chain(icon.iter().flat_map(|i| i.alt_names().iter().copied())).collect();
    let t = xcursor::CursorTheme::load(&theme);
    let path = names.iter().find_map(|n| t.load_icon(n)).or_else(|| names.iter().find_map(|n| xcursor::CursorTheme::load("Adwaita").load_icon(n))).or_else(|| t.load_icon("default"))?;
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
    state.init_idle();
    // init.lua applied when it changes
    state.watch_config();

    // an older loom session on another console: ended first (restarting is just starting again)
    end_other_looms();

    // the session: the GPU and input devices, from logind (seatd)
    let (session, session_events) = seat_session().map_err(|e| format!("no seat (run it from a TTY, logged in): {e:?}"))?;
    eprintln!("loom: seat {}", session.seat());

    // the GPU and its screen
    let path = primary_gpu(session.seat())?.or_else(|| all_gpus(session.seat()).ok()?.into_iter().next()).ok_or("no GPU")?;
    let fd = session.clone().open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_events) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    // Safety: the display and context live as long as the renderer
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let renderer = unsafe { GlesRenderer::new(EGLContext::new(&egl)?)? };
    eprintln!("loom: GPU {}", path.display());

    // programs' GPU buffers (linux-dmabuf with feedback), the renderer shared with the import
    crate::gpu::offer(&mut state, &renderer);
    let formats = renderer.dmabuf_formats();
    let renderer = Rc::new(RefCell::new(renderer));
    state.gpu = Some(renderer.clone());

    // input
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&session.seat()).map_err(|_| "libinput: no seat")?;
    event_loop.handle().insert_source(LibinputInputBackend::new(libinput.clone()), |ev, _, state: &mut Loom| state.tty_input(ev))?;

    let node = DrmNode::from_file(fd.clone()).ok();
    state.tty = Some(Tty { session: session.clone(), drm, gbm: gbm.clone(), node, formats, libinput, screens: Vec::new(), active: true, vt_switch: None, owed_at: std::time::Instant::now() });
    // every connected screen, placed as the monitor rules say
    state.scan_screens();
    if state.outputs.is_empty() {
        return Err("no screen connected".into());
    }
    // a screen plugged in or out: scanned again
    let udev = smithay::backend::udev::UdevBackend::new(session.seat())?;
    event_loop.handle().insert_source(udev, |ev, _, state: &mut Loom| {
        if let smithay::backend::udev::UdevEvent::Changed { .. } = ev {
            state.scan_screens();
        }
    })?;

    // switched away (Ctrl+Alt+F-key) and back
    event_loop.handle().insert_source(session_events, |ev, _, state: &mut Loom| {
        let Some(t) = state.tty.as_mut() else { return };
        match ev {
            SessionEvent::PauseSession => {
                eprintln!("loom: session paused (switched away)");
                t.libinput.suspend();
                t.drm.pause();
                t.active = false;
                for s in &mut t.screens {
                    s.pending = false;
                }
            }
            SessionEvent::ActivateSession => {
                eprintln!("loom: session back");
                let _ = t.libinput.resume();
                let _ = t.drm.activate(false);
                for s in &mut t.screens {
                    let _ = s.compositor.reset_state();
                    s.pending = false;
                }
                t.active = true;
                state.dirty = true;
                // screens may have come or gone while away
                state.scan_screens();
                // another session may have set their colours: ours again
                for (n, m) in state.ctm.clone() {
                    state.set_ctm(&n, m);
                }
            }
        }
    })?;

    // a screen took its frame: its next may be drawn, the programs may draw theirs
    event_loop.handle().insert_source(drm_events, |ev, _, state: &mut Loom| match ev {
        DrmEvent::VBlank(crtc) => {
            // its own surfaces may draw their next frame (not every screen's)
            let o = state.tty.as_mut().and_then(|t| t.screens.iter_mut().find(|s| s.crtc == crtc)).map(|s| {
                let _ = s.compositor.frame_submitted();
                s.pending = false;
                s.output.clone()
            });
            match o {
                Some(o) => state.send_frames_for(&o),
                None => state.send_frames(),
            }
        }
        DrmEvent::Error(e) => eprintln!("loom: DRM error: {e}"),
    })?;

    // the Wayland socket and the rest, as the other backends
    let (socket_fd, socket_name) = crate::restart::listen(&event_loop.handle(), opts.socket.as_deref())?;
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
    // Java (Bitwig's UI) places its windows and menus wrongly under a compositor that does not
    // reparent X windows unless told so (the sway wiki's advice); the config may set it otherwise
    if std::env::var_os("_JAVA_AWT_WM_NONREPARENTING").is_none() {
        // Safety: one thread so far (before the event loop runs)
        unsafe { std::env::set_var("_JAVA_AWT_WM_NONREPARENTING", "1") };
    }
    // Qt 6 programs (the shell, Dotos, Vitals) reconnect when loom restarts in place (restart.rs)
    if std::env::var_os("QT_WAYLAND_RECONNECT").is_none() {
        // Safety: one thread so far (before the event loop runs)
        unsafe { std::env::set_var("QT_WAYLAND_RECONNECT", "1") };
    }
    eprintln!("loom: {} env lines from the Hyprland config", state.binds.env.len());
    state.hypr = Some(crate::hypr_ipc::start(&event_loop.handle())?);
    match crate::xwayland::start(&event_loop.handle(), &mut state) {
        Ok(n) => {
            state.x_display = Some(n);
            // on the real screen Xwayland starts now, with the user's X settings, before any X
            // program: started by the first one, that one read them too early (Bitwig's tiny cursor)
            event_loop.handle().insert_idle(|s| s.x_start_now());
        }
        Err(e) => eprintln!("loom: no Xwayland: {e}"),
    }
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
    for cmd in [opts.run.join(" ")].into_iter().filter(|c| !c.is_empty() && !crate::restart::restarted()) {
        state.spawn(&cmd);
    }
    if opts.session && !crate::restart::restarted() {
        eprintln!("loom: session: {} exec-once programs", state.binds.exec_once.len());
        for cmd in state.binds.exec_once.clone() {
            state.spawn(&cmd);
        }
    }

    // the first tries: a stats line a second while anything happens (frames, render time, GPU vs CPU buffers)
    // after `loom restart`: the screens show what they showed, reconnecting windows go back to their workspace
    crate::restart::restore_workspaces(&mut state);
    state.start_stats();
    state.dirty = true;
    event_loop.run(None, &mut state, |state| {
        state.tty_frame(&renderer);
        state.after_dispatch();
    })?;
    eprintln!("loom: bye");
    crate::restart::after_loop(state, event_loop, socket_fd, &socket_name)
}

impl Loom {
    /// A frame for each screen when something changed and its last one is out.
    fn tty_frame(&mut self, renderer: &Rc<RefCell<GlesRenderer>>) {
        let Some(t) = self.tty.as_ref() else { return };
        let period = t.screens.iter().map(|s| s.period).min().unwrap_or(Duration::from_millis(16));
        if self.frames_owed && t.screens.iter().all(|s| !s.pending) && t.owed_at.elapsed() >= period {
            self.send_frames();
        }
        // screen or window copies asked for (grim, previews): drawn even when nothing else changed
        if self.captures_ready() {
            crate::capture::run::<_, smithay::backend::renderer::gles::GlesTexture>(self, &mut *renderer.borrow_mut());
        }
        let off = self.screen_off;
        let Some(t) = self.tty.as_mut() else { return };
        if !t.active {
            return;
        }
        // dpms off: each screen cleared once (its CRTC off), then nothing drawn; on: drawn again
        for s in &mut t.screens {
            if off && !s.dark {
                if let Err(e) = s.compositor.clear() {
                    eprintln!("loom: screen off: {e}");
                }
                s.dark = true;
                s.pending = false;
            } else if !off && s.dark {
                s.dark = false;
                s.needs = true;
            }
        }
        if off {
            return;
        }
        self.apply_vrr();
        // what changed is due on every screen
        if self.dirty || self.animating() {
            self.dirty = false;
            for s in &mut self.tty.as_mut().expect("tty").screens {
                s.needs = true;
            }
        }
        let due: Vec<usize> = self.tty.as_ref().expect("tty").screens.iter().enumerate().filter(|(_, s)| s.needs && !s.pending).map(|(i, _)| i).collect();
        if due.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let mut owed = false;
        for i in due {
            let Some(output) = self.tty.as_ref().and_then(|t| t.screens.get(i)).map(|s| s.output.clone()) else { continue };
            let mut r = renderer.borrow_mut();
            let mut elements: Vec<TtyElement> = Vec::new();
            // the cursor, front-most, on the screen it is on: the program's own image, the theme's arrow, or none
            let og = self.space.output_geometry(&output).unwrap_or_default();
            let os = output.current_scale().fractional_scale();
            if og.to_f64().contains(self.pointer_at) {
                let at = (self.pointer_at - og.loc.to_f64()).to_physical(os);
                // a Super+drag shows what it does, as Hyprland: a grab while moving, a resize arrow while
                // resizing (the window's own cursor stayed during it)
                let status = match &self.drag {
                    Some(d) if !d.resize => CursorImageStatus::Named(smithay::input::pointer::CursorIcon::Grabbing),
                    Some(d) if d.was_tiled => CursorImageStatus::Named(smithay::input::pointer::CursorIcon::ColResize),
                    Some(_) => CursorImageStatus::Named(smithay::input::pointer::CursorIcon::SeResize),
                    None => self.cursor_status.clone(),
                };
                match &status {
                    CursorImageStatus::Hidden => {}
                    CursorImageStatus::Surface(s) => {
                        let hot = smithay::wayland::compositor::with_states(s, |st| st.data_map.get::<smithay::input::pointer::CursorImageSurfaceData>().map(|d| d.lock().unwrap().hotspot)).unwrap_or_default();
                        // an X program's cursor (Bitwig) comes in X pixels, which are this screen's own pixels
                        // (xscreens.rs): drawn 1:1, a small one grown to the theme arrow's size (24 or XCURSOR_SIZE)
                        let from_x = smithay::reexports::wayland_server::Resource::client(s).is_some_and(|c| self.x.client.as_ref().is_some_and(|x| x.id() == c.id()));
                        let scale = if from_x {
                            let want = std::env::var("XCURSOR_SIZE").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(24.0) * os;
                            let h = smithay::backend::renderer::utils::with_renderer_surface_state(s, |st| st.surface_size().map(|z| z.h as f64)).flatten().unwrap_or(want);
                            (want / h.max(1.0)).max(1.0)
                        } else {
                            os
                        };
                        let loc = (at - hot.to_f64().to_physical(scale)).to_i32_round();
                        elements.extend(render_elements_from_surface_tree(&mut *r, s, loc, scale, 1.0, Kind::Cursor).into_iter().map(TtyElement::Surface));
                    }
                    CursorImageStatus::Named(icon) => {
                        // the shape asked for (loaded once per screen), the arrow when the theme lacks it
                        let name = icon.name();
                        let shown = self.tty.as_mut().and_then(|t| t.screens.get_mut(i)).and_then(|s| {
                            if name == "default" {
                                return s.cursor.as_ref();
                            }
                            let px = s.cursor_px;
                            s.cursors.entry(name).or_insert_with(|| load_cursor(name, px)).as_ref().or(s.cursor.as_ref())
                        });
                        if let Some((buf, hot)) = shown {
                            let loc: Point<f64, smithay::utils::Physical> = (at.x - hot.0 as f64, at.y - hot.1 as f64).into();
                            if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(&mut *r, loc, buf, None, None, None, Kind::Cursor) {
                                elements.push(TtyElement::Cursor(e));
                            }
                        }
                    }
                }
            }
            elements.extend(crate::render::elements(self, &mut *r, &output, now).into_iter().map(TtyElement::Window));
            let s = &mut self.tty.as_mut().expect("tty").screens[i];
            s.needs = false;
            let t0 = std::time::Instant::now();
            let rendered = s.compositor.render_frame(&mut *r, &elements, [0.08, 0.08, 0.1, 1.0], FrameFlags::DEFAULT);
            let us = t0.elapsed().as_micros() as u64;
            match rendered {
                Ok(res) if !res.is_empty => match s.compositor.queue_frame(()) {
                    Ok(()) => {
                        s.pending = true;
                        self.stats.frames += 1;
                        self.shown += 1;
                    }
                    Err(e) => eprintln!("loom: queue frame on {}: {e:?}", output.name()),
                },
                // nothing changed on this screen: the programs still get their frame, a refresh from now
                Ok(_) => owed = true,
                Err(e) => eprintln!("loom: render on {}: {e:?}", output.name()),
            }
            self.stats.render_us += us;
            self.stats.render_max_us = self.stats.render_max_us.max(us);
        }
        if owed {
            if let Some(t) = self.tty.as_mut() {
                t.owed_at = now;
            }
            self.wake_in(period);
            self.frames_owed = true;
        }
        self.frames += 1;
        crate::capture::run::<_, smithay::backend::renderer::gles::GlesTexture>(self, &mut *renderer.borrow_mut());
    }

    /// One screen's refresh came: its programs may draw their next frame.
    pub fn send_frames_for(&mut self, o: &Output) {
        // what was owed came (the idle path below would send every screen's again)
        self.frames_owed = false;
        let ms = Duration::from_millis(self.now_ms() as u64);
        self.frames_to_output(o, ms);
        self.space.refresh();
        self.popups.cleanup();
    }

    /// Frame callbacks: the programs may draw their next frame.
    pub fn send_frames(&mut self) {
        self.frames_owed = false;
        let ms = Duration::from_millis(self.now_ms() as u64);
        self.frames_to_all(ms);
        self.space.refresh();
        self.popups.cleanup();
    }

    /// One libinput event.
    fn tty_input(&mut self, ev: InputEvent<LibinputInputBackend>) {
        let time = self.now_ms();
        match ev {
            InputEvent::DeviceAdded { device } => {
                if device.has_capability(smithay::reexports::input::DeviceCapability::TabletTool) {
                    self.tablet_added(&device);
                }
                let lines = self.input_lines();
                self.inputs.added(device, &lines);
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
                // the raw motion to programs that want it (games, 3D), and a locked or confined pointer
                if self.pointer_relative(event.delta(), event.delta_unaccel(), smithay::backend::input::Event::time(&event)) {
                    self.dirty = true;
                    return;
                }
                // over every screen: kept on one (the nearest) when it leaves them
                let at = self.clamp_to_outputs(self.pointer_at + event.delta());
                let at = self.confine(at);
                self.pointer_move(at, time);
                self.dirty = true;
            }
            InputEvent::PointerMotionAbsolute { event } => {
                // a tablet or touchscreen: on the focused screen
                let g = self.output.as_ref().and_then(|o| self.space.output_geometry(o)).unwrap_or_default();
                self.pointer_move(event.position_transformed(g.size) + g.loc.to_f64(), time);
                self.dirty = true;
            }
            InputEvent::PointerButton { event } => self.pointer_button(event.button_code(), event.state() == ButtonState::Pressed, time),
            InputEvent::DeviceRemoved { device } => {
                if device.has_capability(smithay::reexports::input::DeviceCapability::TabletTool) {
                    self.tablet_removed(&device);
                }
                self.inputs.removed(&device);
            }
            InputEvent::GestureSwipeBegin { event } => self.swipe_begin(event.fingers(), time),
            InputEvent::GestureSwipeUpdate { event } => self.swipe_update(event.delta(), time),
            InputEvent::GestureSwipeEnd { event } => self.swipe_end(event.cancelled(), time),
            InputEvent::GesturePinchBegin { event } => self.pinch_begin(event.fingers(), time),
            InputEvent::GesturePinchUpdate { event } => self.pinch_update(event.delta(), event.scale(), event.rotation(), time),
            InputEvent::GesturePinchEnd { event } => self.pinch_end(event.cancelled(), time),
            InputEvent::GestureHoldBegin { event } => self.hold_begin(event.fingers(), time),
            InputEvent::GestureHoldEnd { event } => self.hold_end(event.cancelled(), time),
            InputEvent::TabletToolAxis { event } => self.pen_axis(event),
            InputEvent::TabletToolProximity { event } => self.pen_proximity(event),
            InputEvent::TabletToolTip { event } => self.pen_tip(event),
            InputEvent::TabletToolButton { event } => self.pen_button(event),
            // the touchscreen, on the built-in panel
            InputEvent::TouchDown { event } => {
                let a = self.touch_area();
                self.touch_down(smithay::backend::input::TouchEvent::slot(&event), event.position_transformed(a.size) + a.loc.to_f64(), time);
            }
            InputEvent::TouchMotion { event } => {
                let a = self.touch_area();
                self.touch_motion(smithay::backend::input::TouchEvent::slot(&event), event.position_transformed(a.size) + a.loc.to_f64(), time);
            }
            InputEvent::TouchUp { event } => self.touch_up(smithay::backend::input::TouchEvent::slot(&event), time),
            InputEvent::TouchFrame { .. } => self.touch_frame(),
            InputEvent::TouchCancel { .. } => self.touch_cancel(),
            InputEvent::PointerAxis { event } => {
                let v = event.amount(Axis::Vertical).or_else(|| event.amount_v120(Axis::Vertical).map(|v| v * 15.0 / 120.0)).unwrap_or(0.0);
                let h = event.amount(Axis::Horizontal).or_else(|| event.amount_v120(Axis::Horizontal).map(|v| v * 15.0 / 120.0)).unwrap_or(0.0);
                let notches = Some((event.amount_v120(Axis::Vertical).unwrap_or(0.0), event.amount_v120(Axis::Horizontal).unwrap_or(0.0)));
                // the device.s scroll_factor
                let f = self.inputs.scroll_of(&smithay::backend::input::Event::device(&event));
                let notches = notches.map(|(a, b)| (a * f, b * f));
                self.pointer_scroll(event.source(), v * f, h * f, notches, time);
            }
            _ => {}
        }
    }
}

/// Other `loom --tty` processes of this user (an older session on another console): asked to
/// end (SIGTERM), killed if still there after 3 s. Its programs end with its display.
fn end_other_looms() {
    let me = std::process::id();
    let uid = unsafe { libc::getuid() };
    let others: Vec<i32> = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != me)
        .filter(|pid| std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "loom"))
        .filter(|pid| {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
            status.lines().find_map(|l| l.strip_prefix("Uid:")).and_then(|u| u.split_whitespace().next()?.parse::<u32>().ok()) == Some(uid)
        })
        .filter(|pid| std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.split(|b| *b == 0).any(|a| a == b"--tty")))
        .map(|pid| pid as i32)
        .collect();
    if others.is_empty() {
        return;
    }
    for &pid in &others {
        eprintln!("loom: ending the older loom session (pid {pid})");
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let alive = |pid: i32| std::path::Path::new(&format!("/proc/{pid}")).exists();
    for _ in 0..30 {
        if !others.iter().any(|&p| alive(p)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    for &pid in others.iter().filter(|&&p| alive(p)) {
        eprintln!("loom: pid {pid} did not end, killing it");
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

impl Loom {
    /// The connected screens now: new ones set up (mode, scale and place from the monitor
    /// rules; the built-in panel first), unplugged ones removed with their workspaces moved.
    pub fn scan_screens(&mut self) {
        let Some(t) = self.tty.as_mut() else { return };
        let Ok(res) = t.drm.resource_handles() else { return };
        let mut connected: Vec<connector::Info> = res.connectors().iter().filter_map(|c| t.drm.get_connector(*c, true).ok()).filter(|i| i.state() == connector::State::Connected).collect();
        connected.sort_by_key(|i| !matches!(i.interface(), connector::Interface::EmbeddedDisplayPort | connector::Interface::LVDS));
        // gone
        let gone: Vec<usize> = t.screens.iter().enumerate().filter(|(_, s)| !connected.iter().any(|c| c.handle() == s.conn)).map(|(i, _)| i).collect();
        let mut removed = Vec::new();
        for i in gone.into_iter().rev() {
            let s = t.screens.remove(i);
            removed.push((s.output.name(), s.global));
        }
        for (name, global) in removed {
            self.remove_output(&name);
            self.dh.remove_global::<Loom>(global);
        }
        // new
        for conn in connected {
            if self.tty.as_ref().is_some_and(|t| t.screens.iter().any(|s| s.conn == conn.handle())) {
                continue;
            }
            match self.add_screen(&conn, &res) {
                Ok(()) => {}
                Err(e) => eprintln!("loom: screen {}-{}: {e}", conn.interface().as_str(), conn.interface_id()),
            }
        }
        self.dirty = true;
    }

    fn add_screen(&mut self, conn: &connector::Info, res: &smithay::reexports::drm::control::ResourceHandles) -> Result<(), Box<dyn std::error::Error>> {
        let t = self.tty.as_mut().ok_or("no tty")?;
        let name = format!("{}-{}", conn.interface().as_str(), conn.interface_id());
        let edid = t.drm.get_properties(conn.handle()).ok().and_then(|props| {
            props.iter().find_map(|(h, v)| (t.drm.get_property(*h).ok()?.name().to_str().ok()? == "EDID").then(|| t.drm.get_property_blob(*v).ok()).flatten())
        });
        let (make, model) = edid.as_deref().map(edid_make_model).unwrap_or_default();
        let desc = format!("{make} {model}").trim().to_string();
        let rule = monitor_rule(&name, Some(desc.as_str()));
        // the mode the rule names (size and refresh), else the screen's preferred one
        let mode = rule
            .filter(|r| r.w > 0)
            .and_then(|r| conn.modes().iter().filter(|m| m.size() == (r.w, r.h)).min_by_key(|m| ((m.vrefresh() as f64 - r.hz).abs() * 100.0) as i64).copied())
            .or_else(|| conn.modes().iter().find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED)).copied())
            .or_else(|| conn.modes().first().copied())
            .ok_or("no mode")?;
        let scale = rule.map(|r| r.scale).unwrap_or(1.0);
        // a CRTC no other screen uses
        let used: Vec<crtc::Handle> = t.screens.iter().map(|s| s.crtc).collect();
        let crtc = conn
            .encoders()
            .iter()
            .filter_map(|e| t.drm.get_encoder(*e).ok())
            .flat_map(|e| res.filter_crtcs(e.possible_crtcs()))
            .find(|c| !used.contains(c))
            .ok_or("no free CRTC")?;
        eprintln!("loom: screen {name} ({desc}) {}x{}@{} scale {scale}", mode.size().0, mode.size().1, mode.vrefresh());
        let surface = t.drm.create_surface(crtc, mode, &[conn.handle()])?;
        let (mw, mh) = conn.size().unwrap_or((0, 0));
        let output = Output::new(name.clone(), PhysicalProperties { size: (mw as i32, mh as i32).into(), subpixel: Subpixel::Unknown, make, model });
        let global = output.create_global::<Loom>(&self.dh);
        let wl_mode = Mode::from(mode);
        let out_scale = if (scale - scale.round()).abs() < 1e-6 { Scale::Integer(scale.round() as i32) } else { Scale::Fractional(scale) };
        output.change_current_state(Some(wl_mode), Some(Transform::Normal), Some(out_scale), None);
        output.set_preferred(wl_mode);
        for m in conn.modes() {
            output.add_mode(Mode::from(*m));
        }
        let t = self.tty.as_mut().ok_or("no tty")?;
        let allocator = GbmAllocator::new(t.gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let exporter = GbmFramebufferExporter::new(t.gbm.clone(), t.node.map(Into::into));
        let compositor = DrmCompositor::new(&output, surface, None, allocator, exporter, [Fourcc::Argb8888, Fourcc::Xrgb8888], t.formats.clone(), t.drm.cursor_size(), Some(t.gbm.clone())).map_err(|e| format!("DRM compositor: {e:?}"))?;
        let vrr_can = compositor.vrr_supported(conn.handle()).is_ok_and(|v| !matches!(v, smithay::backend::drm::VrrSupport::NotSupported));
        let refresh = (mode.vrefresh().max(1)) as u64;
        t.screens.push(Screen {
            output: output.clone(),
            global,
            conn: conn.handle(),
            crtc,
            compositor,
            pending: false,
            needs: true,
            period: Duration::from_micros(1_000_000 / refresh),
            dark: false,
            modes: conn.modes().to_vec(),
            vrr: rule.map(|r| r.vrr).unwrap_or(0),
            vrr_can,
            vrr_on: false,
            cursor: load_cursor("default", (24.0 * scale).round() as u32),
            cursors: Default::default(),
            cursor_px: (24.0 * scale).round() as u32,
        });
        // where it goes: the rule's place, else right of the others
        let at = match rule.and_then(|r| r.pos) {
            Some(p) => p,
            None => (self.outputs.iter().filter_map(|o| self.space.output_geometry(o)).map(|g| g.loc.x + g.size.w).max().unwrap_or(0), 0),
        };
        let name = output.name();
        self.add_output(output, at.into());
        // its colour matrix: ours, else none (a night light the other session left on is cleared)
        let m = self.ctm.get(&name).copied().unwrap_or(IDENTITY);
        self.set_ctm(&name, m);
        Ok(())
    }
}

impl Loom {
    /// A screen's colour matrix (hyprsunset's night light), done by the display hardware: the
    /// CRTC's CTM property, as Hyprland does it (the identity clears it). Kept, so the session
    /// coming back or the screen plugged in again gets it.
    pub fn set_ctm(&mut self, name: &str, m: [f64; 9]) {
        self.ctm.insert(name.to_string(), m);
        let Some(t) = self.tty.as_ref() else {
            eprintln!("loom: colour matrix for {name} kept (no real screen): {:?}", [m[0], m[4], m[8]]);
            return;
        };
        let Some(s) = t.screens.iter().find(|s| s.output.name() == name) else { return };
        match apply_ctm(&t.drm, s.crtc, m) {
            Ok(()) => eprintln!("loom: colour matrix on {name}: {:?}", [m[0], m[4], m[8]]),
            Err(e) => eprintln!("loom: colour matrix on {name}: {e}"),
        }
    }
}

const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

/// The matrix to the CRTC's CTM property: S31.32 sign-magnitude numbers (the kernel's
/// drm_color_ctm), none for the identity.
fn apply_ctm(drm: &DrmDevice, crtc: crtc::Handle, m: [f64; 9]) -> std::io::Result<()> {
    let props = drm.get_properties(crtc)?;
    let (handles, _) = props.as_props_and_values();
    let prop = *handles
        .iter()
        .find(|h| drm.get_property(**h).is_ok_and(|p| p.name().to_str() == Ok("CTM")))
        .ok_or_else(|| std::io::Error::other("the screen has no CTM property"))?;
    // a matrix that would blank or blow out the screen is refused (each entry 0..2, none negative)
    if m.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 2.0) {
        return Err(std::io::Error::other("matrix out of range"));
    }
    let value: u64 = if m == IDENTITY {
        0
    } else {
        let blob: [u64; 9] = m.map(|v| ((v.abs() * (1u64 << 32) as f64).round() as u64) & !(1u64 << 63));
        drm.create_property_blob(&blob)?.into()
    };
    drm.set_property(crtc, prop, value)
}

impl Loom {
    /// The screen a fullscreen window is shown on.
    pub fn fullscreen_output(&self) -> Option<String> {
        let f = self.fullscreen.as_ref()?;
        let ws = self.managed.iter().find(|m| &m.window == f)?.ws;
        Some(self.output_showing(ws)?.name())
    }

    /// Adaptive sync on each screen as its rule says: on, off, or on while a window is fullscreen
    /// there (vrr 2: a game's own frame rate, the desktop at the panel's).
    fn apply_vrr(&mut self) {
        let full = self.fullscreen_output();
        let Some(t) = self.tty.as_mut() else { return };
        for s in &mut t.screens {
            let want = s.vrr_can && match s.vrr { 1 => true, 2 => full.as_deref() == Some(s.output.name().as_str()), _ => false };
            if want == s.vrr_on {
                continue;
            }
            match s.compositor.use_vrr(want) {
                Ok(()) => {
                    s.vrr_on = want;
                    eprintln!("loom: {} adaptive sync {}", s.output.name(), if want { "on" } else { "off" });
                }
                Err(e) => {
                    // not to be asked again every frame
                    eprintln!("loom: {} adaptive sync: {e:?}", s.output.name());
                    s.vrr_can = false;
                }
            }
        }
    }

    /// Whether adaptive sync is on for screen `name` now (hyprctl monitors).
    pub fn vrr_on(&self, name: &str) -> bool {
        self.tty.as_ref().is_some_and(|t| t.screens.iter().any(|s| s.vrr_on && s.output.name() == name))
    }
}

#[cfg(test)]
mod vrr_tests {
    #[test]
    fn reads_vrr_from_monitor_lines() {
        let home = std::env::temp_dir().join(format!("loom-vrr-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".config/hypr")).unwrap();
        std::fs::write(
            home.join(".config/hypr/hyprland.conf"),
            "monitor=desc:Tianma Microelectronics Ltd. TL134ADXP03,2560x1600@180.0,0x0,1.6,vrr,2\nmonitor=desc:AOC 2590G5,1920x1080@60,1600x0,1,vrr,0\nmonitor=HDMI-A-1,preferred,auto,1\nmonitor=desc:TL134ADXP03 eDP-1,2560x1600@60,0x0,1.6\n",
        )
        .unwrap();
        // SAFETY: the only test reading HOME
        unsafe { std::env::set_var("HOME", &home) };
        // desc: is the start of "Make Model": the later rule saved for "TL134ADXP03 eDP-1" is not it
        let edp = super::monitor_rule("eDP-1", Some("Tianma Microelectronics Ltd. TL134ADXP03")).unwrap();
        assert_eq!((edp.w, edp.h, edp.vrr), (2560, 1600, 2));
        assert!((edp.hz - 180.0).abs() < 1e-9, "{}", edp.hz);
        assert!((edp.scale - 1.6).abs() < 1e-9);
        assert_eq!(super::monitor_rule("DP-12", Some("AOC 2590G5")).unwrap().vrr, 0);
        assert_eq!(super::monitor_rule("HDMI-A-1", None).unwrap().vrr, 0);
        let _ = std::fs::remove_dir_all(&home);
    }
}


#[cfg(test)]
mod edid_tests {
    #[test]
    fn maker_from_the_pnp_code() {
        // bytes 8-9: TMA packed in 5 bits a letter; the model in a 0xFC descriptor at 54
        let mut e = vec![0u8; 128];
        let v: u16 = (20 << 10) | (13 << 5) | 1;
        e[8..10].copy_from_slice(&v.to_be_bytes());
        e[54..59].copy_from_slice(&[0, 0, 0, 0xFC, 0]);
        e[59..71].copy_from_slice(b"TL134ADXP03\n");
        let (make, model) = super::edid_make_model(&e);
        assert_eq!(model, "TL134ADXP03");
        // hwdata names it; without hwdata the code stands for it
        assert!(make == "Tianma Microelectronics Ltd." || make == "TMA", "{make}");
    }
}

/// The seat from logind. Right after `loom restart` the old one may not be let go yet: tried again
/// for up to two seconds.
fn seat_session() -> Result<(LibSeatSession, smithay::backend::session::libseat::LibSeatSessionNotifier), smithay::backend::session::libseat::Error> {
    let mut tries = 0;
    loop {
        match LibSeatSession::new() {
            Err(e) if crate::restart::restarted() && tries < 20 => {
                tries += 1;
                eprintln!("loom: seat not free yet ({e:?}), again");
                std::thread::sleep(Duration::from_millis(100));
            }
            r => return r,
        }
    }
}
