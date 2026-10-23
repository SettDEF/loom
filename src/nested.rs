//! The nested backend: loom in a window of the desktop it runs in (winit, GL), with that
//! desktop's keyboard and mouse. For trying loom live without leaving the session; closing the
//! window ends it.

use std::{process::Command, time::Duration};

use smithay::{
    backend::{
        input::{AbsolutePositionEvent, Axis, ButtonState, InputEvent, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent},
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, PostAction},
        wayland_server::Display,
    },
    utils::Transform,
    wayland::{

        socket::ListeningSocketSource,
    },
};

use crate::state::{client_data, Loom};

/// The GL window and its renderer, while loom runs nested.
pub struct Nested {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
}

pub fn run(socket: Option<String>, run: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<Loom> = EventLoop::try_new()?;
    let display: Display<Loom> = Display::new()?;
    let mut state = Loom::new(display.handle(), event_loop.get_signal());

    // the window on the desktop it runs in: named loom (Hyprland's rules and title bars see that)
    let attrs = smithay::reexports::winit::window::WindowAttributes::default().with_title("loom").with_inner_size(smithay::reexports::winit::dpi::LogicalSize::new(1280.0, 800.0));
    let (mut backend, winit_loop) = winit::init_from_attributes::<GlesRenderer>(attrs)?;
    // GPU buffers from programs, offered with feedback (gpu.rs)
    crate::gpu::offer(&mut state, backend.renderer());

    let size = backend.window_size();
    let output = Output::new("NESTED-1".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "loom".into(), model: "nested".into() });
    let _global = output.create_global::<Loom>(&state.dh);
    let mode = Mode { size, refresh: 60_000 };
    output.change_current_state(Some(mode), Some(Transform::Flipped180), Some(Scale::Integer(1)), Some((0, 0).into()));
    output.set_preferred(mode);
    state.add_output(output.clone(), (0, 0).into());
    state.nested = Some(Nested { backend });

    let socket = match &socket {
        Some(name) => ListeningSocketSource::with_name(name)?,
        None => ListeningSocketSource::new_auto()?,
    };
    let socket_name = socket.socket_name().to_string_lossy().to_string();
    state.socket_name = Some(socket_name.clone());
    state.loop_handle = Some(event_loop.handle());
    state.init_idle();
    // init.lua applied when it changes
    state.watch_config();
    // the user's Hyprland binds (LOOM_HYPR_CONF, else ~/.config/hypr/hyprland.conf)
    let conf = std::env::var("LOOM_HYPR_CONF").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/hypr/hyprland.conf"));
    state.binds = crate::binds::Binds::load(&conf);
    eprintln!("loom: {} binds from {} ({} not handled yet: mouse)", state.binds.binds.len(), conf.display(), state.binds.skipped);
    state.hypr = Some(crate::hypr_ipc::start(&event_loop.handle())?);
    // X programs: the display reserved now, Xwayland started by the first X program
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
    eprintln!("loom: nested, WAYLAND_DISPLAY={socket_name}");

    let mut damage = OutputDamageTracker::from_output(&output);
    let out2 = output.clone();
    event_loop.handle().insert_source(winit_loop, move |event, _, state: &mut Loom| match event {
        WinitEvent::Resized { size, .. } => {
            let mode = Mode { size, refresh: 60_000 };
            out2.change_current_state(Some(mode), None, None, None);
            out2.set_preferred(mode);
            smithay::desktop::layer_map_for_output(&out2).arrange();
            state.relayout();
        }
        WinitEvent::CloseRequested => state.signal.stop(),
        WinitEvent::Input(ev) => state.nested_input(ev),
        WinitEvent::Redraw | WinitEvent::Focus(_) => state.dirty = true,
    })?;

    // a frame when something changed or an animation runs (at most one per frame time); idle,
    // loom sleeps: no ticking timer (battery)
    let out3 = output.clone();
    let mut frame = move |state: &mut Loom| {
        if state.frame_due() {
            state.last_frame = std::time::Instant::now();
            state.dirty = false;
            let now = std::time::Instant::now();
            let mut nested = state.nested.take().expect("nested");
            let size = nested.backend.window_size();
            let _ = size;
            // only what changed goes to the screen (the damage), not the whole window
            let mut changed: Option<Vec<smithay::utils::Rectangle<i32, smithay::utils::Physical>>> = None;
            if let Ok((renderer, mut fb)) = nested.backend.bind() {
                let elements = crate::render::elements(state, renderer, &out3, now);
                if let Ok(r) = damage.render_output(renderer, &mut fb, 0, &elements, [0.08, 0.08, 0.1, 1.0]) {
                    changed = r.damage.cloned();
                    if changed.as_ref().is_some_and(|d| !d.is_empty()) {
                        state.shown += 1;
                    }
                }
            }
            // window copies for the shell (previews), drawn offscreen with the same renderer
            crate::capture::run::<_, smithay::backend::renderer::gles::GlesTexture>(state, nested.backend.renderer());
            if let Some(d) = changed.filter(|d| !d.is_empty()) {
                let _ = nested.backend.submit(Some(&d));
            }
            state.nested = Some(nested);
            let ms = state.now_ms();
            for w in state.space.elements() {
                if !state.wants_frame(w) {
                    continue;
                }
                w.send_frame(&out3, Duration::from_millis(ms as u64), Some(Duration::ZERO), |_, _| Some(out3.clone()));
            }
            for l in smithay::desktop::layer_map_for_output(&out3).layers() {
                l.send_frame(&out3, Duration::from_millis(ms as u64), Some(Duration::ZERO), |_, _| Some(out3.clone()));
            }
            state.lock_frame(&out3, Duration::from_millis(ms as u64));
            state.space.refresh();
            state.popups.cleanup();
        } else if state.captures_ready() {
            // nothing to show, but a window copy is due: only that is drawn
            let mut nested = state.nested.take().expect("nested");
            crate::capture::run::<_, smithay::backend::renderer::gles::GlesTexture>(state, nested.backend.renderer());
            state.nested = Some(nested);
        }
    };

    if !run.is_empty() {
        let mut cmd = Command::new(&run[0]);
        cmd.args(&run[1..]).env("WAYLAND_DISPLAY", &socket_name).env_remove("DISPLAY");
        if let Some(h) = &state.hypr {
            cmd.env("HYPRLAND_INSTANCE_SIGNATURE", &h.signature);
        }
        if let Some(n) = state.x_display {
            cmd.env("DISPLAY", format!(":{n}"));
        }
        cmd.spawn()?;
    }
    event_loop.run(None, &mut state, |state| {
        frame(state);
        state.after_dispatch();
    })?;
    Ok(())
}

impl Loom {
    /// The real keyboard and mouse, from the window loom runs in.
    fn nested_input(&mut self, ev: InputEvent<smithay::backend::winit::WinitInput>) {
        let time = self.now_ms();
        match ev {
            InputEvent::Keyboard { event } => {
                self.key_input(event.key_code(), event.state(), time);
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output) = self.output.clone() else { return };
                let size = output.current_mode().map(|m| m.size.to_logical(1)).unwrap_or_default();
                let at = event.position_transformed(size);
                self.pointer_move(at, time);
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
