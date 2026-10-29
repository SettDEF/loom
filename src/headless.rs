//! The headless backend: no screen, an output drawn into memory with pixman, 60 frames a second.
//! `--snapshot` writes what it shows as a PNG after `--wait` and quits (how every milestone is
//! tested without touching the running session).

use std::{path::PathBuf, process::Command, time::Duration};

use std::{cell::RefCell, rc::Rc};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{damage::OutputDamageTracker, gles::GlesTexture, pixman::PixmanRenderer, Bind, ExportMem, Offscreen, Texture},
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, PostAction,
        },
        pixman::{FormatCode, Image},
        wayland_server::Display,
    },
    utils::Transform,
};

use crate::state::Loom;

pub struct Options {
    pub size: (i32, i32),
    pub snapshot: Option<PathBuf>,
    pub wait: Duration,
    pub socket: Option<String>,
    pub run: Vec<String>,
    /// scripted input (--keys, --click), one step every 25 ms from `script_at`
    pub script: Vec<crate::input::Action>,
    pub script_at: Duration,
    /// every frame drawn saved here as a PNG (animations as pictures; at most 400)
    pub record: Option<PathBuf>,
    /// a Hyprland config whose binds to use (--binds; none by default headless: the user's binds talk to their running shell)
    pub binds: Option<PathBuf>,
    /// draw on the GPU (surfaceless) and offer linux-dmabuf, as on a real screen (--gpu)
    pub gpu: bool,
    /// more screens, right of the first (scale 1): only the first is drawn and shot; the others show through grim -o
    pub more: Vec<(i32, i32)>,
    /// the output's scale (--scale 1.6)
    pub scale: f64,
}

