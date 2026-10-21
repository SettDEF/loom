//! The compositor's state: Smithay's protocol states, the windows in a Space, the seat; and the
//! handlers each protocol calls into.

use std::{os::unix::io::OwnedFd, sync::Arc, time::Instant};

use smithay::{
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{layer_map_for_output, LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType},
    input::{Seat, SeatHandler, SeatState},
    output::Output,
    reexports::{
        calloop::LoopSignal,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
            Client, DisplayHandle,
        },
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{get_parent, is_sync_subsurface, with_states, CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
            SelectionHandler,
        },
        shell::wlr_layer::{Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState},
        shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
        shm::{ShmHandler, ShmState},
    },
};

use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;

pub struct Loom {
    pub dh: DisplayHandle,
    pub start: Instant,
    pub signal: LoopSignal,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Loom>,
    pub data_device_state: DataDeviceState,
    /// kept alive: it serves wl_output and xdg-output
    pub _output_manager_state: OutputManagerState,
    pub seat: Seat<Loom>,
    /// the windows and where they are
    pub space: Space<Window>,
    /// each window's animation (place, fade, scale), by its address (wm.rs)
    pub anims: std::collections::HashMap<u64, crate::anim::WindowAnim>,
    /// something changed since the last frame (a commit, a move): draw again
    pub dirty: bool,
    /// the nested backend's window and renderer (None headless)
    pub nested: Option<crate::nested::Nested>,
    /// where the pointer is (compositor space)
    pub pointer_at: smithay::utils::Point<f64, smithay::utils::Logical>,
    /// frames drawn so far (a test checks an idle desktop draws none)
    pub frames: u64,
    /// menus and other popups, with the window they belong to
    pub popups: PopupManager,
    /// the one output while there is one (headless, nested)
    pub output: Option<Output>,
    /// every window with its workspace and address (wm.rs); the workspace shown; the focused one
    pub managed: Vec<crate::wm::Managed>,
    pub next_addr: u64,
    pub active_ws: i32,
    pub focused: Option<Window>,
    /// Hyprland's IPC (hypr_ipc.rs): the event socket's listeners
    pub hypr: Option<crate::hypr_ipc::HyprIpc>,
    /// each workspace's AwesomeWM layout (layouts.rs)
    pub layouts: crate::layouts::Layouts,
    /// Hyprland's Wayland protocols' state (hypr_proto.rs)
    pub proto: crate::hypr_proto::HyprProto,
    /// the docks' window list (foreign.rs)
    pub foreign: crate::foreign::Foreign,
    /// the user's Hyprland binds (binds.rs); the actions a key just matched
    pub binds: crate::binds::Binds,
    pub bind_actions: Vec<String>,
    /// the fullscreen window, if one is
    pub fullscreen: Option<Window>,
    /// the repeating bind held (its key, its actions), its timer's generation; the event loop
    pub repeat: Option<(u32, Vec<String>)>,
    pub repeat_gen: u64,
    pub loop_handle: Option<smithay::reexports::calloop::LoopHandle<'static, Loom>>,
    /// the special workspace (Super+S) shown over the active one
    pub special_shown: bool,
    /// frame pacing (event driven, no ticking timer): the last frame drawn, a wake-up for the next one armed
    pub last_frame: Instant,
    pub frame_wake_armed: bool,
    /// the Wayland socket programs connect to (WAYLAND_DISPLAY for what loom starts)
    pub socket_name: Option<String>,
    /// Xwayland's window manager once it runs, its display (DISPLAY=:N for what loom starts)
    pub xwm: Option<smithay::xwayland::X11Wm>,
    pub x_display: Option<u32>,
    pub x: crate::xwayland::XState,
    pub xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState,
}

