//! The headless backend: no screen, an output drawn into memory with pixman, 60 frames a second.
//! `--snapshot` writes what it shows as a PNG after `--wait` and quits (how every milestone is
//! tested without touching the running session).

use std::{path::PathBuf, process::Command, time::Duration};

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        element::surface::WaylandSurfaceRenderElement,
        pixman::PixmanRenderer,
        Bind,
    },
    desktop::space::render_output,
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
        if let Ok(mut fb) = renderer.bind(&mut image) {
            let _ = render_output::<_, WaylandSurfaceRenderElement<PixmanRenderer>, _, _>(&output, &mut renderer, &mut fb, 1.0, 0, [&state.space], &[], &mut damage, [0.08, 0.08, 0.1, 1.0]);
        }
        let now = state.now_ms();
        for w in state.space.elements() {
            w.send_frame(&output, Duration::from_millis(now as u64), Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        state.space.refresh();
        let _ = state.dh.flush_clients();
        if let (Some(path), true) = (&opts.snapshot, state.start.elapsed() >= opts.wait) {
            match save_png(&image, path) {
                Ok(()) => eprintln!("loom: wrote {}", path.display()),
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
        cmd.args(&opts.run[1..]).env("WAYLAND_DISPLAY", &socket_name).env_remove("DISPLAY").env_remove("HYPRLAND_INSTANCE_SIGNATURE");
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
