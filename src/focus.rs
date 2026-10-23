//! What has the keyboard: a Wayland surface (window, layer, popup) or an X window (whose X
//! focus must be set too, or X programs ignore the keys).

use std::borrow::Cow;

use smithay::{
    backend::input::KeyState,
    input::{
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
        Seat,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Serial},
    wayland::seat::WaylandFocus,
    xwayland::X11Surface,
};

use crate::state::Loom;

#[derive(Debug, Clone, PartialEq)]
pub enum KbFocus {
    Surface(WlSurface),
    X11(X11Surface),
}

impl From<WlSurface> for KbFocus {
    fn from(s: WlSurface) -> Self {
        KbFocus::Surface(s)
    }
}

impl IsAlive for KbFocus {
    fn alive(&self) -> bool {
        match self {
            KbFocus::Surface(s) => s.alive(),
            KbFocus::X11(x) => x.alive(),
        }
    }
}

impl WaylandFocus for KbFocus {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            KbFocus::Surface(s) => Some(Cow::Borrowed(s)),
            KbFocus::X11(x) => x.wl_surface().map(Cow::Owned),
        }
    }
}

impl KbFocus {
    /// its wl_surface (an X window has one once Xwayland paired them)
    pub fn surface(&self) -> Option<WlSurface> {
        WaylandFocus::wl_surface(self).map(|c| c.into_owned())
    }
}

macro_rules! each {
    ($self:ident, $t:ident => $e:expr) => {
        match $self {
            KbFocus::Surface($t) => $e,
            KbFocus::X11($t) => $e,
        }
    };
}

impl KeyboardTarget<Loom> for KbFocus {
    fn enter(&self, seat: &Seat<Loom>, data: &mut Loom, keys: Vec<KeysymHandle<'_>>, serial: Serial) {
        each!(self, t => KeyboardTarget::enter(t, seat, data, keys, serial))
    }
    fn leave(&self, seat: &Seat<Loom>, data: &mut Loom, serial: Serial) {
        each!(self, t => KeyboardTarget::leave(t, seat, data, serial))
    }
    fn key(&self, seat: &Seat<Loom>, data: &mut Loom, key: KeysymHandle<'_>, state: KeyState, serial: Serial, time: u32) {
        each!(self, t => KeyboardTarget::key(t, seat, data, key, state, serial, time))
    }
    fn modifiers(&self, seat: &Seat<Loom>, data: &mut Loom, modifiers: ModifiersState, serial: Serial) {
        each!(self, t => KeyboardTarget::modifiers(t, seat, data, modifiers, serial))
    }
}

use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, LayerSurfaceCachedState};

impl Loom {
    /// What a layer surface asked for the keyboard (none, on demand, exclusive).
    pub fn layer_keyboard(&self, s: &WlSurface) -> KeyboardInteractivity {
        smithay::wayland::compositor::with_states(s, |st| st.cached_state.get::<LayerSurfaceCachedState>().current().keyboard_interactivity)
    }

    /// A layer's commit: it gets the keyboard when it starts asking for it (it appeared, or its
    /// setting turned on, as the launcher does), and gives it back when it stops.
    pub fn layer_keyboard_changed(&mut self, s: &WlSurface, shown: bool) {
        // a layer behind the windows (the desktop's icons and widgets) that only allows the
        // keyboard gets it by a click, not by appearing: as Hyprland, it never takes it from a window
        let behind = self.layer_output_of(s).is_some_and(|(_, l)| matches!(l.layer(), smithay::wayland::shell::wlr_layer::Layer::Background | smithay::wayland::shell::wlr_layer::Layer::Bottom));
        let k = self.layer_keyboard(s);
        let wants = shown && k != KeyboardInteractivity::None && !(behind && k == KeyboardInteractivity::OnDemand);
        let id = smithay::reexports::wayland_server::Resource::id(s);
        let had = self.layer_kb_wanted.contains(&id);
        if wants && !had {
            self.layer_kb_wanted.insert(id);
            eprintln!("loom: keyboard to layer {}", self.layer_ns(s));
            self.focus_layer(s);
        } else if !wants && had {
            self.layer_kb_wanted.remove(&id);
            eprintln!("loom: layer {} gave the keyboard back", self.layer_ns(s));
            self.layer_gave_up_keyboard(s);
        } else if !wants && (!shown || k == KeyboardInteractivity::None) {
            // it had the keyboard from a click, not by asking (the desktop's icons): hidden, gone
            // or no longer taking keys, it gives it back too (keys went to a gone surface before)
            self.layer_gave_up_keyboard(s);
        }
    }

    /// The keyboard to a layer (not while locked).
    pub fn focus_layer(&mut self, s: &WlSurface) {
        if self.locked() {
            return;
        }
        let kb = self.seat.get_keyboard().expect("keyboard");
        if kb.current_focus().and_then(|f| f.surface()).as_ref() != Some(s) {
            kb.set_focus(self, Some(s.clone().into()), smithay::utils::SERIAL_COUNTER.next_serial());
        }
    }