pub fn run(opts: Options) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<Loom> = EventLoop::try_new()?;
    let display: Display<Loom> = Display::new()?;
    let mut state = Loom::new(display.handle(), event_loop.get_signal());

    // clients connect here
    let (socket_fd, socket_name) = crate::restart::listen(&event_loop.handle(), opts.socket.as_deref())?;
    state.socket_name = Some(socket_name.clone());
    state.loop_handle = Some(event_loop.handle());
    state.init_idle();
    // init.lua applied when it changes
    state.watch_config();
    if let Some(p) = &opts.binds {
        state.binds = crate::binds::Binds::load(p);
        eprintln!("loom: {} binds from {}", state.binds.binds.len(), p.display());
    }
    state.hypr = Some(crate::hypr_ipc::start(&event_loop.handle())?);
    // loom's night light on hyprsunset's socket (nightlight.rs)
    if let Some(dir) = state.hypr.as_ref().map(|h| h.dir.clone()) {
        if let Err(e) = crate::nightlight::start(&event_loop.handle(), &dir) {
            eprintln!("loom: night light socket: {e}");
        }
    }
    state.headless = true;
    // X programs: the display reserved now, Xwayland started by the first X program
    match crate::xwayland::start(&event_loop.handle(), &mut state) {
        Ok(n) => state.x_display = Some(n),
        Err(e) => eprintln!("loom: no Xwayland: {e}"),
    }
    event_loop.handle().insert_source(Generic::new(display, Interest::READ, smithay::reexports::calloop::Mode::Level), |_, display, state: &mut Loom| {
        // Safety: the display is not dropped while the source lives
        unsafe { display.get_mut().dispatch_clients(state).ok() };
        Ok(PostAction::Continue)
    })?;
    eprintln!("loom: WAYLAND_DISPLAY={socket_name}");

    // the output
    let (w, h) = opts.size;
    let output = Output::new("HEADLESS-1".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "loom".into(), model: "headless".into() });
    let global = output.create_global::<Loom>(&state.dh);
    state.output_globals.insert(output.name(), global);
    let mode = Mode { size: (w, h).into(), refresh: 60_000 };
    // the scale the real screen would have (--scale 1.6: the Z13 panel), logical size = pixels / scale
    let scale = if (opts.scale - 1.0).abs() < 1e-6 { Scale::Integer(1) } else { Scale::Fractional(opts.scale) };
    output.change_current_state(Some(mode), Some(Transform::Normal), Some(scale), Some((0, 0).into()));
    output.set_preferred(mode);
    state.add_output(output.clone(), (0, 0).into());
    // more screens (--size A,B): to the right of each other, at scale 1
    let mut x = (w as f64 / opts.scale).round() as i32;
    for (i, &(mw, mh)) in opts.more.iter().enumerate() {
        let o = Output::new(format!("HEADLESS-{}", i + 2), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "loom".into(), model: "headless".into() });
        let g = o.create_global::<Loom>(&state.dh);
        state.output_globals.insert(o.name(), g);
        let m = Mode { size: (mw, mh).into(), refresh: 60_000 };
        o.change_current_state(Some(m), Some(Transform::Normal), Some(Scale::Integer(1)), None);
        o.set_preferred(m);
        state.add_output(o, (x, 0).into());
        x += mw;
    }

    let damage = OutputDamageTracker::from_output(&output);
    let fo = FrameOpts { snapshot: opts.snapshot.clone(), wait: opts.wait, record: opts.record.clone(), size: (w, h) };
    // a frame when something changed or an animation runs (at most one per 16 ms); an idle
    // desktop draws nothing and loom sleeps: no timer ticks (battery)
    if opts.snapshot.is_some() {
        state.wake_in(opts.wait);
    }
    if state.stats_on {
        state.start_stats();
    }
    let mut frame: Box<dyn FnMut(&mut Loom)> = if opts.gpu {
        // the GPU, surfaceless: programs may hand GPU buffers (linux-dmabuf), as on a real screen
        let r = crate::gpu::headless_renderer()?;
        crate::gpu::offer(&mut state, &r);
        let shared = Rc::new(RefCell::new(r));
        state.gpu = Some(shared.clone());
        let target: GlesTexture = shared.borrow_mut().create_buffer(Fourcc::Abgr8888, (w, h).into())?;
        eprintln!("loom: headless on the GPU");
        Box::new(frames(shared, target, output.clone(), damage, fo))
    } else {
        let r = Rc::new(RefCell::new(PixmanRenderer::new()?));
        let target = Image::new(FormatCode::A8R8G8B8, w as usize, h as usize, true).map_err(|_| "pixman image")?;
        Box::new(frames(r, target, output.clone(), damage, fo))
    };

    // the script: one step every 25 ms once its time has come
    let mut script: std::collections::VecDeque<crate::input::Action> = opts.script.iter().cloned().collect();
    let script_at = opts.script_at;
    event_loop.handle().insert_source(Timer::from_duration(Duration::from_millis(25)), move |_, _, state: &mut Loom| {
        if state.start.elapsed() >= script_at {
            if let Some(a) = script.pop_front() {
                state.act(&a);
            }
        }
        // input a test sends over IPC (loomtest), after the script
        if script.is_empty() {
            if let Some(a) = state.test_input.pop_front() {
                state.act(&a);
            }
        }
        TimeoutAction::ToDuration(Duration::from_millis(25))
    })?;

    // what to run in it
    if !opts.run.is_empty() && !crate::restart::restarted() {
        let mut cmd = Command::new(&opts.run[0]);
        cmd.args(&opts.run[1..]).env("WAYLAND_DISPLAY", &socket_name).env_remove("DISPLAY");
        if let Some(h) = &state.hypr {
            cmd.env("HYPRLAND_INSTANCE_SIGNATURE", &h.signature);
        }
        if let Some(n) = state.x_display {
            cmd.env("DISPLAY", format!(":{n}"));
        }
        cmd.spawn()?;
    }

    crate::restart::restore_workspaces(&mut state);
    event_loop.run(None, &mut state, |state| {
        frame(state);
        state.after_dispatch();
    })?;
    crate::restart::after_loop(state, event_loop, socket_fd, &socket_name)
}

