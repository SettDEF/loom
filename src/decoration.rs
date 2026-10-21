//! Server-side decorations, as Hyprland does them: programs are told the compositor decorates
//! them, so they draw no title bar or borders of their own (xdg-decoration for GTK/winit/most,
//! KDE's server-decoration for Qt). Saves their CPU (winit drew five CPU surfaces per frame) and
//! looks as windows do on Hyprland. loom draws borders itself later (milestone 6).

use smithay::{
    delegate_kde_decoration, delegate_xdg_decoration,
    reexports::{
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
        wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as KdeMode,
    },
    wayland::shell::{
        kde::decoration::{KdeDecorationHandler, KdeDecorationState},
        xdg::{decoration::XdgDecorationHandler, ToplevelSurface},
    },
};

use crate::state::Loom;

pub fn init(dh: &smithay::reexports::wayland_server::DisplayHandle) -> (smithay::wayland::shell::xdg::decoration::XdgDecorationState, KdeDecorationState) {
    (smithay::wayland::shell::xdg::decoration::XdgDecorationState::new::<Loom>(dh), KdeDecorationState::new::<Loom>(dh, KdeMode::Server))
}

fn server_side(t: &ToplevelSurface) {
    t.with_pending_state(|s| s.decoration_mode = Some(Mode::ServerSide));
    if t.is_initial_configure_sent() {
        t.send_pending_configure();
    }
}

impl XdgDecorationHandler for Loom {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        server_side(&toplevel);
    }
    // whatever a program asks for, the compositor decorates (as Hyprland)
    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: Mode) {
        server_side(&toplevel);
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        server_side(&toplevel);
    }
}
delegate_xdg_decoration!(Loom);

impl KdeDecorationHandler for Loom {
    fn kde_decoration_state(&self) -> &KdeDecorationState {
        &self.kde_decoration_state
    }
}
delegate_kde_decoration!(Loom);
