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
