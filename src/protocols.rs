//! The smaller protocols programs expect: cursor shapes by name, xdg-activation (a link opened in
//! the running browser brings it forward), relative pointer and pointer constraints (games and 3D
//! programs lock or confine the pointer), keyboard-shortcuts-inhibit (a VM or remote desktop gets
//! loom's keys too), single-pixel buffers, xdg-foreign (a portal dialog over its window),
//! content-type and xdg-dialog.

use smithay::{
    delegate_content_type, delegate_cursor_shape, delegate_keyboard_shortcuts_inhibit, delegate_pointer_constraints, delegate_relative_pointer, delegate_single_pixel_buffer, delegate_xdg_activation, delegate_xdg_dialog, delegate_xdg_foreign,
    input::pointer::PointerHandle,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle},
    utils::{Logical, Point},
    wayland::{
        content_type::ContentTypeState,
        cursor_shape::CursorShapeManagerState,
        keyboard_shortcuts_inhibit::{KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor},
        pointer_constraints::{with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState},
        relative_pointer::RelativePointerManagerState,
        seat::WaylandFocus,
        shell::xdg::dialog::{XdgDialogHandler, XdgDialogState},
        single_pixel_buffer::SinglePixelBufferState,
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
    },
};

use crate::state::Loom;

/// The protocols' state loom keeps (the rest live as long as the display).
pub struct Protocols {
    pub activation: XdgActivationState,
    pub inhibit: KeyboardShortcutsInhibitState,
    pub foreign: XdgForeignState,
}

pub fn init(dh: &DisplayHandle) -> Protocols {
    std::mem::forget(CursorShapeManagerState::new::<Loom>(dh));
    std::mem::forget(RelativePointerManagerState::new::<Loom>(dh));
    std::mem::forget(PointerConstraintsState::new::<Loom>(dh));
    std::mem::forget(SinglePixelBufferState::new::<Loom>(dh));
    std::mem::forget(ContentTypeState::new::<Loom>(dh));
    std::mem::forget(XdgDialogState::new::<Loom>(dh));
    Protocols { activation: XdgActivationState::new::<Loom>(dh), inhibit: KeyboardShortcutsInhibitState::new::<Loom>(dh), foreign: XdgForeignState::new::<Loom>(dh) }
}

impl XdgActivationHandler for Loom {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.protocols.activation
    }

    fn request_activation(&mut self, token: XdgActivationToken, data: XdgActivationTokenData, surface: WlSurface) {
        // a token older than 10 s is stale (the user has moved on)
        if data.timestamp.elapsed().as_secs() >= 10 {
            self.protocols.activation.remove_token(&token);
            return;
        }
        let Some(m) = self.managed.iter().find(|m| m.window.wl_surface().as_deref() == Some(&surface)).cloned() else { return };
        // its workspace shown, the window up with the keyboard (not while locked)
        if !self.locked() {
            self.switch_workspace(m.ws);
            self.focus_window(&m.window);
        }
        self.protocols.activation.remove_token(&token);
    }
}

impl PointerConstraintsHandler for Loom {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // the pointer already over that surface: the lock or confinement starts now
        let over = pointer.current_focus().is_some_and(|f| &f == surface);
        if over {
            with_pointer_constraint(surface, pointer, |c| {
                if let Some(c) = c {
                    c.activate();
                }
            });
        }
    }

    fn cursor_position_hint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>, location: Point<f64, Logical>) {
        // a locked pointer let go: the cursor where the program says it is (surface-local)
        let locked = with_pointer_constraint(surface, pointer, |c| c.is_some_and(|c| c.is_active()));
        if !locked {
            return;
        }
        // Smithay hands the hint on as the program sent it: for an X program in its own (1.6×)
        // pixels (its window's screen's scale). Taken as loom's, Bitwig's middle-drag (it holds the cursor still and moves it
        // back) sent the hidden cursor off 1.6× too far, out of its window, and the lock let go
        let from_x = smithay::reexports::wayland_server::Resource::client(surface).is_some_and(|c| self.x.client.as_ref().is_some_and(|x| x.id() == c.id()));
        let location = if from_x { location.downscale(self.x_surface_scale(surface)) } else { location };
        if let Some(origin) = self.surface_origin(surface) {
            self.pointer_at = origin + location;
        }
    }
}

