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
    utils::{Rectangle, Serial},
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
use smithay::reexports::wayland_server::Resource;

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
    /// each window's animation (place, fade, scale), by its surface
    pub anims: std::collections::HashMap<smithay::reexports::wayland_server::backend::ObjectId, crate::anim::WindowAnim>,
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
}

impl Loom {
    pub fn new(dh: DisplayHandle, signal: LoopSignal) -> Loom {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 600, 25).expect("keyboard");
        seat.add_pointer();
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
            start: Instant::now(),
            signal,
            dh,
        }
    }

    /// The windows side by side across the output (milestone 1 brings dwindle tiling).
    pub fn relayout(&mut self) {
        let Some(output) = self.output.clone() else { return };
        let Some(geo) = self.space.output_geometry(&output) else { return };
        // what the layers (bar, dock) leave free
        let mut area = layer_map_for_output(&output).non_exclusive_zone();
        area.loc += geo.loc;
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        let n = windows.len().max(1) as i32;
        let gap = 8;
        let w = (area.size.w - gap * (n + 1)) / n;
        for (i, win) in windows.into_iter().enumerate() {
            let x = area.loc.x + gap + i as i32 * (w + gap);
            let rect = Rectangle::new((x, area.loc.y + gap).into(), (w, area.size.h - 2 * gap).into());
            if let Some(t) = win.toplevel() {
                t.with_pending_state(|s| s.size = Some(rect.size));
                if t.is_initial_configure_sent() {
                    t.send_pending_configure();
                }
            }
            // the window glides there (a new one fades and grows in where it lands)
            if let Some(id) = win.toplevel().map(|t| t.wl_surface().id()) {
                let now = std::time::Instant::now();
                match self.anims.get_mut(&id) {
                    Some(a) => a.moving_to(rect.loc.x as f64, rect.loc.y as f64, now),
                    None => {
                        self.anims.insert(id, crate::anim::WindowAnim::opening(rect.loc.x as f64, rect.loc.y as f64));
                    }
                }
            }
            self.space.map_element(win, rect.loc, false);
        }
        self.dirty = true;
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
        w.toplevel().and_then(|t| self.anims.get(&t.wl_surface().id()))
    }

    /// Any animation still running (the next frame must be drawn).
    pub fn animating(&self) -> bool {
        let now = std::time::Instant::now();
        self.anims.values().any(|a| !a.done(now))
    }

    /// This window has the keyboard and shows as active (xdg activated); the others not.
    pub fn focus_window(&mut self, window: &Window) {
        self.space.raise_element(window, true);
        for w in self.space.elements() {
            let Some(t) = w.toplevel() else { continue };
            let on = w == window;
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
        if let Some(t) = window.toplevel() {
            let kb = self.seat.get_keyboard().expect("keyboard");
            kb.set_focus(self, Some(t.wl_surface().clone()), Serial::from(0));
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
        if let Some(window) = self.space.elements().find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root)).cloned() {
            window.on_commit();
            // its first frame with something to show: the opening animation starts now
            let has_buffer = smithay::backend::renderer::utils::with_renderer_surface_state(&root, |s| s.buffer().is_some()).unwrap_or(false);
            if has_buffer {
                if let Some(a) = self.anims.get_mut(&root.id()) {
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
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface.clone());
        self.space.map_element(window.clone(), (0, 0), true);
        self.relayout();
        // the newest window has the keyboard
        let _ = surface;
        self.focus_window(&window);
    }
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let gone = self.space.elements().find(|w| w.toplevel() == Some(&surface)).cloned();
        if let Some(w) = gone {
            self.space.unmap_elem(&w);
        }
        self.anims.remove(&surface.wl_surface().id());
        self.relayout();
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
    type KeyboardFocus = WlSurface;
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
