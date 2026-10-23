//! The lock screen (ext-session-lock, what the shell's lock uses): while locked only the locker's
//! surface is drawn and gets the keyboard and pointer; no bind runs and loom cannot be quit with
//! Ctrl+Alt+Backspace. A locker that dies leaves the session locked (dark) until a new one locks
//! and unlocks it.

use std::time::Duration;

use smithay::{
    delegate_session_lock,
    desktop::{utils::under_from_surface_tree, WindowSurfaceType},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Point},
    wayland::{
        compositor::{with_surface_tree_downward, SurfaceAttributes, TraversalAction},
        session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
    },
};

use crate::state::Loom;

#[derive(Default)]
pub enum Lock {
    #[default]
    Open,
    /// the locker's surfaces, one per screen (by screen name)
    Locked(Vec<(String, LockSurface)>),
}

pub fn init(dh: &smithay::reexports::wayland_server::DisplayHandle) -> SessionLockManagerState {
    SessionLockManagerState::new::<Loom, _>(dh, |_| true)
}

impl Loom {
    pub fn locked(&self) -> bool {
        matches!(self.lock, Lock::Locked(_))
    }

    /// The lock surface on screen `name`, while locked and the locker made one there.
    pub fn lock_surface_on(&self, name: &str) -> Option<&WlSurface> {
        match &self.lock {
            Lock::Locked(v) => v.iter().find(|(n, s)| n == name && s.alive()).map(|(_, s)| s.wl_surface()),
            _ => None,
        }
    }

    /// A live lock surface: the focused screen's, else any (the locker is alive).
    pub fn lock_surface(&self) -> Option<&WlSurface> {
        let focused = self.output.as_ref().map(|o| o.name()).unwrap_or_default();
        self.lock_surface_on(&focused).or_else(|| match &self.lock {
            Lock::Locked(v) => v.iter().find(|(_, s)| s.alive()).map(|(_, s)| s.wl_surface()),
            _ => None,
        })
    }

    /// The lock surface under `at` (on the screen there), and where it is.
    pub fn lock_surface_at(&self, at: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let o = self.output_at(at).or_else(|| self.output.clone())?;
        let off = self.space.output_geometry(&o)?.loc;
        let s = self.lock_surface_on(&o.name())?;
        under_from_surface_tree(s, at, off, WindowSurfaceType::ALL).map(|(s, loc)| (s, loc.to_f64()))
    }

    /// Their frame callbacks (the backends send the rest).
    pub fn lock_frame(&self, _output: &Output, ms: Duration) {
        let Lock::Locked(v) = &self.lock else { return };
        for (_, s) in v.iter().filter(|(_, s)| s.alive()) {
            with_surface_tree_downward(
                s.wl_surface(),
                (),
                |_, _, _| TraversalAction::DoChildren(()),
                |_, states, _| {
                    for cb in states.cached_state.get::<SurfaceAttributes>().current().frame_callbacks.drain(..) {
                        cb.done(ms.as_millis() as u32);
                    }
                },
                |_, _, _| true,
            );
        }
    }

    /// Sized to its screen (logical).
    fn lock_configure(&self, surface: &LockSurface, o: &Output) {
        let Some(size) = o.current_mode().map(|mode| o.current_transform().transform_size(mode.size).to_f64().to_logical(o.current_scale().fractional_scale()).to_i32_round::<i32>()) else {
            return;
        };
        surface.with_pending_state(|s| s.size = Some((size.w as u32, size.h as u32).into()));
        surface.send_configure();
    }
}

impl SessionLockHandler for Loom {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.lock_manager
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // locked by a live locker already: this one is refused (dropping it says so)
        if self.lock_surface().is_some() {
            return;
        }
        eprintln!("loom: locked ({} lock listeners)", self.proto.lock_notes.len());
        self.lock = Lock::Locked(Vec::new());
        // nothing of the session keeps the keyboard or the pointer
        let kb = self.seat.get_keyboard().expect("keyboard");
        kb.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial());
        let pointer = self.seat.get_pointer().expect("pointer");
        pointer.motion(self, None, &smithay::input::pointer::MotionEvent { location: self.pointer_at, serial: smithay::utils::SERIAL_COUNTER.next_serial(), time: self.now_ms() });
        pointer.frame(self);
        self.dirty = true;
        confirmation.lock();
        for n in &self.proto.lock_notes {
            n.locked();
        }
    }

    fn unlock(&mut self) {
        eprintln!("loom: unlocked");
        self.lock = Lock::Open;
        for n in &self.proto.lock_notes {
            n.unlocked();
        }
        self.dirty = true;
        // the keyboard back to the window that had it, the pointer to what is under it
        if let Some(w) = self.focused.clone() {
            self.focus_window(&w);
        }
        let at = self.pointer_at;
        self.pointer_move(at, self.now_ms());
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        if !self.locked() {
            return;
        }
        let Some(o) = Output::from_resource(&wl_output).or_else(|| self.output.clone()) else { return };
        self.lock_configure(&surface, &o);
        // the keyboard: the focused screen's lock surface (or the first one made)
        let first = self.lock_surface().is_none();
        if first || self.output.as_ref() == Some(&o) {
            let kb = self.seat.get_keyboard().expect("keyboard");
            kb.set_focus(self, Some(surface.wl_surface().clone().into()), smithay::utils::SERIAL_COUNTER.next_serial());
        }
        if let Lock::Locked(v) = &mut self.lock {
            v.retain(|(n, _)| *n != o.name());
            v.push((o.name(), surface));
        }
        let at = self.pointer_at;
        self.pointer_move(at, self.now_ms());
        self.dirty = true;
    }
}

delegate_session_lock!(Loom);