    /// A layer that had the keyboard is gone or stopped asking: back to the window that had it.
    pub fn layer_gave_up_keyboard(&mut self, s: &WlSurface) {
        let kb = self.seat.get_keyboard().expect("keyboard");
        if kb.current_focus().and_then(|f| f.surface()).as_ref() != Some(s) {
            return;
        }
        match self.focused.clone() {
            Some(w) => self.focus_window(&w),
            None => kb.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial()),
        }
    }

    /// A focus grab let go of panels (closed, or the grab ended): the keyboard, when it was in one
    /// of them, back to the window that had it, else the one under the pointer, as Hyprland does.
    /// Without it the keys went into a closed panel until a window was clicked.
    pub fn grab_released(&mut self, old: &[WlSurface]) {
        let kb = self.seat.get_keyboard().expect("keyboard");
        let Some(f) = kb.current_focus().and_then(|f| f.surface()) else { return };
        if !old.contains(&f) || self.proto.grabs.iter().any(|g| g.surfaces.contains(&f)) {
            return;
        }
        // a panel asking for the keyboard itself (the launcher) keeps it
        if self.layer_kb_wanted.contains(&smithay::reexports::wayland_server::Resource::id(&f)) {
            return;
        }
        let under = self.space.element_under(self.pointer_at).map(|(w, _)| w.clone()).filter(|w| self.managed_of(w).is_some());
        match self.focused.clone().or(under) {
            Some(w) => self.focus_window(&w),
            None => kb.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial()),
        }
    }
}

impl Loom {
    /// A layer surface's namespace (for the log), "?" when it is none.
    pub fn layer_ns(&self, s: &WlSurface) -> String {
        self.layer_output_of(s).map(|(_, l)| l.namespace().to_string()).unwrap_or_else(|| "?".into())
    }
}

/// Menus (xdg popups with a grab): the keyboard goes to the top one, back down when it closes,
/// and a click anywhere else closes them, as a menu does. Done here rather than with Smithay's
/// popup grab, which wants every focus (an X window too) to turn into a wl_surface.
impl Loom {
    /// A popup asked for the grab: it gets the keyboard (where it was is kept for later).
    pub fn popup_grab_start(&mut self, p: smithay::wayland::shell::xdg::PopupSurface) {
        if self.locked() || !p.alive() {
            return;
        }
        self.popup_grab.retain(|q| q.alive() && *q != p);
        let kb = self.seat.get_keyboard().expect("keyboard");
        if self.popup_grab.is_empty() {
            self.popup_grab_before = kb.current_focus();
        }
        let s = p.wl_surface().clone();
        self.popup_grab.push(p);
        kb.set_focus(self, Some(s.into()), smithay::utils::SERIAL_COUNTER.next_serial());
    }

    /// A popup gone: off the stack; if it had the keyboard, the next menu down gets it, else
    /// where the keyboard was before the menus (a panel, a window).
    pub fn popup_grab_gone(&mut self, p: &smithay::wayland::shell::xdg::PopupSurface) {
        let had = self.popup_grab.iter().any(|q| q == p);
        self.popup_grab.retain(|q| q.alive() && q != p);
        if !had {
            return;
        }
        let kb = self.seat.get_keyboard().expect("keyboard");
        let on_it = kb.current_focus().and_then(|f| f.surface()).is_none_or(|s| s == *p.wl_surface() || !smithay::reexports::wayland_server::Resource::is_alive(&s));
        if !on_it {
            return;
        }
        match self.popup_grab.last().map(|q| q.wl_surface().clone()) {
            Some(s) => kb.set_focus(self, Some(s.into()), smithay::utils::SERIAL_COUNTER.next_serial()),
            None => self.popup_grab_restore(),
        }
    }

    /// A press: outside every grabbing menu, they all close (the press still reaches what is there).
    pub fn popup_grab_press(&mut self, at: smithay::utils::Point<f64, smithay::utils::Logical>) {
        self.popup_grab.retain(|q| q.alive());
        if self.popup_grab.is_empty() {
            return;
        }
        let inside = self.surface_at(at).map(|(s, _)| root_of(&s)).is_some_and(|r| self.popup_grab.iter().any(|q| *q.wl_surface() == r));
        if inside {
            return;
        }
        for p in std::mem::take(&mut self.popup_grab).into_iter().rev() {
            if std::env::var_os("LOOM_DEBUG_POPUP").is_some() {
                eprintln!("loom: popup done (a press outside) {:?}", smithay::reexports::wayland_server::Resource::id(p.wl_surface()));
            }
            p.send_popup_done();
        }
        self.popup_grab_restore();
    }

    /// The menus are closed: the keyboard back where it was before them, if that is still there,
    /// else to the focused window.
    fn popup_grab_restore(&mut self) {
        let before = self.popup_grab_before.take().filter(|f| f.alive());
        let kb = self.seat.get_keyboard().expect("keyboard");
        match (before, self.focused.clone()) {
            (Some(f), _) => kb.set_focus(self, Some(f), smithay::utils::SERIAL_COUNTER.next_serial()),
            (None, Some(w)) => self.focus_window(&w),
            (None, None) => kb.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial()),
        }
    }
}

/// The surface a subsurface belongs to (itself when it is none).
fn root_of(s: &WlSurface) -> WlSurface {
    let mut s = s.clone();
    while let Some(p) = smithay::wayland::compositor::get_parent(&s) {
        s = p;
    }
    s
}