impl Loom {
    pub fn new(dh: DisplayHandle, signal: LoopSignal) -> Loom {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 600, 25).expect("keyboard");
        seat.add_pointer();
        crate::hypr_proto::init(&dh);
        crate::foreign::init(&dh);
        crate::hypr_proto::init_mapping(&dh);
        Loom {
            compositor_state: CompositorState::new::<Loom>(&dh),
            xdg_shell_state: XdgShellState::new::<Loom>(&dh),
            layer_shell_state: WlrLayerShellState::new::<Loom>(&dh),
            shm_state: ShmState::new::<Loom>(&dh, vec![]),
            data_device_state: DataDeviceState::new::<Loom>(&dh),
            _output_manager_state: OutputManagerState::new_with_xdg_output::<Loom>(&dh),
            seat_state,
            seat,
            space: Space::default(),
            popups: PopupManager::default(),
            anims: Default::default(),
            dirty: true,
            frames: 0,
            nested: None,
            pointer_at: (0.0, 0.0).into(),
            output: None,
            managed: Vec::new(),
            next_addr: 0,
            active_ws: 1,
            focused: None,
            hypr: None,
            layouts: crate::layouts::Layouts::load(),
            proto: Default::default(),
            foreign: Default::default(),
            binds: Default::default(),
            bind_actions: Vec::new(),
            fullscreen: None,
            repeat: None,
            repeat_gen: 0,
            loop_handle: None,
            special_shown: false,
            last_frame: Instant::now(),
            frame_wake_armed: false,
            socket_name: None,
            xwm: None,
            x_display: None,
            x: Default::default(),
            xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState::new::<Loom>(&dh),
            start: Instant::now(),
            signal,
            dh,
        }
    }

    /// The surface under a point, and where it is: the layers above windows, the windows, the
    /// layers below (a bar, a dock and the desktop take the pointer too).
    pub fn surface_at(&self, at: smithay::utils::Point<f64, smithay::utils::Logical>) -> Option<(WlSurface, smithay::utils::Point<f64, smithay::utils::Logical>)> {
        use smithay::wayland::shell::wlr_layer::Layer as L;
        let output = self.output.as_ref()?;
        let map = layer_map_for_output(output);
        let layer_hit = |which: &[L]| {
            which.iter().find_map(|l| {
                let layer = map.layer_under(*l, at)?;
                let geo = map.layer_geometry(layer)?;
                layer.surface_under(at - geo.loc.to_f64(), WindowSurfaceType::ALL).map(|(s, loc)| (s, (loc + geo.loc).to_f64()))
            })
        };
        layer_hit(&[L::Overlay, L::Top])
            .or_else(|| {
                self.space.element_under(at).and_then(|(w, loc)| w.surface_under(at - loc.to_f64(), WindowSurfaceType::ALL).map(|(s, sloc)| (s, (sloc + loc).to_f64())))
            })
            .or_else(|| layer_hit(&[L::Bottom, L::Background]))
    }

    /// A window's animation, if it has one.
    pub fn anim_of(&self, w: &Window) -> Option<&crate::anim::WindowAnim> {
        self.managed_of(w).and_then(|m| self.anims.get(&m.addr))
    }

    /// Any animation still running (the next frame must be drawn).
    pub fn animating(&self) -> bool {
        let now = std::time::Instant::now();
        self.anims.values().any(|a| !a.done(now))
    }

    /// This window has the keyboard and shows as active (xdg activated); the others not.
    pub fn focus_window(&mut self, window: &Window) {
        if self.focused.as_ref() != Some(window) {
            self.focused = Some(window.clone());
            let addr = self.managed_of(window).map(|m| m.addr).unwrap_or(0);
            self.hypr_event(&format!("activewindow>>{},{}", self.class_of(window), self.title_of(window)));
            self.hypr_event(&format!("activewindowv2>>{addr:x}"));
            self.foreign_all_changed();
        }
        self.space.raise_element(window, true);
        for w in self.space.elements() {
            let on = w == window;
            if let Some(x) = w.x11_surface() {
                let _ = x.set_activated(on);
                continue;
            }
            let Some(t) = w.toplevel() else { continue };
            t.with_pending_state(|s| {
                if on {
                    s.states.set(xdg_toplevel::State::Activated);
                } else {
                    s.states.unset(xdg_toplevel::State::Activated);
                }
            });
            // sent only when it differs from the last configure
            if t.is_initial_configure_sent() {
                t.send_pending_configure();
            }
        }
        let target = match (window.toplevel(), window.x11_surface()) {
            (Some(t), _) => Some(crate::focus::KbFocus::Surface(t.wl_surface().clone())),
            (_, Some(x)) => {
                if let Some(wm) = self.xwm.as_mut() {
                    let _ = wm.raise_window(x);
                }
                Some(crate::focus::KbFocus::X11(x.clone()))
            }
            _ => None,
        };
        if let Some(t) = target {
            let kb = self.seat.get_keyboard().expect("keyboard");
            kb.set_focus(self, Some(t), Serial::from(0));
        }
    }

    /// The milliseconds since the start, for frame callbacks.
    pub fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }
}

