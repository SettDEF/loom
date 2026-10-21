//! Hyprland's own Wayland protocols, as the shell uses them: global shortcuts (Quickshell's
//! GlobalShortcut, fired by `dispatch global app:name` and by binds), focus grab (the launcher and
//! sidebars keep the keyboard, a click elsewhere closes them), per-surface opacity, lock notify,
//! the colour matrix (night light; accepted, applied once loom draws to a real screen).

use std::collections::HashMap;

use smithay::reexports::wayland_server::{
    backend::{ClientId, ObjectId},
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

macro_rules! protocol {
    ($name:ident, $file:literal) => {
        pub mod $name {
            #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables, non_upper_case_globals, non_snake_case, unused_imports, missing_docs, clippy::all)]
            use wayland_server;
            use wayland_server::protocol::*;
            pub mod __interfaces {
                use wayland_server::protocol::__interfaces::*;
                use wayland_server::backend as wayland_backend;
                wayland_scanner::generate_interfaces!($file);
            }
            use self::__interfaces::*;
            wayland_scanner::generate_server_code!($file);
        }
    };
}
protocol!(shortcuts, "protocols/hyprland-global-shortcuts-v1.xml");
protocol!(grab, "protocols/hyprland-focus-grab-v1.xml");
protocol!(surface, "protocols/hyprland-surface-v1.xml");
protocol!(lock, "protocols/hyprland-lock-notify-v1.xml");
protocol!(ctm, "protocols/hyprland-ctm-control-v1.xml");

use ctm::hyprland_ctm_control_manager_v1::{self as ctm_m, HyprlandCtmControlManagerV1};
use grab::{
    hyprland_focus_grab_manager_v1::{self as grab_m, HyprlandFocusGrabManagerV1},
    hyprland_focus_grab_v1::{self as grab_g, HyprlandFocusGrabV1},
};
use lock::{
    hyprland_lock_notification_v1::{self as lock_n, HyprlandLockNotificationV1},
    hyprland_lock_notifier_v1::{self as lock_m, HyprlandLockNotifierV1},
};
use shortcuts::{
    hyprland_global_shortcut_v1::{self as sc_s, HyprlandGlobalShortcutV1},
    hyprland_global_shortcuts_manager_v1::{self as sc_m, HyprlandGlobalShortcutsManagerV1},
};
use surface::{
    hyprland_surface_manager_v1::{self as surf_m, HyprlandSurfaceManagerV1},
    hyprland_surface_v1::{self as surf_s, HyprlandSurfaceV1},
};

use crate::state::Loom;

pub struct Shortcut {
    pub obj: HyprlandGlobalShortcutV1,
    pub app_id: String,
    pub id: String,
    pub description: String,
}

/// A focus grab: the surfaces that keep the keyboard, as committed and as being set up.
pub struct Grab {
    pub obj: HyprlandFocusGrabV1,
    pub pending: Vec<WlSurface>,
    pub surfaces: Vec<WlSurface>,
}

#[derive(Default)]
pub struct HyprProto {
    pub shortcuts: Vec<Shortcut>,
    pub grabs: Vec<Grab>,
    /// a surface's opacity (hyprland_surface_v1), by wl_surface
    pub opacity: HashMap<ObjectId, f32>,
    /// what each hyprland_surface_v1 is for
    surface_of: HashMap<ObjectId, ObjectId>,
    pub lock_notes: Vec<HyprlandLockNotificationV1>,
}

/// The globals, made once.
pub fn init(dh: &DisplayHandle) {
    dh.create_global::<Loom, HyprlandGlobalShortcutsManagerV1, ()>(1, ());
    dh.create_global::<Loom, HyprlandFocusGrabManagerV1, ()>(1, ());
    dh.create_global::<Loom, HyprlandSurfaceManagerV1, ()>(2, ());
    dh.create_global::<Loom, HyprlandLockNotifierV1, ()>(1, ());
    dh.create_global::<Loom, HyprlandCtmControlManagerV1, ()>(2, ());
}

macro_rules! bind_plain {
    ($($t:ty),*) => {$(
        impl GlobalDispatch<$t, ()> for Loom {
            fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<$t>, _: &(), data_init: &mut DataInit<'_, Self>) {
                data_init.init(resource, ());
            }
        }
    )*};
}
bind_plain!(HyprlandGlobalShortcutsManagerV1, HyprlandFocusGrabManagerV1, HyprlandSurfaceManagerV1, HyprlandLockNotifierV1, HyprlandCtmControlManagerV1);

impl Dispatch<HyprlandGlobalShortcutsManagerV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, _: &HyprlandGlobalShortcutsManagerV1, request: sc_m::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        if let sc_m::Request::RegisterShortcut { shortcut, id, app_id, description, .. } = request {
            let obj = data_init.init(shortcut, ());
            state.proto.shortcuts.retain(|s| !(s.app_id == app_id && s.id == id));
            state.proto.shortcuts.push(Shortcut { obj, app_id, id, description });
        }
    }
}

impl Dispatch<HyprlandGlobalShortcutV1, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &HyprlandGlobalShortcutV1, _: sc_s::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
    fn destroyed(state: &mut Self, _: ClientId, resource: &HyprlandGlobalShortcutV1, _: &()) {
        state.proto.shortcuts.retain(|s| &s.obj != resource);
    }
}

