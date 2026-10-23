//! The clipboard and the middle-click selection, one for Wayland and X programs alike: offered to
//! the focused client, mirrored to and from X (Xwayland's window manager), and readable by clipboard
//! tools (wl-paste --watch, cliphist: wlr- and ext-data-control).

use std::os::fd::OwnedFd;

use smithay::{
    delegate_data_control, delegate_ext_data_control, delegate_primary_selection,
    input::Seat,
    reexports::wayland_server::Resource,
    wayland::{
        seat::WaylandFocus,
        selection::{
            data_device::{clear_data_device_selection, current_data_device_selection_userdata, request_data_device_client_selection, set_data_device_focus, set_data_device_selection},
            ext_data_control, primary_selection::{clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection, set_primary_focus, set_primary_selection, PrimarySelectionHandler, PrimarySelectionState},
            wlr_data_control, SelectionHandler, SelectionSource, SelectionTarget,
        },
    },
    xwayland::xwm::XwmId,
};

use crate::state::Loom;

/// The protocol states (made with the display, kept in Loom).
pub struct Clipboard {
    pub primary: PrimarySelectionState,
    pub wlr: wlr_data_control::DataControlState,
    pub ext: ext_data_control::DataControlState,
}

impl Clipboard {
    pub fn new(dh: &smithay::reexports::wayland_server::DisplayHandle) -> Clipboard {
        let primary = PrimarySelectionState::new::<Loom>(dh);
        let wlr = wlr_data_control::DataControlState::new::<Loom, _>(dh, Some(&primary), |_| true);
        let ext = ext_data_control::DataControlState::new::<Loom, _>(dh, Some(&primary), |_| true);
        Clipboard { primary, wlr, ext }
    }
}

impl Loom {
    /// The keyboard moved: the focused program is the one offered the clipboard (on paste).
    pub fn clipboard_focus(&mut self, seat: &Seat<Loom>, focused: Option<&crate::focus::KbFocus>) {
        let client = focused.and_then(|f| f.wl_surface()).and_then(|s| self.dh.get_client(s.id()).ok());
        set_data_device_focus(&self.dh, seat, client.clone());
        set_primary_focus(&self.dh, seat, client);
    }

    /// An X program copied: what it offers becomes the Wayland selection (loom serves it).
    pub fn x_new_selection(&mut self, ty: SelectionTarget, mime_types: Vec<String>) {
        let seat = self.seat.clone();
        match ty {
            SelectionTarget::Clipboard => set_data_device_selection(&self.dh, &seat, mime_types, ()),
            SelectionTarget::Primary => set_primary_selection(&self.dh, &seat, mime_types, ()),
        }
    }

    /// The X selection is gone: so is the Wayland one, if it came from X.
    pub fn x_cleared_selection(&mut self, ty: SelectionTarget) {
        let seat = self.seat.clone();
        match ty {
            SelectionTarget::Clipboard if current_data_device_selection_userdata(&seat).is_some() => clear_data_device_selection(&self.dh, &seat),
            SelectionTarget::Primary if current_primary_selection_userdata(&seat).is_some() => clear_primary_selection(&self.dh, &seat),
            _ => {}
        }
    }

    /// An X program pastes what a Wayland program copied: that program writes it into `fd`.
    pub fn x_send_selection(&mut self, ty: SelectionTarget, mime_type: String, fd: OwnedFd) {
        let seat = self.seat.clone();
        let r = match ty {
            SelectionTarget::Clipboard => request_data_device_client_selection(&seat, mime_type, fd).map_err(|e| e.to_string()),
            SelectionTarget::Primary => request_primary_client_selection(&seat, mime_type, fd).map_err(|e| e.to_string()),
        };
        if let Err(e) = r {
            eprintln!("loom: X paste: {e}");
        }
    }

    /// Any X program may read the selection, as on X and Hyprland (xclip -o in a script, not only the focused window).
    pub fn x_may_read_selection(&self, _xwm: XwmId) -> bool {
        true
    }
}

impl SelectionHandler for Loom {
    type SelectionUserData = ();

    // a Wayland program copied: X programs see it too
    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(e) = xwm.new_selection(ty, source.map(|s| s.mime_types())) {
                eprintln!("loom: selection to X: {e}");
            }
        }
    }

    // a Wayland program pastes what an X program copied
    fn send_selection(&mut self, ty: SelectionTarget, mime_type: String, fd: OwnedFd, _seat: Seat<Self>, _user_data: &()) {
        let Some(h) = self.loop_handle.clone() else { return };
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(e) = xwm.send_selection(ty, mime_type, fd, h) {
                eprintln!("loom: paste from X: {e}");
            }
        }
    }
}

impl PrimarySelectionHandler for Loom {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.clipboard.primary
    }
}
impl wlr_data_control::DataControlHandler for Loom {
    fn data_control_state(&self) -> &wlr_data_control::DataControlState {
        &self.clipboard.wlr
    }
}
impl ext_data_control::DataControlHandler for Loom {
    fn data_control_state(&self) -> &ext_data_control::DataControlState {
        &self.clipboard.ext
    }
}

delegate_primary_selection!(Loom);
delegate_data_control!(Loom);
delegate_ext_data_control!(Loom);
