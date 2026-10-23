//! Idle: ext-idle-notify (hypridle learns the user is away), idle-inhibit (a video, the shell's
//! caffeine keep it from going idle), and DPMS (`dpms off|on|toggle`, the screen switched off
//! on the real screen; a key press switches it on again).

use smithay::{
    delegate_idle_inhibit, delegate_idle_notify,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::IsAlive,
    wayland::{
        idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState},
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
    },
};

use crate::state::Loom;

pub fn init(dh: &smithay::reexports::wayland_server::DisplayHandle) {
    // lives as long as the display (no state of its own to keep)
    std::mem::forget(IdleInhibitManagerState::new::<Loom>(dh));
}

impl Loom {
    /// The notifier needs the event loop (its timers): made once the backend has one.
    pub fn init_idle(&mut self) {
        if let (None, Some(h)) = (&self.idle, self.loop_handle.clone()) {
            self.idle = Some(IdleNotifierState::new(&self.dh, h));
        }
    }

    /// The user did something (a key, the pointer): not idle; a key press wakes the screen.
    pub fn activity(&mut self, key_press: bool) {
        if let Some(i) = self.idle.as_mut() {
            i.notify_activity(&self.seat);
        }
        if key_press && self.screen_off {
            self.dpms("on");
        }
    }

    fn inhibitors_changed(&mut self) {
        self.inhibitors.retain(|s| s.alive());
        let on = !self.inhibitors.is_empty();
        eprintln!("loom: idle {}", if on { "inhibited" } else { "allowed" });
        if let Some(i) = self.idle.as_mut() {
            i.set_is_inhibited(on);
        }
    }

    /// `dpms off`, `on` or `toggle`: the screen dark (nothing drawn) or back.
    pub fn dpms(&mut self, a: &str) -> String {
        let off = match a.split_whitespace().next().unwrap_or("toggle") {
            "off" => true,
            "on" => false,
            "toggle" => !self.screen_off,
            other => return format!("dpms: {other}? (on, off, toggle)"),
        };
        if off != self.screen_off {
            eprintln!("loom: screen {}", if off { "off" } else { "on" });
            self.screen_off = off;
            self.dirty = true;
        }
        "ok".into()
    }
}

impl IdleNotifierHandler for Loom {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        self.idle.as_mut().expect("idle notifier made with the event loop")
    }
}

impl IdleInhibitHandler for Loom {
    fn inhibit(&mut self, surface: WlSurface) {
        self.inhibitors.push(surface);
        self.inhibitors_changed();
    }
    fn uninhibit(&mut self, surface: WlSurface) {
        self.inhibitors.retain(|s| s != &surface);
        self.inhibitors_changed();
    }
}

delegate_idle_notify!(Loom);
delegate_idle_inhibit!(Loom);
