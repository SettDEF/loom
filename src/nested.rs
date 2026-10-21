//! The nested backend: loom in a window of the desktop it runs in (winit, GL), with that
//! desktop's keyboard and mouse. For trying loom live without leaving the session; closing the
//! window ends it.

use std::{process::Command, time::Duration};

use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        input::{AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputEvent, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent},
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer, ImportDma},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    delegate_dmabuf,
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, PostAction},
        wayland_server::Display,
    },
    utils::{Transform, SERIAL_COUNTER},
    wayland::{
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        socket::ListeningSocketSource,
    },
};

use crate::state::{client_data, Loom};

/// The GL window and its renderer, while loom runs nested.
pub struct Nested {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub dmabuf_state: DmabufState,
    pub _dmabuf_global: DmabufGlobal,
}

impl DmabufHandler for Loom {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.nested.as_mut().expect("nested").dmabuf_state
    }
    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        // a GPU buffer the client made: the renderer takes it, or the client hears it failed
        let ok = self.nested.as_mut().is_some_and(|n| n.backend.renderer().import_dmabuf(&dmabuf, None).is_ok());
        if ok {
            let _ = notifier.successful::<Loom>();
        } else {
            notifier.failed();
        }
    }
}
delegate_dmabuf!(Loom);

pub fn run(socket: Option<String>, run: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<Loom> = EventLoop::try_new()?;
    let display: Display<Loom> = Display::new()?;
    let mut state = Loom::new(display.handle(), event_loop.get_signal());

    // the window on the desktop it runs in: named loom (Hyprland's rules and title bars see that)
    let attrs = smithay::reexports::winit::window::WindowAttributes::default().with_title("loom").with_inner_size(smithay::reexports::winit::dpi::LogicalSize::new(1280.0, 800.0));
    let (mut backend, winit_loop) = winit::init_from_attributes::<GlesRenderer>(attrs)?;
    let formats = backend.renderer().dmabuf_formats();
    let mut dmabuf_state = DmabufState::new();
    let dmabuf_global = dmabuf_state.create_global::<Loom>(&state.dh, formats);

    let size = backend.window_size();
    let output = Output::new("NESTED-1".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "loom".into(), model: "nested".into() });
    let _global = output.create_global::<Loom>(&state.dh);
    let mode = Mode { size, refresh: 60_000 };
    output.change_current_state(Some(mode), Some(Transform::Flipped180), Some(Scale::Integer(1)), Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.output = Some(output.clone());
    state.nested = Some(Nested { backend, dmabuf_state, _dmabuf_global: dmabuf_global });

    let socket = match &socket {
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

    // the frame: when something changed or an animation runs
    let out3 = output.clone();
    event_loop.handle().insert_source(smithay::reexports::calloop::timer::Timer::from_duration(Duration::from_millis(8)), move |_, _, state: &mut Loom| {
        if state.dirty || state.animating() {
            state.dirty = false;
            let now = std::time::Instant::now();
            let mut nested = state.nested.take().expect("nested");
            let size = nested.backend.window_size();
            let mut drawn = false;
            if let Ok((renderer, mut fb)) = nested.backend.bind() {
                let elements = crate::render::elements(state, renderer, &out3, now);
                if let Ok(r) = damage.render_output(renderer, &mut fb, 0, &elements, [0.08, 0.08, 0.1, 1.0]) {
                    drawn = r.damage.is_some();
                }
            }
            if drawn {
                let _ = nested.backend.submit(Some(&[smithay::utils::Rectangle::from_size(size)]));
            }
            state.nested = Some(nested);
            let ms = state.now_ms();
            for w in state.space.elements() {
                w.send_frame(&out3, Duration::from_millis(ms as u64), Some(Duration::ZERO), |_, _| Some(out3.clone()));
            }
            for l in smithay::desktop::layer_map_for_output(&out3).layers() {
                l.send_frame(&out3, Duration::from_millis(ms as u64), Some(Duration::ZERO), |_, _| Some(out3.clone()));
            }
            state.space.refresh();
            state.popups.cleanup();
        }
        let _ = state.dh.flush_clients();
        smithay::reexports::calloop::timer::TimeoutAction::ToDuration(Duration::from_millis(8))
    })?;

    if !run.is_empty() {
        let mut cmd = Command::new(&run[0]);
        cmd.args(&run[1..]).env("WAYLAND_DISPLAY", &socket_name).env_remove("DISPLAY").env_remove("HYPRLAND_INSTANCE_SIGNATURE");
        cmd.spawn()?;
    }
    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

impl Loom {
    /// The real keyboard and mouse, from the window loom runs in.
    fn nested_input(&mut self, ev: InputEvent<smithay::backend::winit::WinitInput>) {
        let time = self.now_ms();
        match ev {
            InputEvent::Keyboard { event } => {
                let kb = self.seat.get_keyboard().expect("keyboard");
                kb.input::<(), _>(self, event.key_code(), event.state(), SERIAL_COUNTER.next_serial(), time, |_, _, _| FilterResult::Forward);
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output) = self.output.clone() else { return };
                let size = output.current_mode().map(|m| m.size.to_logical(1)).unwrap_or_default();
                let at = event.position_transformed(size);
                self.pointer_at = at;
                let focus = self.surface_at(at);
                let pointer = self.seat.get_pointer().expect("pointer");
                pointer.motion(self, focus, &MotionEvent { location: at, serial: SERIAL_COUNTER.next_serial(), time });
                pointer.frame(self);
            }
            InputEvent::PointerButton { event } => {
                let pointer = self.seat.get_pointer().expect("pointer");
                if event.state() == ButtonState::Pressed {
                    // a press on a window: it comes up and has the keyboard
                    if let Some((w, _)) = self.space.element_under(self.pointer_at).map(|(w, l)| (w.clone(), l)) {
                        self.focus_window(&w);
                    }
                }
                pointer.button(self, &ButtonEvent { button: event.button_code(), state: event.state(), serial: SERIAL_COUNTER.next_serial(), time });
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                let mut frame = AxisFrame::new(event.time_msec()).source(AxisSource::Wheel);
                for axis in [Axis::Vertical, Axis::Horizontal] {
                    if let Some(v) = event.amount(axis).or_else(|| event.amount_v120(axis).map(|v| v * 15.0 / 120.0)) {
                        frame = frame.value(axis, v);
                    }
                }
                let pointer = self.seat.get_pointer().expect("pointer");
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }
}
