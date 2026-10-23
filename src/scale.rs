//! Exact fractional scaling (wp_fractional_scale + wp_viewporter): programs draw at the screen's
//! 1.6 instead of 2 shrunk by loom, as on Hyprland: sharp, and less to draw.

use smithay::{
    delegate_fractional_scale, delegate_viewporter,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle},
    wayland::{
        compositor::with_states,
        fractional_scale::{with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState},
        viewporter::ViewporterState,
    },
};

use crate::state::Loom;

pub fn init(dh: &DisplayHandle) {
    // the globals live as long as the display
    std::mem::forget(FractionalScaleManagerState::new::<Loom>(dh));
    std::mem::forget(ViewporterState::new::<Loom>(dh));
}

impl FractionalScaleHandler for Loom {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self.output.as_ref().map(|o| o.current_scale().fractional_scale()).unwrap_or(1.0);
        with_states(&surface, |s| with_fractional_scale(s, |f| f.set_preferred_scale(scale)));
    }
}

delegate_fractional_scale!(Loom);
delegate_viewporter!(Loom);

/// A surface and everything in it (subsurfaces) get `scale` as their preferred one; sent only
/// when it changes. Set when the surface was made, from the focused screen, it was never
/// corrected: Zen opened while the other screen had the focus drew at 1.0 on the 1.6 one.
pub fn set_tree_scale(surface: &WlSurface, scale: f64) {
    use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, states, _| with_fractional_scale(states, |f| f.set_preferred_scale(scale)),
        |_, _, _| true,
    );
}