impl Loom {
    /// Where a surface's (0, 0) is in compositor space: its window's place.
    pub fn surface_origin(&self, s: &WlSurface) -> Option<Point<f64, Logical>> {
        let w = self.space.elements().find(|w| w.wl_surface().as_deref() == Some(s))?;
        let loc = self.space.element_location(w)?;
        Some((loc - w.geometry().loc).to_f64())
    }
}

impl KeyboardShortcutsInhibitHandler for Loom {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.protocols.inhibit
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // granted (a VM or a remote desktop): loom's binds stand aside while it has the keyboard.
        // Not for Xwayland: it asks whenever an X program grabs the keyboard (Java's menus in
        // Bitwig do), and every bind was dead while that program had the keys.
        let from_x = smithay::reexports::wayland_server::Resource::client(inhibitor.wl_surface()).is_some_and(|c| self.x.client.as_ref().is_some_and(|x| x.id() == c.id()));
        if from_x {
            eprintln!("loom: an X program asked to keep the keys from loom's binds: not granted");
            return;
        }
        inhibitor.activate();
    }
}

impl XdgForeignHandler for Loom {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.protocols.foreign
    }
}

impl XdgDialogHandler for Loom {}

delegate_cursor_shape!(Loom);
delegate_xdg_activation!(Loom);
delegate_relative_pointer!(Loom);
delegate_pointer_constraints!(Loom);
delegate_keyboard_shortcuts_inhibit!(Loom);
delegate_single_pixel_buffer!(Loom);
delegate_xdg_foreign!(Loom);
delegate_content_type!(Loom);
delegate_xdg_dialog!(Loom);

impl Loom {
    /// A mouse move's raw motion to the program under the pointer (relative-pointer). True when
    /// that program has locked the pointer: the cursor stays where it is.
    pub fn pointer_relative(&mut self, delta: Point<f64, Logical>, unaccel: Point<f64, Logical>, utime: u64) -> bool {
        use smithay::wayland::pointer_constraints::PointerConstraint;
        let pointer = self.seat.get_pointer().expect("pointer");
        let under = self.surface_at(self.pointer_at);
        pointer.relative_motion(self, under.clone(), &smithay::input::pointer::RelativeMotionEvent { delta, delta_unaccel: unaccel, utime });
        let locked = under.as_ref().is_some_and(|(s, _)| with_pointer_constraint(s, &pointer, |c| c.is_some_and(|c| c.is_active() && matches!(&*c, PointerConstraint::Locked(_)))));
        if locked {
            pointer.frame(self);
        }
        locked
    }

    /// A pointer confined to its program's window stays inside it.
    pub fn confine(&self, at: Point<f64, Logical>) -> Point<f64, Logical> {
        use smithay::wayland::pointer_constraints::PointerConstraint;
        let pointer = self.seat.get_pointer().expect("pointer");
        let Some((s, _)) = self.surface_at(self.pointer_at) else { return at };
        let confined = with_pointer_constraint(&s, &pointer, |c| c.is_some_and(|c| c.is_active() && matches!(&*c, PointerConstraint::Confined(_))));
        if !confined {
            return at;
        }
        let Some(w) = self.space.elements().find(|w| w.wl_surface().as_deref() == Some(&s)) else { return at };
        let (Some(loc), g) = (self.space.element_location(w), w.geometry()) else { return at };
        let (x0, y0) = (loc.x as f64, loc.y as f64);
        (at.x.clamp(x0, x0 + g.size.w as f64 - 1.0), at.y.clamp(y0, y0 + g.size.h as f64 - 1.0)).into()
    }
}
