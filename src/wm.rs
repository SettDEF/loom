//! The window manager: every window with its address and workspace (as Hyprland keeps them, so
//! its IPC can answer about them), dwindle tiling, workspaces switched and windows moved
//! between them. The Space holds only the active workspace's windows.

use smithay::{
    desktop::{layer_map_for_output, Window},
    reexports::wayland_server::Resource,
    utils::{Logical, Rectangle},
};

use crate::{anim::WindowAnim, state::Loom};

/// A window loom manages.
#[derive(Clone)]
pub struct Managed {
    pub window: Window,
    /// Hyprland's address for it (0x…), stable while it lives
    pub addr: u64,
    pub ws: i32,
    pub pid: i32,
    pub floating: bool,
}

/// Hyprland's default gaps: between windows, and to the screen's edge.
const GAPS_IN: i32 = 5;
const GAPS_OUT: i32 = 10;

impl Loom {
    /// A new window: on the active workspace, tiled, focused.
    pub fn manage(&mut self, window: Window) {
        self.next_addr += 1;
        let addr = 0x5a00_0000 + self.next_addr * 0x100;
        let pid = window.toplevel().and_then(|t| t.wl_surface().client()).and_then(|c| c.get_credentials(&self.dh).ok()).map(|c| c.pid).unwrap_or(-1);
        self.managed.push(Managed { window: window.clone(), addr, ws: self.active_ws, pid, floating: false });
        self.space.map_element(window.clone(), (0, 0), true);
        self.relayout();
        self.focus_window(&window);
        self.hypr_event(&format!("openwindow>>{addr:x},{},{},{}", self.active_ws, self.class_of(&window), self.title_of(&window)));
    }

    /// A window gone: off its workspace, the rest tiled again, the focus to the next one there.
    pub fn unmanage(&mut self, window: &Window) {
        let Some(i) = self.managed.iter().position(|m| &m.window == window) else { return };
        let m = self.managed.remove(i);
        self.space.unmap_elem(window);
        if let Some(t) = window.toplevel() {
            self.anims.remove(&t.wl_surface().id());
        }
        self.hypr_event(&format!("closewindow>>{:x}", m.addr));
        if self.focused.as_ref() == Some(window) {
            self.focused = None;
            if let Some(next) = self.managed.iter().rev().find(|m| m.ws == self.active_ws).map(|m| m.window.clone()) {
                self.focus_window(&next);
            }
        }
        self.relayout();
    }

    /// The active workspace's windows, tiled in what the layers leave free.
    pub fn relayout(&mut self) {
        let Some(output) = self.output.clone() else { return };
        let Some(geo) = self.space.output_geometry(&output) else { return };
        let mut area = layer_map_for_output(&output).non_exclusive_zone();
        area.loc += geo.loc;
        // gaps as Hyprland's: the edge gets gaps_out, between two windows twice gaps_in
        let inset = GAPS_OUT - GAPS_IN;
        let area = Rectangle::new((area.loc.x + inset, area.loc.y + inset).into(), (area.size.w - 2 * inset, area.size.h - 2 * inset).into());
        let tiled: Vec<Window> = self.managed.iter().filter(|m| m.ws == self.active_ws && !m.floating).map(|m| m.window.clone()).collect();
        let suit = self.layouts.of(&self.active_ws.to_string());
        let rects: Vec<Rectangle<i32, Logical>> = crate::layouts::arrange(tiled.len(), area, &suit)
            .into_iter()
            .map(|r| Rectangle::new((r.loc.x + GAPS_IN, r.loc.y + GAPS_IN).into(), ((r.size.w - 2 * GAPS_IN).max(1), (r.size.h - 2 * GAPS_IN).max(1)).into()))
            .collect();
        let now = std::time::Instant::now();
        for (win, rect) in tiled.into_iter().zip(rects) {
            if let Some(t) = win.toplevel() {
                t.with_pending_state(|s| s.size = Some(rect.size));
                if t.is_initial_configure_sent() {
                    t.send_pending_configure();
                }
                // it glides there (a new one fades and grows in where it lands)
                let id = t.wl_surface().id();
                match self.anims.get_mut(&id) {
                    Some(a) => a.moving_to(rect.loc.x as f64, rect.loc.y as f64, now),
                    None => {
                        self.anims.insert(id, WindowAnim::opening(rect.loc.x as f64, rect.loc.y as f64));
                    }
                }
            }
            self.space.map_element(win, rect.loc, false);
        }
        self.dirty = true;
    }

