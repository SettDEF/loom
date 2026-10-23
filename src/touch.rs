//! The touchscreen (wl_touch): libinput's touches to the surface under each finger, many at once.
//! A touch maps to the built-in panel (input:touchdevice:output, eDP), else the focused screen;
//! a finger going down focuses what it lands on, as a click does.

use smithay::{
    backend::input::TouchSlot,
    input::touch::{DownEvent, MotionEvent, UpEvent},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
};

use crate::state::Loom;

/// The surface each finger went down on, and where that surface is.
pub type Slots = Vec<(TouchSlot, Option<(WlSurface, Point<f64, Logical>)>)>;

impl Loom {
    /// Where touches land: the built-in panel, else the focused screen.
    pub fn touch_area(&self) -> Rectangle<i32, Logical> {
        let o = self.outputs.iter().find(|o| o.name().starts_with("eDP")).cloned().or_else(|| self.output.clone());
        o.and_then(|o| self.space.output_geometry(&o)).unwrap_or_default()
    }

    pub fn touch_down(&mut self, slot: TouchSlot, at: Point<f64, Logical>, time: u32) {
        self.activity(true);
        self.click_focus(at);
        let focus = self.surface_at(at);
        self.touch_slots.retain(|(s, _)| *s != slot);
        self.touch_slots.push((slot, focus.clone()));
        let Some(touch) = self.seat.get_touch() else { return };
        touch.down(self, focus, &DownEvent { slot, location: at, serial: SERIAL_COUNTER.next_serial(), time });
        self.dirty = true;
    }

    pub fn touch_motion(&mut self, slot: TouchSlot, at: Point<f64, Logical>, time: u32) {
        self.activity(false);
        // the finger stays with the surface it went down on, as a dragged pointer does
        let focus = self.touch_slots.iter().find(|(s, _)| *s == slot).and_then(|(_, f)| f.clone());
        let Some(touch) = self.seat.get_touch() else { return };
        touch.motion(self, focus, &MotionEvent { slot, location: at, time });
    }

    pub fn touch_up(&mut self, slot: TouchSlot, time: u32) {
        self.touch_slots.retain(|(s, _)| *s != slot);
        let Some(touch) = self.seat.get_touch() else { return };
        touch.up(self, &UpEvent { slot, serial: SERIAL_COUNTER.next_serial(), time });
    }

    pub fn touch_frame(&mut self) {
        if let Some(touch) = self.seat.get_touch() {
            touch.frame(self);
        }
    }

    pub fn touch_cancel(&mut self) {
        self.touch_slots.clear();
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
        }
    }
}
