//! The pen (zwp_tablet_v2): libinput's tablet tool events to the program under the pen, with
//! pressure, tilt, distance, rotation and the pen's buttons. The pen draws on the built-in panel
//! (else the focused screen) and the cursor goes with it. A program that does not speak the
//! tablet protocol gets the pen as a mouse: the tip a left click.

use smithay::{
    backend::input::{ButtonState, Device, InputBackend, ProximityState, TabletToolAxisEvent, TabletToolButtonEvent, TabletToolEvent, TabletToolProximityEvent, TabletToolTipEvent, TabletToolTipState},
    delegate_tablet_manager,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::tablet_manager::{TabletDescriptor, TabletManagerState, TabletSeatHandler, TabletSeatTrait},
};

use crate::state::Loom;

pub fn init(dh: &smithay::reexports::wayland_server::DisplayHandle) {
    // lives as long as the display
    std::mem::forget(TabletManagerState::new::<Loom>(dh));
}

/// BTN_LEFT: the tip, for a program that only knows the mouse.
const BTN_LEFT: u32 = 0x110;

impl Loom {
    /// Where the pen draws: the built-in panel (eDP), else the focused screen.
    fn pen_area(&self) -> Rectangle<i32, Logical> {
        let o = self.outputs.iter().find(|o| o.name().starts_with("eDP")).cloned().or_else(|| self.output.clone());
        o.and_then(|o| self.space.output_geometry(&o)).unwrap_or_default()
    }

    /// The pen's place on its screen, from an event's position.
    fn pen_at<B: InputBackend, E: TabletToolEvent<B>>(&self, e: &E) -> Point<f64, Logical> {
        let a = self.pen_area();
        e.position_transformed(a.size) + a.loc.to_f64()
    }

    /// A tablet plugged in (or found at start): known to the programs.
    pub fn tablet_added<D: Device>(&mut self, device: &D) {
        self.seat.tablet_seat().add_tablet::<Loom>(&self.dh, &TabletDescriptor::from(device));
    }

    pub fn tablet_removed<D: Device>(&mut self, device: &D) {
        let seat = self.seat.tablet_seat();
        seat.remove_tablet(&TabletDescriptor::from(device));
        // no tablet left: its tools go too
        if seat.count_tablets() == 0 {
            seat.clear_tools();
        }
    }

    /// Whether the program owning `s` listens to the pen itself (else it gets mouse events).
    fn speaks_tablet(&self, s: &WlSurface) -> bool {
        s.client().is_some_and(|c| self.seat.tablet_seat().client_bound(&c.id()))
    }

    pub fn pen_axis<B: InputBackend, E: TabletToolAxisEvent<B>>(&mut self, e: E) {
        self.activity(false);
        let at = self.pen_at(&e);
        let time = self.now_ms();
        // the cursor goes with the pen (and a mouse-only program under it sees it move)
        self.pointer_move(at, time);
        let under = self.surface_at(at);
        let seat = self.seat.tablet_seat();
        let (Some(tablet), Some(tool)) = (seat.get_tablet(&TabletDescriptor::from(&e.device())), seat.get_tool(&e.tool())) else { return };
        if e.pressure_has_changed() {
            tool.pressure(e.pressure());
        }
        if e.distance_has_changed() {
            tool.distance(e.distance());
        }
        if e.tilt_has_changed() {
            tool.tilt(e.tilt());
        }
        if e.slider_has_changed() {
            tool.slider_position(e.slider_position());
        }
        if e.rotation_has_changed() {
            tool.rotation(e.rotation());
        }
        if e.wheel_has_changed() {
            tool.wheel(e.wheel_delta(), e.wheel_delta_discrete());
        }
        tool.motion(at, under, &tablet, SERIAL_COUNTER.next_serial(), time);
        self.dirty = true;
    }

    pub fn pen_proximity<B: InputBackend, E: TabletToolProximityEvent<B>>(&mut self, e: E) {
        let at = self.pen_at(&e);
        let time = self.now_ms();
        let seat = self.seat.tablet_seat();
        let tablet = seat.add_tablet::<Loom>(&self.dh, &TabletDescriptor::from(&e.device()));
        let tool = seat.add_tool::<Loom>(self, &self.dh.clone(), &e.tool());
        self.pointer_move(at, time);
        match e.state() {
            ProximityState::In => {
                if let Some(under) = self.surface_at(at) {
                    tool.proximity_in(at, under, &tablet, SERIAL_COUNTER.next_serial(), time);
                }
            }
            ProximityState::Out => tool.proximity_out(time),
        }
        self.dirty = true;
    }

    pub fn pen_tip<B: InputBackend, E: TabletToolTipEvent<B>>(&mut self, e: E) {
        self.activity(e.tip_state() == TabletToolTipState::Down);
        let at = self.pen_at(&e);
        let time = self.now_ms();
        let down = e.tip_state() == TabletToolTipState::Down;
        // the lift goes where the touch went: decided anew under the pen, a stroke begun in a
        // drawing program and ended over another window never ended there (it drew on)
        let tablet_prog = match (down, self.pen_down.take()) {
            (false, Some(was)) => was,
            _ => self.surface_at(at).is_some_and(|(s, _)| self.speaks_tablet(&s)),
        };
        if down {
            self.pen_down = Some(tablet_prog);
        }
        if !tablet_prog {
            // a mouse-only program (or none): the tip is the left button (focus, binds, all of it)
            self.pointer_button(BTN_LEFT, down, time);
            return;
        }
        let Some(tool) = self.seat.tablet_seat().get_tool(&e.tool()) else { return };
        if down {
            // the window or layer under the pen comes up and takes the keyboard, as a click does
            self.click_focus(at);
            tool.tip_down(SERIAL_COUNTER.next_serial(), time);
        } else {
            tool.tip_up(time);
        }
    }

    pub fn pen_button<B: InputBackend, E: TabletToolButtonEvent<B>>(&mut self, e: E) {
        let time = self.now_ms();
        let at = self.pointer_at;
        let tablet_prog = self.surface_at(at).is_some_and(|(s, _)| self.speaks_tablet(&s));
        if !tablet_prog {
            // the pen's side buttons as the mouse's right and middle ones
            let b = if e.button() == 0x14b { 0x112 } else { 0x111 };
            self.pointer_button(b, e.button_state() == ButtonState::Pressed, time);
            return;
        }
        if let Some(tool) = self.seat.tablet_seat().get_tool(&e.tool()) {
            tool.button(e.button(), e.button_state(), SERIAL_COUNTER.next_serial(), time);
        }
    }
}

impl TabletSeatHandler for Loom {}

delegate_tablet_manager!(Loom);
