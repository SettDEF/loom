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
    /// colour matrices set but not committed yet (by screen name)
    pub ctm_pending: std::collections::HashMap<String, [f64; 9]>,
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
                let old = std::mem::replace(&mut g.surfaces, g.pending.clone());
                // the newest one: the panel just opened (the bar comes first in the shell's list)
                let first = g.surfaces.last().cloned();
                // the keyboard goes into the grab, unless it is in it already
                let kb = state.seat.get_keyboard().expect("keyboard");
                let inside = kb.current_focus().and_then(|f| f.surface()).is_some_and(|f| state.proto.grabs.iter().any(|g| g.surfaces.contains(&f)));
                if let (Some(s), false, false) = (first, inside, state.locked()) {
                    kb.set_focus(state, Some(s.into()), smithay::utils::SERIAL_COUNTER.next_serial());
                }
                // a panel taken out of the grab (closed) gives the keyboard back
                state.grab_released(&old);
            }
            _ => {}
        }
    }
    fn destroyed(state: &mut Self, _: ClientId, resource: &HyprlandFocusGrabV1, _: &()) {
        let old: Vec<_> = state.proto.grabs.iter().filter(|g| &g.obj == resource).flat_map(|g| g.surfaces.clone()).collect();
        state.proto.grabs.retain(|g| &g.obj != resource);
        state.grab_released(&old);
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
            let n = data_init.init(id, ());
            // locked already: it hears so at once
            if state.locked() {
                n.locked();
            }
            state.proto.lock_notes.push(n);
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
    fn request(state: &mut Self, _: &Client, _: &HyprlandCtmControlManagerV1, request: ctm_m::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        match request {
            // a colour matrix for a screen (hyprsunset's night light), applied on commit
            ctm_m::Request::SetCtmForOutput { output, mat0, mat1, mat2, mat3, mat4, mat5, mat6, mat7, mat8 } => {
                if let Some(o) = smithay::output::Output::from_resource(&output) {
                    state.proto.ctm_pending.insert(o.name(), [mat0, mat1, mat2, mat3, mat4, mat5, mat6, mat7, mat8]);
                }
            }
            ctm_m::Request::Commit => {
                for (name, m) in std::mem::take(&mut state.proto.ctm_pending) {
                    state.set_ctm(&name, m);
                }
            }
            _ => {}
        }
    }
    // its client gone: the screens back to their own colours, as Hyprland does
    fn destroyed(state: &mut Self, _: ClientId, _: &HyprlandCtmControlManagerV1, _: &()) {
        for o in state.outputs.clone() {
            state.set_ctm(&o.name(), [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        }
    }
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
        // the screen under it; its layers' boxes are relative to it
        let output = self.output_at(at).or_else(|| self.output.clone())?;
        let off = self.space.output_geometry(&output).map(|g| g.loc.to_f64()).unwrap_or_default();
        let map = smithay::desktop::layer_map_for_output(&output);
        // by each layer's input region, not its box (a mostly see-through bar is not "under" every click)
        let layer = |which: &[L]| crate::state::layer_input_at(&map, which, at - off).map(|(ly, _, _)| ly.wl_surface().clone());
        layer(&[L::Overlay, L::Top])
            .or_else(|| self.space.element_under(at).and_then(|(w, _)| w.toplevel().map(|t| t.wl_surface().clone()).or_else(|| w.x11_surface().and_then(|x| x.wl_surface()))))
            .or_else(|| layer(&[L::Bottom, L::Background]))
    }

    /// The opacity the shell gave a surface, 1 when none.
    pub fn opacity_of(&self, s: &WlSurface) -> f32 {
        self.proto.opacity.get(&s.id()).copied().unwrap_or(1.0)
    }
}

// ── hyprland-toplevel-mapping-v1: a window of the dock's list → its Hyprland address ──

pub mod mapping {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables, non_upper_case_globals, non_snake_case, unused_imports, missing_docs, clippy::all)]
    use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::*;
    use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::*;
    use wayland_server;
    use wayland_server::protocol::*;
    pub mod __interfaces {
        use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::__interfaces::*;
        use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::__interfaces::*;
        use wayland_server::backend as wayland_backend;
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-mapping-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/hyprland-toplevel-mapping-v1.xml");
}

