//! The headless backend: no screen, an output drawn into memory with pixman, 60 frames a second.
//! `--snapshot` writes what it shows as a PNG after `--wait` and quits (how every milestone is
//! tested without touching the running session).

use std::{path::PathBuf, process::Command, time::Duration};

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        pixman::PixmanRenderer,
        Bind,
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
    wayland::socket::ListeningSocketSource,
};

use crate::state::{client_data, Loom};

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
}

pub fn run(opts: Options) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<Loom> = EventLoop::try_new()?;
    let display: Display<Loom> = Display::new()?;
    let mut state = Loom::new(display.handle(), event_loop.get_signal());

    // clients connect here
    let socket = match &opts.socket {
        Some(name) => ListeningSocketSource::with_name(name)?,
        None => ListeningSocketSource::new_auto()?,
    };
    let socket_name = socket.socket_name().to_string_lossy().to_string();
    state.socket_name = Some(socket_name.clone());
    if let Some(p) = &opts.binds {
        state.binds = crate::binds::Binds::load(p);
        eprintln!("loom: {} binds from {}", state.binds.binds.len(), p.display());
    }
    state.hypr = Some(crate::hypr_ipc::start(&event_loop.handle())?);
    // X programs: Xwayland, started now (on demand later)
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
    eprintln!("loom: WAYLAND_DISPLAY={socket_name}");

    // the output
    let (w, h) = opts.size;
    let output = Output::new("HEADLESS-1".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "loom".into(), model: "headless".into() });
    let _global = output.create_global::<Loom>(&state.dh);
    let mode = Mode { size: (w, h).into(), refresh: 60_000 };
    output.change_current_state(Some(mode), Some(Transform::Normal), Some(Scale::Integer(1)), Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.output = Some(output.clone());

    let mut renderer = PixmanRenderer::new()?;
    let mut image = Image::new(FormatCode::A8R8G8B8, w as usize, h as usize, true).map_err(|_| "pixman image")?;
    let mut damage = OutputDamageTracker::from_output(&output);

    // a frame every 16 ms: draw, then tell the windows they were shown
    event_loop.handle().insert_source(Timer::from_duration(Duration::from_millis(16)), move |_, _, state: &mut Loom| {
        // only when something changed or an animation runs: an idle desktop draws nothing
        let snap_due = opts.snapshot.is_some() && state.start.elapsed() >= opts.wait;
        if state.dirty || state.animating() || snap_due {
            state.dirty = false;
            let elements = crate::render::elements(state, &mut renderer, &output, std::time::Instant::now());
            if let Ok(mut fb) = renderer.bind(&mut image) {
                let _ = damage.render_output(&mut renderer, &mut fb, 0, &elements, [0.08, 0.08, 0.1, 1.0]);
            }
            state.frames += 1;
            if let Some(dir) = &opts.record {
                if state.frames <= 400 {
                    let _ = save_png(&image, &dir.join(format!("frame-{:04}-{:06}ms.png", state.frames, state.start.elapsed().as_millis())));
                }
            }
            let now = state.now_ms();
            for w in state.space.elements() {
                w.send_frame(&output, Duration::from_millis(now as u64), Some(Duration::ZERO), |_, _| Some(output.clone()));
            }
            let map = smithay::desktop::layer_map_for_output(&output);
            for l in map.layers() {
                l.send_frame(&output, Duration::from_millis(now as u64), Some(Duration::ZERO), |_, _| Some(output.clone()));
            }
        }
        state.space.refresh();
        state.popups.cleanup();
        let _ = state.dh.flush_clients();
        if let (Some(path), true) = (&opts.snapshot, state.start.elapsed() >= opts.wait) {
            match save_png(&image, path) {
                Ok(()) => eprintln!("loom: wrote {} ({} frames drawn in {:.1} s)", path.display(), state.frames, state.start.elapsed().as_secs_f64()),
                Err(e) => eprintln!("loom: {e}"),
            }
            state.signal.stop();
        }
        TimeoutAction::ToDuration(Duration::from_millis(16))
    })?;

    // the script: one step every 25 ms once its time has come
    let mut script: std::collections::VecDeque<crate::input::Action> = opts.script.iter().cloned().collect();
    let script_at = opts.script_at;
    event_loop.handle().insert_source(Timer::from_duration(Duration::from_millis(25)), move |_, _, state: &mut Loom| {
        if state.start.elapsed() >= script_at {
            if let Some(a) = script.pop_front() {
                state.act(&a);
            }
        }
        if script.is_empty() && state.start.elapsed() >= script_at { TimeoutAction::Drop } else { TimeoutAction::ToDuration(Duration::from_millis(25)) }
    })?;

    // what to run in it
    if !opts.run.is_empty() {
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

    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

/// The pixman image (ARGB8888 in memory as B G R A) as an RGBA PNG.
fn save_png(image: &Image<'static, 'static>, path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h, stride) = (image.width(), image.height(), image.stride());
    let data: &[u8] = unsafe { std::slice::from_raw_parts(image.data() as *const u8, stride * h) };
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let p = &data[y * stride + x * 4..y * stride + x * 4 + 4];
            rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