impl Dispatch<HyprlandFocusGrabManagerV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, _: &HyprlandFocusGrabManagerV1, request: grab_m::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        if let grab_m::Request::CreateGrab { grab } = request {
            let obj = data_init.init(grab, ());
            state.proto.grabs.push(Grab { obj, pending: Vec::new(), surfaces: Vec::new() });
        }
    }
}

impl Dispatch<HyprlandFocusGrabV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, resource: &HyprlandFocusGrabV1, request: grab_g::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let Some(g) = state.proto.grabs.iter_mut().find(|g| &g.obj == resource) else { return };
        match request {
            grab_g::Request::AddSurface { surface } => {
                if !g.pending.contains(&surface) {
                    g.pending.push(surface);
                }
            }
            grab_g::Request::RemoveSurface { surface } => g.pending.retain(|s| s != &surface),
            grab_g::Request::Commit => {
                g.surfaces = g.pending.clone();
                let first = g.surfaces.first().cloned();
                // the keyboard goes into the grab, unless it is in it already
                let kb = state.seat.get_keyboard().expect("keyboard");
                let inside = kb.current_focus().is_some_and(|f| state.proto.grabs.iter().any(|g| g.surfaces.contains(&f)));
                if let (Some(s), false) = (first, inside) {
                    kb.set_focus(state, Some(s), smithay::utils::SERIAL_COUNTER.next_serial());
                }
            }
            _ => {}
        }
    }
    fn destroyed(state: &mut Self, _: ClientId, resource: &HyprlandFocusGrabV1, _: &()) {
        state.proto.grabs.retain(|g| &g.obj != resource);
    }
}

impl Dispatch<HyprlandSurfaceManagerV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, _: &HyprlandSurfaceManagerV1, request: surf_m::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        if let surf_m::Request::GetHyprlandSurface { id, surface } = request {
            let obj = data_init.init(id, ());
            state.proto.surface_of.insert(obj.id(), surface.id());
        }
    }
}

impl Dispatch<HyprlandSurfaceV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, resource: &HyprlandSurfaceV1, request: surf_s::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let Some(s) = state.proto.surface_of.get(&resource.id()).cloned() else { return };
        if let surf_s::Request::SetOpacity { opacity } = request {
            state.proto.opacity.insert(s, opacity.clamp(0.0, 1.0) as f32);
            state.dirty = true;
        }
    }
    fn destroyed(state: &mut Self, _: ClientId, resource: &HyprlandSurfaceV1, _: &()) {
        if let Some(s) = state.proto.surface_of.remove(&resource.id()) {
            state.proto.opacity.remove(&s);
            state.dirty = true;
        }
    }
}

impl Dispatch<HyprlandLockNotifierV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, _: &HyprlandLockNotifierV1, request: lock_m::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        if let lock_m::Request::GetLockNotification { id } = request {
            state.proto.lock_notes.push(data_init.init(id, ()));
        }
    }
}

impl Dispatch<HyprlandLockNotificationV1, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &HyprlandLockNotificationV1, _: lock_n::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
    fn destroyed(state: &mut Self, _: ClientId, resource: &HyprlandLockNotificationV1, _: &()) {
        state.proto.lock_notes.retain(|n| n != resource);
    }
}

impl Dispatch<HyprlandCtmControlManagerV1, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &HyprlandCtmControlManagerV1, _: ctm_m::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl Loom {
    /// `dispatch global app:name`: that shortcut pressed (Hyprland's dispatcher sends only that).
    pub fn global_shortcut(&mut self, which: &str, pressed: bool) -> bool {
        let (app, id) = which.split_once(':').unwrap_or(("", which));
        let Some(s) = self.proto.shortcuts.iter().find(|s| s.id == id && (app.is_empty() || s.app_id == app)) else { return false };
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let (hi, lo, ns) = ((t.as_secs() >> 32) as u32, t.as_secs() as u32, t.subsec_nanos());
        if pressed {
            s.obj.pressed(hi, lo, ns);
        } else {
            s.obj.released(hi, lo, ns);
        }
        true
    }

    /// A press at `at`: a focus grab it falls outside of is cleared (the launcher closes).
    pub fn grab_press(&mut self, at: smithay::utils::Point<f64, smithay::utils::Logical>) {
        if self.proto.grabs.iter().all(|g| g.surfaces.is_empty()) {
            return;
        }
        let hit = self.root_at(at);
        for g in self.proto.grabs.iter_mut().filter(|g| !g.surfaces.is_empty()) {
            if !hit.as_ref().is_some_and(|h| g.surfaces.contains(h)) {
                g.obj.cleared();
                g.surfaces.clear();
                g.pending.clear();
            }
        }
    }

    /// The window's or layer's own surface under `at` (not a popup's or subsurface's).
    pub fn root_at(&self, at: smithay::utils::Point<f64, smithay::utils::Logical>) -> Option<WlSurface> {
        use smithay::wayland::shell::wlr_layer::Layer as L;
        let output = self.output.as_ref()?;
        let map = smithay::desktop::layer_map_for_output(output);
        let layer = |which: &[L]| which.iter().find_map(|l| map.layer_under(*l, at).map(|s| s.wl_surface().clone()));
        layer(&[L::Overlay, L::Top])
            .or_else(|| self.space.element_under(at).and_then(|(w, _)| w.toplevel().map(|t| t.wl_surface().clone())))
            .or_else(|| layer(&[L::Bottom, L::Background]))
    }

    /// The opacity the shell gave a surface, 1 when none.
    pub fn opacity_of(&self, s: &WlSurface) -> f32 {
        self.proto.opacity.get(&s.id()).copied().unwrap_or(1.0)
    }
}
