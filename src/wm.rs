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
        self.manage_at(window, None);
    }

    /// A new floating window, at `loc` (an X dialog).
    pub fn manage_floating(&mut self, window: Window, loc: smithay::utils::Point<i32, Logical>) {
        self.manage_at(window, Some(loc));
    }

    fn manage_at(&mut self, window: Window, float_at: Option<smithay::utils::Point<i32, Logical>>) {
        self.next_addr += 1;
        let addr = 0x5a00_0000 + self.next_addr * 0x100;
        let pid = match window.x11_surface() {
            Some(x) => x.pid().map(|p| p as i32).unwrap_or(-1),
            None => window.toplevel().and_then(|t| t.wl_surface().client()).and_then(|c| c.get_credentials(&self.dh).ok()).map(|c| c.pid).unwrap_or(-1),
        };
        self.managed.push(Managed { window: window.clone(), addr, ws: self.active_ws, pid, floating: float_at.is_some() });
        self.space.map_element(window.clone(), float_at.unwrap_or_default(), true);
        if let Some(at) = float_at {
            self.anims.insert(addr, WindowAnim::opening(at.x as f64, at.y as f64));
        }
        self.relayout();
        self.focus_window(&window);
        self.hypr_event(&format!("openwindow>>{addr:x},{},{},{}", self.active_ws, self.class_of(&window), self.title_of(&window)));
    }

    /// A window gone: off its workspace, the rest tiled again, the focus to the next one there.
    pub fn unmanage(&mut self, window: &Window) {
        let Some(i) = self.managed.iter().position(|m| &m.window == window) else { return };
        let m = self.managed.remove(i);
        self.space.unmap_elem(window);
        self.anims.remove(&m.addr);
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
        let tiled: Vec<(Window, u64)> = self.managed.iter().filter(|m| m.ws == self.active_ws && !m.floating && self.fullscreen.as_ref() != Some(&m.window)).map(|m| (m.window.clone(), m.addr)).collect();
        let suit = self.layouts.of(&self.active_ws.to_string());
        let rects: Vec<Rectangle<i32, Logical>> = crate::layouts::arrange(tiled.len(), area, &suit)
            .into_iter()
            .map(|r| Rectangle::new((r.loc.x + GAPS_IN, r.loc.y + GAPS_IN).into(), ((r.size.w - 2 * GAPS_IN).max(1), (r.size.h - 2 * GAPS_IN).max(1)).into()))
            .collect();
        let now = std::time::Instant::now();
        for ((win, addr), rect) in tiled.into_iter().zip(rects) {
            if let Some(t) = win.toplevel() {
                t.with_pending_state(|s| {
                    s.size = Some(rect.size);
                    s.states.unset(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen);
                });
                if t.is_initial_configure_sent() {
                    t.send_pending_configure();
                }
            } else if let Some(x) = win.x11_surface() {
                if x.is_fullscreen() {
                    let _ = x.set_fullscreen(false);
                }
                let _ = x.configure(rect);
            }
            // it glides there (a new one fades and grows in where it lands)
            match self.anims.get_mut(&addr) {
                Some(a) => a.moving_to(rect.loc.x as f64, rect.loc.y as f64, now),
                None => {
                    self.anims.insert(addr, WindowAnim::opening(rect.loc.x as f64, rect.loc.y as f64));
                }
            }
            self.space.map_element(win, rect.loc, false);
        }
        // a fullscreen window: the whole output, above the rest
        if let Some(f) = self.fullscreen.clone().filter(|f| self.managed_of(f).is_some_and(|m| m.ws == self.active_ws)) {
            let full = geo;
            if let Some(t) = f.toplevel() {
                t.with_pending_state(|s| {
                    s.size = Some(full.size);
                    s.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen);
                });
                t.send_pending_configure();
            } else if let Some(x) = f.x11_surface() {
                let _ = x.set_fullscreen(true);
                let _ = x.configure(full);
            }
            self.space.map_element(f.clone(), full.loc, true);
            if let Some(a) = self.managed_of(&f).map(|m| m.addr).and_then(|addr| self.anims.get_mut(&addr)) {
                a.moving_to(full.loc.x as f64, full.loc.y as f64, now);
            }
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
        if let Some(x) = w.x11_surface() {
            return x.class();
        }
        w.toplevel().and_then(|t| smithay::wayland::compositor::with_states(t.wl_surface(), |s| s.data_map.get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>().and_then(|d| d.lock().ok()?.app_id.clone()))).unwrap_or_default()
    }
    pub fn title_of(&self, w: &Window) -> String {
        if let Some(x) = w.x11_surface() {
            return x.title();
        }
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


impl Loom {
    /// movefocus l|r|u|d: the nearest window that way on the workspace (by centres).
    pub fn move_focus(&mut self, dir: &str) {
        let Some(cur) = self.focused.clone() else { return };
        let centre = |w: &Window| self.space.element_geometry(w).map(|g| (g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2));
        let Some((cx, cy)) = centre(&cur) else { return };
        let (dx, dy) = match dir.trim() {
            "l" | "left" => (-1, 0),
            "r" | "right" => (1, 0),
            "u" | "up" | "t" | "top" => (0, -1),
            _ => (0, 1),
        };
        let best = self
            .managed
            .iter()
            .filter(|m| m.ws == self.active_ws && m.window != cur)
            .filter_map(|m| centre(&m.window).map(|c| (m.window.clone(), c)))
            .filter(|(_, (x, y))| (x - cx) * dx + (y - cy) * dy > 0)
            .min_by_key(|(_, (x, y))| {
                let along = ((x - cx) * dx + (y - cy) * dy).abs();
                let across = ((x - cx) * dy + (y - cy) * dx).abs();
                along + 2 * across
            })
            .map(|(w, _)| w);
        if let Some(w) = best {
            self.focus_window(&w);
        }
    }

    /// cyclenext / cyclenext prev: the next window on the workspace, round.
    pub fn cycle_focus(&mut self, forward: bool) {
        let here: Vec<Window> = self.managed.iter().filter(|m| m.ws == self.active_ws).map(|m| m.window.clone()).collect();
        if here.is_empty() {
            return;
        }
        let i = self.focused.as_ref().and_then(|f| here.iter().position(|w| w == f)).unwrap_or(0);
        let n = here.len();
        let next = if forward { (i + 1) % n } else { (i + n - 1) % n };
        self.focus_window(&here[next].clone());
    }

    /// fullscreen: the focused window over the whole output, or back into the tiling.
    pub fn toggle_fullscreen(&mut self) {
        let Some(w) = self.focused.clone() else { return };
        let on = self.fullscreen.as_ref() != Some(&w);
        self.fullscreen = on.then(|| w.clone());
        self.relayout();
    }
}
