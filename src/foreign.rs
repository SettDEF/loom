//! wlr-foreign-toplevel-management: the window list for docks and task bars (Quickshell's
//! ToplevelManager, the shell's dock). Every managed window, with title, app id, output and
//! state; activate, close, (un)fullscreen and minimize asked by the dock are carried out.

use smithay::reexports::{
    wayland_protocols_wlr::foreign_toplevel::v1::server::{
        zwlr_foreign_toplevel_handle_v1::{self as handle, State, ZwlrForeignToplevelHandleV1},
        zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
    },
    wayland_server::{backend::ClientId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource},
};

use crate::state::Loom;

/// The docks listening, and each window's handles (one per dock) by its address.
#[derive(Default)]
pub struct Foreign {
    pub managers: Vec<ZwlrForeignToplevelManagerV1>,
    pub handles: Vec<(u64, ZwlrForeignToplevelHandleV1)>,
}

pub fn init(dh: &DisplayHandle) {
    dh.create_global::<Loom, ZwlrForeignToplevelManagerV1, ()>(3, ());
}

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for Loom {
    fn bind(state: &mut Self, dh: &DisplayHandle, client: &Client, resource: New<ZwlrForeignToplevelManagerV1>, _: &(), data_init: &mut DataInit<'_, Self>) {
        let m = data_init.init(resource, ());
        // what is open already
        let addrs: Vec<u64> = state.managed.iter().map(|w| w.addr).collect();
        for addr in addrs {
            state.announce_to(dh, client, &m, addr);
        }
        state.foreign.managers.push(m);
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Loom {
    fn request(state: &mut Self, _: &Client, resource: &ZwlrForeignToplevelManagerV1, request: manager::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let manager::Request::Stop = request {
            resource.finished();
            state.foreign.managers.retain(|m| m != resource);
        }
    }
    fn destroyed(state: &mut Self, _: ClientId, resource: &ZwlrForeignToplevelManagerV1, _: &()) {
        state.foreign.managers.retain(|m| m != resource);
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, u64> for Loom {
    fn request(state: &mut Self, _: &Client, _: &ZwlrForeignToplevelHandleV1, request: handle::Request, addr: &u64, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let Some(m) = state.managed.iter().find(|m| m.addr == *addr).cloned() else { return };
        match request {
            handle::Request::Activate { .. } => {
                // its workspace shown, the window up and focused (a dock click)
                if m.ws != state.active_ws {
                    state.switch_workspace(m.ws);
                }
                state.focus_window(&m.window);
            }
            handle::Request::Close => {
                if let Some(t) = m.window.toplevel() {
                    t.send_close();
                } else if let Some(x) = m.window.x11_surface() {
                    let _ = x.close();
                }
            }
            handle::Request::SetFullscreen { .. } | handle::Request::UnsetFullscreen => {
                let want = matches!(request, handle::Request::SetFullscreen { .. });
                if state.is_full(&m.window) != want {
                    state.focus_window(&m.window);
                    state.toggle_fullscreen();
                }
            }
            _ => {}
        }
    }
    fn destroyed(state: &mut Self, _: ClientId, resource: &ZwlrForeignToplevelHandleV1, _: &u64) {
        state.foreign.handles.retain(|(_, h)| h != resource);
    }
}

impl Loom {
    /// A window made known to one dock: its handle, title, app id, output, state.
    fn announce_to(&mut self, dh: &DisplayHandle, client: &Client, m: &ZwlrForeignToplevelManagerV1, addr: u64) {
        let Ok(h) = client.create_resource::<ZwlrForeignToplevelHandleV1, u64, Loom>(dh, m.version(), addr) else { return };
        m.toplevel(&h);
        self.foreign.handles.push((addr, h.clone()));
        if let Some(o) = &self.output {
            for wo in o.client_outputs(client) {
                h.output_enter(&wo);
            }
        }
        self.send_details(&h, addr, client);
    }

    fn send_details(&self, h: &ZwlrForeignToplevelHandleV1, addr: u64, _client: &Client) {
        let Some(w) = self.managed.iter().find(|m| m.addr == addr).map(|m| m.window.clone()) else { return };
        h.title(self.title_of(&w));
        h.app_id(self.class_of(&w));
        h.state(self.foreign_state(&w));
        h.done();
    }

    fn foreign_state(&self, w: &smithay::desktop::Window) -> Vec<u8> {
        let mut st: Vec<u32> = Vec::new();
        if self.focused.as_ref() == Some(w) {
            st.push(State::Activated as u32);
        }
        if self.is_full(w) {
            st.push(State::Fullscreen as u32);
        }
        st.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    /// A new window: told to every dock.
    pub fn foreign_opened(&mut self, addr: u64) {
        let dh = self.dh.clone();
        for m in self.foreign.managers.clone() {
            if let Some(c) = m.client() {
                self.announce_to(&dh, &c, &m, addr);
            }
        }
    }

    /// A window gone: its handles closed.
    pub fn foreign_closed(&mut self, addr: u64) {
        for (_, h) in self.foreign.handles.iter().filter(|(a, _)| *a == addr) {
            h.closed();
        }
        self.foreign.handles.retain(|(a, _)| *a != addr);
    }

    /// A window's title, focus or fullscreen changed: the docks told again.
    pub fn foreign_changed(&self, addr: u64) {
        for (_, h) in self.foreign.handles.iter().filter(|(a, _)| *a == addr) {
            if let Some(c) = h.client() {
                self.send_details(h, addr, &c);
            }
        }
    }

    /// Focus moved: every window's activated state may have changed.
    pub fn foreign_all_changed(&self) {
        let addrs: Vec<u64> = self.managed.iter().map(|m| m.addr).collect();
        for a in addrs {
            self.foreign_changed(a);
        }
    }
}