use mapping::{
    hyprland_toplevel_mapping_manager_v1::{self as map_m, HyprlandToplevelMappingManagerV1},
    hyprland_toplevel_window_mapping_handle_v1::{self as map_h, HyprlandToplevelWindowMappingHandleV1},
};

pub fn init_mapping(dh: &DisplayHandle) {
    dh.create_global::<Loom, HyprlandToplevelMappingManagerV1, ()>(1, ());
}
bind_plain!(HyprlandToplevelMappingManagerV1);

impl Dispatch<HyprlandToplevelMappingManagerV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, _: &HyprlandToplevelMappingManagerV1, request: map_m::Request, _: &(), _: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        match request {
            map_m::Request::GetWindowForToplevelWlr { handle, toplevel } => {
                let h = data_init.init(handle, ());
                // the address the dock's handle was made for (foreign.rs keeps it as its data)
                match toplevel.data::<u64>().copied().filter(|a| state.managed.iter().any(|m| m.addr == *a)) {
                    Some(addr) => h.window_address((addr >> 32) as u32, addr as u32),
                    None => h.failed(),
                }
            }
            map_m::Request::GetWindowForToplevel { handle, .. } => {
                // ext-foreign-toplevel-list is not served yet
                data_init.init(handle, ()).failed();
            }
            _ => {}
        }
    }
}

impl Dispatch<HyprlandToplevelWindowMappingHandleV1, ()> for Loom {
    fn request(_: &mut Self, _: &Client, _: &HyprlandToplevelWindowMappingHandleV1, _: map_h::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

/// What holds a global shortcut down: a key (its keycode) or a mouse button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Held {
    Key(u32),
    Button(u32),
}

/// The global shortcuts among `actions` (`global app:name`), held by `by` from now on.
fn hold(list: &mut Vec<(Held, String)>, by: Held, actions: &[String]) {
    for a in actions {
        if let Some(name) = a.strip_prefix("global ") {
            let name = name.trim().to_string();
            if !list.iter().any(|(h, n)| *h == by && *n == name) {
                list.push((by, name));
            }
        }
    }
}

/// `by` came up: the shortcuts it held, taken off the list.
fn let_go(list: &mut Vec<(Held, String)>, by: Held) -> Vec<String> {
    let (up, keep): (Vec<_>, Vec<_>) = std::mem::take(list).into_iter().partition(|(h, _)| *h == by);
    *list = keep;
    up.into_iter().map(|(_, n)| n).collect()
}

impl Loom {
    /// After a bind ran `actions` on the press of `by`: its global shortcuts are held until `by` is let go.
    pub fn hold_globals(&mut self, by: Held, actions: &[String]) {
        hold(&mut self.globals_held, by, actions);
    }

    /// `by` came up: the global shortcuts it pressed are released.
    pub fn release_globals(&mut self, by: Held) {
        for name in let_go(&mut self.globals_held, by) {
            self.global_shortcut(&name, false);
        }
    }
}

#[cfg(test)]
mod held_tests {
    use super::{hold, let_go, Held};

    #[test]
    fn a_global_is_released_by_what_pressed_it_only() {
        let mut l = Vec::new();
        // Super+left drag: the shell's shortcut and the drag itself
        hold(&mut l, Held::Button(272), &["global quickshell:windowDrag".into(), "__drag move".into()]);
        hold(&mut l, Held::Key(125), &["exec foo".into()]);
        assert_eq!(l.len(), 1, "only the global is held");
        assert!(let_go(&mut l, Held::Key(125)).is_empty(), "another key lets nothing go");
        assert_eq!(let_go(&mut l, Held::Button(272)), vec!["quickshell:windowDrag".to_string()]);
        assert!(l.is_empty() && let_go(&mut l, Held::Button(272)).is_empty(), "released once");
    }
}