impl CompositorHandler for Loom {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(x) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &x.compositor_state;
        }
        &client.get_data::<ClientState>().expect("client state").compositor_state
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.dirty = true;
        self.popups.commit(surface);
        // a layer surface (bar, dock, panel): arranged on its output, its first configure sent
        if let Some(output) = self.output.clone() {
            let mut map = layer_map_for_output(&output);
            if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned() {
                let sent = with_states(surface, |s| s.data_map.get::<LayerSurfaceData>().map(|d| d.lock().unwrap().initial_configure_sent).unwrap_or(true));
                let moved = map.arrange();
                if !sent {
                    layer.layer_surface().send_configure();
                }
                drop(map);
                if moved {
                    self.relayout();
                }
                return;
            }
        }
        // a popup's first commit: its place (from its positioner)
        if let Some(PopupKind::Xdg(p)) = self.popups.find_popup(surface) {
            if !p.is_initial_configure_sent() {
                let _ = p.send_configure();
            }
        }
        if is_sync_subsurface(surface) {
            return;
        }
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if let Some(window) = self.space.elements().find(|w| smithay::wayland::seat::WaylandFocus::wl_surface(*w).is_some_and(|s| *s == root)).cloned() {
            window.on_commit();
            // its first frame with something to show: the opening animation starts now
            let has_buffer = smithay::backend::renderer::utils::with_renderer_surface_state(&root, |s| s.buffer().is_some()).unwrap_or(false);
            if has_buffer {
                if let Some(a) = self.managed_of(&window).map(|m| m.addr).and_then(|addr| self.anims.get_mut(&addr)) {
                    a.shown(std::time::Instant::now());
                }
            }
            // the first commit: the window learns its size
            if let Some(t) = window.toplevel() {
                if !t.is_initial_configure_sent() {
                    t.send_configure();
                }
            }
        }
    }
}

impl XdgShellHandler for Loom {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    fn title_changed(&mut self, surface: ToplevelSurface) {
        let Some(w) = self.managed.iter().find(|m| m.window.toplevel() == Some(&surface)).map(|m| m.window.clone()) else { return };
        self.title_changed_of(&w);
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.manage(Window::new_wayland_window(surface));
    }
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let gone = self.managed.iter().find(|m| m.window.toplevel() == Some(&surface)).map(|m| m.window.clone());
        if let Some(w) = gone {
            self.unmanage(&w);
        }
    }
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|s| s.geometry = positioner.get_geometry());
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }
    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        surface.send_repositioned(token);
    }
}

impl WlrLayerShellHandler for Loom {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }
    fn new_layer_surface(&mut self, surface: WlrLayerSurface, _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>, _layer: Layer, namespace: String) {
        // one output so far: every layer goes on it
        let Some(output) = self.output.clone() else { return };
        let mut map = layer_map_for_output(&output);
        let _ = map.map_layer(&LayerSurface::new(surface, namespace));
    }
    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let Some(output) = self.output.clone() else { return };
        let mut map = layer_map_for_output(&output);
        let gone = map.layers().find(|l| l.layer_surface() == &surface).cloned();
        if let Some(l) = gone {
            map.unmap_layer(&l);
        }
        drop(map);
        self.relayout();
    }
}

impl BufferHandler for Loom {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Loom {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for Loom {
    type KeyboardFocus = crate::focus::KbFocus;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
}

impl SelectionHandler for Loom {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Loom {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
impl ClientDndGrabHandler for Loom {}
impl ServerDndGrabHandler for Loom {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl OutputHandler for Loom {}

/// What loom keeps per client.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

pub fn client_data() -> Arc<ClientState> {
    Arc::new(ClientState::default())
}

delegate_compositor!(Loom);
delegate_xdg_shell!(Loom);
delegate_shm!(Loom);
delegate_seat!(Loom);
delegate_data_device!(Loom);
delegate_output!(Loom);
delegate_layer_shell!(Loom);

/// The time between frames (60 Hz; the real screen's refresh with M4).
pub const FRAME: std::time::Duration = std::time::Duration::from_micros(16_667);

impl Loom {
    /// Something to draw, and the last frame is a frame's time ago.
    pub fn frame_due(&self) -> bool {
        (self.dirty || self.animating()) && self.last_frame.elapsed() >= FRAME
    }

    /// The loop wakes in `d` (it sleeps otherwise until a client, input or a timer wakes it).
    pub fn wake_in(&mut self, d: std::time::Duration) {
        if let Some(h) = &self.loop_handle {
            let _ = h.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(d), |_, _, _| smithay::reexports::calloop::timer::TimeoutAction::Drop);
        }
    }

    /// After each turn of the event loop: a frame waiting for its time gets one wake-up then
    /// (never a ticking timer); what the clients were sent goes out.
    pub fn after_dispatch(&mut self) {
        if (self.dirty || self.animating()) && !self.frame_wake_armed {
            let left = FRAME.saturating_sub(self.last_frame.elapsed());
            self.frame_wake_armed = true;
            if let Some(h) = &self.loop_handle {
                let _ = h.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(left), |_, _, s: &mut Loom| {
                    s.frame_wake_armed = false;
                    smithay::reexports::calloop::timer::TimeoutAction::Drop
                });
            }
        }
        let _ = self.dh.flush_clients();
    }
}