    /// Workspace `n` shown (made when it was not there): its windows mapped, the others not.
    pub fn switch_workspace(&mut self, n: i32) {
        if n == self.active_ws {
            return;
        }
        let existed = self.workspace_ids().contains(&n);
        for m in self.managed.iter().filter(|m| m.ws == self.active_ws) {
            self.space.unmap_elem(&m.window);
        }
        let old = self.active_ws;
        self.active_ws = n;
        let here: Vec<Window> = self.managed.iter().filter(|m| m.ws == n).map(|m| m.window.clone()).collect();
        for w in &here {
            self.space.map_element(w.clone(), (0, 0), false);
        }
        self.relayout();
        self.focused = None;
        if let Some(w) = here.last() {
            self.focus_window(w);
        }
        if !existed {
            self.hypr_event(&format!("createworkspacev2>>{n},{n}"));
        }
        self.hypr_event(&format!("workspacev2>>{n},{n}"));
        if !self.managed.iter().any(|m| m.ws == old) {
            self.hypr_event(&format!("destroyworkspacev2>>{old},{old}"));
        }
    }

    /// The focused window to workspace `n` (it stays there; the view does not follow).
    pub fn move_to_workspace(&mut self, n: i32) {
        let Some(w) = self.focused.clone() else { return };
        let Some(m) = self.managed.iter_mut().find(|m| m.window == w) else { return };
        if m.ws == n {
            return;
        }
        m.ws = n;
        let addr = m.addr;
        self.space.unmap_elem(&w);
        self.focused = None;
        self.hypr_event(&format!("movewindowv2>>{addr:x},{n},{n}"));
        self.relayout();
    }

    /// swapmaster: the focused window first on its workspace (the master slot).
    pub fn swap_master(&mut self) {
        let Some(w) = self.focused.clone() else { return };
        let ws = self.active_ws;
        let first = self.managed.iter().position(|m| m.ws == ws && !m.floating);
        let me = self.managed.iter().position(|m| m.window == w);
        if let (Some(a), Some(b)) = (first, me) {
            self.managed.swap(a, b);
        }
    }

    /// The workspaces in use (with windows, or shown), sorted.
    pub fn workspace_ids(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self.managed.iter().map(|m| m.ws).chain([self.active_ws]).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub fn class_of(&self, w: &Window) -> String {
        w.toplevel().and_then(|t| smithay::wayland::compositor::with_states(t.wl_surface(), |s| s.data_map.get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.app_id.clone()))).unwrap_or_default()
    }
    pub fn title_of(&self, w: &Window) -> String {
        w.toplevel().and_then(|t| smithay::wayland::compositor::with_states(t.wl_surface(), |s| s.data_map.get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.title.clone()))).unwrap_or_default()
    }
    /// windowtitle(v2) for the shell; activewindow again when it is the focused one
    pub fn title_changed_of(&mut self, w: &Window) {
        let Some(addr) = self.managed_of(w).map(|m| m.addr) else { return };
        let title = self.title_of(w);
        self.hypr_event(&format!("windowtitle>>{addr:x}"));
        self.hypr_event(&format!("windowtitlev2>>{addr:x},{title}"));
        if self.focused.as_ref() == Some(w) {
            self.hypr_event(&format!("activewindow>>{},{title}", self.class_of(w)));
        }
    }
    pub fn managed_of(&self, w: &Window) -> Option<&Managed> {
        self.managed.iter().find(|m| &m.window == w)
    }
}