/// What the frame loop needs of the options.
struct FrameOpts {
    snapshot: Option<PathBuf>,
    wait: Duration,
    record: Option<PathBuf>,
    size: (i32, i32),
}

/// The frame loop for a renderer `R` drawing into `target` (pixman image or GPU texture).
fn frames<R, T>(renderer: Rc<RefCell<R>>, mut target: T, output: Output, mut damage: OutputDamageTracker, fo: FrameOpts) -> impl FnMut(&mut Loom)
where
    R: crate::render::LoomRenderer + Bind<T> + Bind<smithay::backend::allocator::dmabuf::Dmabuf> + Offscreen<T> + ExportMem + 'static,
    R::TextureId: Texture + Clone + 'static,
    T: 'static,
{
    move |state: &mut Loom| {
        let snap_due = fo.snapshot.is_some() && state.start.elapsed() >= fo.wait;
        if !state.frame_due() && !snap_due {
            // nothing to show, but a window copy may be due: only that is drawn
            if state.captures_ready() {
                crate::capture::run::<R, T>(state, &mut renderer.borrow_mut());
            }
            return;
        }
        state.last_frame = std::time::Instant::now();
        state.dirty = false;
        let mut r = renderer.borrow_mut();
        let t0 = std::time::Instant::now();
        let elements = crate::render::elements(state, &mut *r, &output, std::time::Instant::now());
        let want_png = snap_due || (fo.record.is_some() && state.frames < 400);
        let mut pixels = None;
        if let Ok(mut fb) = r.bind(&mut target) {
            let _ = damage.render_output(&mut *r, &mut fb, 0, &elements, [0.08, 0.08, 0.1, 1.0]);
            let us = t0.elapsed().as_micros() as u64;
            state.stats.render_us += us;
            state.stats.render_max_us = state.stats.render_max_us.max(us);
            state.shown += 1;
            state.stats.frames += 1;
            if want_png {
                pixels = read_rgba(&mut *r, &fb, fo.size);
            }
        }
        state.frames += 1;
        crate::capture::run::<R, T>(state, &mut r);
        drop(r);
        if let (Some(dir), Some(px)) = (&fo.record, &pixels) {
            let _ = save_png(px, fo.size, &dir.join(format!("frame-{:04}-{:06}ms.png", state.frames, state.start.elapsed().as_millis())));
        }
        let now = state.now_ms();
        state.frames_to_all(Duration::from_millis(now as u64));
        state.space.refresh();
        state.popups.cleanup();
        let _ = state.dh.flush_clients();
        if let (Some(path), true) = (&fo.snapshot, snap_due) {
            match pixels.as_ref().ok_or_else(|| "nothing read back".to_string()).and_then(|px| save_png(px, fo.size, path).map_err(|e| e.to_string())) {
                Ok(()) => eprintln!("loom: wrote {} ({} frames drawn in {:.1} s)", path.display(), state.frames, state.start.elapsed().as_secs_f64()),
                Err(e) => eprintln!("loom: {e}"),
            }
            state.signal.stop();
        }
    }
}

/// What the framebuffer shows, as RGBA bytes (R G B A in memory: Abgr8888).
fn read_rgba<R: ExportMem>(r: &mut R, fb: &R::Framebuffer<'_>, size: (i32, i32)) -> Option<Vec<u8>> {
    let mapping = r.copy_framebuffer(fb, smithay::utils::Rectangle::from_size(size.into()), Fourcc::Abgr8888).ok()?;
    r.map_texture(&mapping).ok().map(|b| b.to_vec())
}

/// RGBA bytes as a PNG (opaque).
fn save_png(px: &[u8], size: (i32, i32), path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut rgba = px.to_vec();
    for p in rgba.chunks_exact_mut(4) {
        p[3] = 255;
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, size.0 as u32, size.1 as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
