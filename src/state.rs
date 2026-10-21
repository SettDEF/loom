//! The compositor's state: Smithay's protocol states, the windows in a Space, the seat; and the
//! handlers each protocol calls into.

use std::{os::unix::io::OwnedFd, sync::Arc, time::Instant};

use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{Space, Window},
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
        compositor::{get_parent, is_sync_subsurface, CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
            SelectionHandler,
        },
        shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
        shm::{ShmHandler, ShmState},
    },
};

use smithay::backend::renderer::utils::on_commit_buffer_handler;

pub struct Loom {
    pub dh: DisplayHandle,
    pub start: Instant,
    pub signal: LoopSignal,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Loom>,
    pub data_device_state: DataDeviceState,
    pub output_manager_state: OutputManagerState,
    pub seat: Seat<Loom>,
    /// the windows and where they are
    pub space: Space<Window>,
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
            shm_state: ShmState::new::<Loom>(&dh, vec![]),
            data_device_state: DataDeviceState::new::<Loom>(&dh),
            output_manager_state: OutputManagerState::new_with_xdg_output::<Loom>(&dh),
            seat_state,
            seat,
            space: Space::default(),
            output: None,
            start: Instant::now(),
            signal,
            dh,
        }
    }

    /// The windows side by side across the output (milestone 1 brings dwindle tiling).
    pub fn relayout(&mut self) {
        let Some(area) = self.output.as_ref().and_then(|o| self.space.output_geometry(o)) else { return };
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
            self.space.map_element(win, rect.loc, false);
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
        if is_sync_subsurface(surface) {
            return;
        }
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if let Some(window) = self.space.elements().find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root)).cloned() {
            window.on_commit();
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
        self.space.map_element(window, (0, 0), true);
        self.relayout();
        // the newest window has the keyboard
        let kb = self.seat.get_keyboard().expect("keyboard");
        kb.set_focus(self, Some(surface.wl_surface().clone()), Serial::from(0));
    }
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let gone = self.space.elements().find(|w| w.toplevel() == Some(&surface)).cloned();
        if let Some(w) = gone {
            self.space.unmap_elem(&w);
        }
        self.relayout();
    }
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}
    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
    fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {}
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
