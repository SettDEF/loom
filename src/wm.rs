//! The window manager: every window with its address and workspace (as Hyprland keeps them, so
//! its IPC can answer about them), dwindle tiling, workspaces switched and windows moved
//! between them. The Space holds only the active workspace's windows.

use smithay::{
    desktop::Window,
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
    /// on every workspace (pin; floating windows only, as in Hyprland)
    pub pinned: bool,
    /// where it was last shown (a hidden workspace's windows keep their place in `hyprctl clients`)
    pub last_at: smithay::utils::Point<i32, Logical>,
    /// when it was made, and whether it has drawn yet: one that never draws (zett after a restart,
    /// a 0x0 window) gives up its tile after DRAW_WAIT, and gets it back when it draws
    pub since: std::time::Instant,
    pub drawn: bool,
}

/// How long a new window keeps a tile before its first picture: laid out at once, so it draws at
/// its tile's size and opens without a jump.
pub const DRAW_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

// the gaps come from the settings (config.rs: Hyprland's general:gaps_in / gaps_out, init.lua)

/// Hyprland's id for the special workspace ("special:special").
pub const SPECIAL: i32 = -98;

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
        // started with `exec [workspace N silent]`: its window goes there (by its process or a parent)
        let ruled = self.exec_rule_for(pid);
        let ws = ruled.map(|r| r.0).unwrap_or(self.active_ws);
        if ws != self.active_ws && !ruled.is_some_and(|r| r.1) {
            self.switch_workspace(ws);
        }
        let m = Managed { window: window.clone(), addr, ws, pid, floating: float_at.is_some(), pinned: false, last_at: Default::default(), since: std::time::Instant::now(), drawn: false };
        // on a center workspace a new tiled window goes first: it becomes the master, floating in the middle
        if !m.floating && self.layouts.of(&ws.to_string()).suit == "center" { self.managed.insert(0, m) } else { self.managed.push(m) }
        // still nothing drawn by then: laid out again without it
        if let Some(h) = self.loop_handle.clone() {
            use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
            let _ = h.insert_source(Timer::from_duration(DRAW_WAIT + std::time::Duration::from_millis(100)), move |_, _, s: &mut crate::state::Loom| {
                if s.managed.iter().any(|m| m.addr == addr && !m.drawn) {
                    s.relayout();
                }
                TimeoutAction::Drop
            });
        }
        if !self.is_shown(ws) {
            self.hypr_event(&format!("openwindow>>{addr:x},{ws},{},{}", self.class_of(&window), self.title_of(&window)));
            self.foreign_opened(addr);
            return;
        }
        self.space.map_element(window.clone(), float_at.unwrap_or_default(), true);
        if let Some(at) = float_at {
            self.anims.insert(addr, WindowAnim::opening(self.fx.is_none(), at.x as f64, at.y as f64, &self.layer_cfg));
        }
        self.relayout();
        self.focus_window(&window);
        self.hypr_event(&format!("openwindow>>{addr:x},{},{},{}", self.active_ws, self.class_of(&window), self.title_of(&window)));
        // an X window has its class and title already: its rules now
        if window.x11_surface().is_some() {
            self.apply_window_rules(&window);
        }
        self.foreign_opened(addr);
    }

    /// A window gone: off its workspace, the rest tiled again, the focus to the next one there.
    pub fn unmanage(&mut self, window: &Window) {
        let Some(i) = self.managed.iter().position(|m| &m.window == window) else { return };
        // a fullscreen window closing: the bar and the rest come back
        if self.fullscreen.as_ref() == Some(window) {
            self.fullscreen = None;
            self.hypr_event("fullscreen>>0");
        }
        if self.fx.is_some() {
            self.fx_closing(window);
        } else if self.is_shown(self.managed[i].ws) {
            self.window_closing(window);
        }
        let m = self.managed.remove(i);
        self.fx_seen.remove(&m.addr);
        self.fx_last.remove(&m.addr);
        self.space.unmap_elem(window);
        self.anims.remove(&m.addr);
        crate::frames::forget(m.addr);
        self.hypr_event(&format!("closewindow>>{:x}", m.addr));
        self.foreign_closed(m.addr);
        self.decor.borrow_mut().forget(m.addr);
        self.focus_history.retain(|w| w != window);
        if self.focused.as_ref() == Some(window) {
            self.focus_next(self.active_ws);
        }
        self.relayout();
        self.pointer_refresh();
    }

    /// Each screen's workspace tiled in what its layers leave free.
    pub fn relayout(&mut self) {
        let now = std::time::Instant::now();
        for o in self.outputs.clone() {
            let (Some(area), Some(ws)) = (self.area_of(&o), self.shown_on.get(&o.name()).copied()) else { continue };
            self.tile(ws, area, false);
        }
        // mapping a tiled window puts it on top: the floating ones back over them, in their order
        let floats: Vec<Window> = self.space.elements().filter(|w| self.managed_of(w).is_some_and(|m| m.floating && m.ws != SPECIAL)).cloned().collect();
        for w in floats {
            self.space.raise_element(&w, false);
            // a floating one is not tiled (GTK draws its own shadow and corners then)
            if let Some(t) = w.toplevel() {
                t.with_pending_state(|s| set_tiled(s, false));
                if t.is_initial_configure_sent() {
                    t.send_pending_configure();
                }
            }
        }
        // the special workspace over the focused screen (Super+S), as Hyprland draws it
        if let (true, Some(o)) = (self.special_shown, self.output.clone()) {
            if let Some(area) = self.area_of(&o) {
                self.tile(SPECIAL, area, true);
            }
        }
        // a fullscreen window: the whole screen showing its workspace, above the rest
        let full_ws = self.fullscreen.as_ref().and_then(|f| self.managed_of(f)).map(|m| m.ws);
        if let (Some(f), Some(o)) = (self.fullscreen.clone(), full_ws.and_then(|ws| self.output_showing(ws))) {
            let Some(full) = self.space.output_geometry(&o) else { return };
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
        self.x_sync_stacking();
        self.x_scales();
        self.dirty = true;
    }

    /// One workspace's tiled windows in `area`; `raise` puts them on top (the special one).
    fn tile(&mut self, ws: i32, area: Rectangle<i32, Logical>, raise: bool) {
        let tiled: Vec<(Window, u64)> = self.managed.iter().filter(|m| m.ws == ws && !m.floating && self.fullscreen.as_ref() != Some(&m.window) && (m.drawn || m.since.elapsed() < DRAW_WAIT)).map(|m| (m.window.clone(), m.addr)).collect();
        let suit = self.layouts.of(&ws.to_string());
        let rects: Vec<Rectangle<i32, Logical>> = crate::layouts::arrange(tiled.len(), area, &suit)
            .into_iter()
            .map(|r| { let g = self.look.gaps_in.round() as i32; Rectangle::new((r.loc.x + g, r.loc.y + g).into(), ((r.size.w - 2 * g).max(1), (r.size.h - 2 * g).max(1)).into()) })
            .collect();
        let now = std::time::Instant::now();
        let maxed = self.maximized.clone();
        // center: the master (first) floats over the others, so it is raised; the rest stay under it
        let center = suit.suit == "center";
        for (i, ((win, addr), rect)) in tiled.into_iter().zip(rects).enumerate() {
            // maximized: the whole area; a title bar (ii-decor) takes its top, the window the rest
            let rect = if maxed.as_ref() == Some(&win) {
                let g = self.look.gaps_in.round() as i32;
                Rectangle::new((area.loc.x + g, area.loc.y + g).into(), ((area.size.w - 2 * g).max(1), (area.size.h - 2 * g).max(1)).into())
            } else {
                rect
            };
            let raise = raise || maxed.as_ref() == Some(&win) || (center && i == 0);
            let bh = self.bar_height(&win).round() as i32;
            let rect = Rectangle::new((rect.loc.x, rect.loc.y + bh).into(), (rect.size.w, (rect.size.h - bh).max(1)).into());
            if let Some(t) = win.toplevel() {
                t.with_pending_state(|s| {
                    s.size = Some(rect.size);
                    s.states.unset(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen);
                    set_tiled(s, true);
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
                Some(a) => {
                    if std::env::var_os("LOOM_DEBUG_ANIM").is_some() {
                        eprintln!("loom: anim {addr:x} moves to {:?} {:?} (waiting {})", rect.loc, rect.size, a.waiting);
                    }
                    // not drawn yet: straight to its place (a correction before it shows is no glide)
                    if a.waiting {
                        a.jump_to(rect.loc.x as f64, rect.loc.y as f64);
                    } else {
                        a.moving_to(rect.loc.x as f64, rect.loc.y as f64, now);
                    }
                }
                None => {
                    if std::env::var_os("LOOM_DEBUG_ANIM").is_some() {
                        eprintln!("loom: anim {addr:x} opens at {:?} {:?}", rect.loc, rect.size);
                    }
                    self.anims.insert(addr, WindowAnim::opening(self.fx.is_none(), rect.loc.x as f64, rect.loc.y as f64, &self.layer_cfg));
                }
            }
            self.space.map_element(win, rect.loc, raise);
        }
    }
    /// Workspace `n` shown, as Hyprland does it on several screens: shown on another screen
    /// already, that screen gets the focus (and the pointer); one that belongs to another screen
    /// is shown there; else the focused screen shows it (made when it was not there).
    pub fn switch_workspace(&mut self, n: i32) {
        if n == self.active_ws {
            return;
        }
        if let Some(o) = self.output_showing(n) {
            self.focus_output(&o);
            self.pointer_to_middle(&o);
            self.focus_next(n);
            self.hypr_event(&format!("workspacev2>>{n},{n}"));
            self.pointer_refresh();
            return;
        }
        if let Some(o) = self.ws_on.get(&n).and_then(|name| self.output_named(name)).filter(|o| Some(o) != self.output.as_ref()) {
            self.focus_output(&o);
            self.pointer_to_middle(&o);
        }
        let existed = self.workspace_ids().contains(&n);
        for m in self.managed.iter_mut().filter(|m| m.ws == self.active_ws && !m.pinned) {
            if let Some(at) = self.space.element_location(&m.window) {
                m.last_at = at;
            }
            self.space.unmap_elem(&m.window);
            suspend(&m.window, true);
        }
        let old = self.active_ws;
        self.active_ws = n;
        if let Some(name) = self.output.as_ref().map(|o| o.name()) {
            self.shown_on.insert(name.clone(), n);
            self.ws_on.insert(n, name);
        }
        // pinned windows come along
        for m in self.managed.iter_mut().filter(|m| m.pinned) {
            m.ws = n;
        }
        let here: Vec<Window> = self.managed.iter().filter(|m| m.ws == n).map(|m| m.window.clone()).collect();
        for w in &here {
            self.space.map_element(w.clone(), (0, 0), false);
            suspend(w, false);
        }
        self.relayout();
        self.focus_next(n);
        if !existed {
            self.hypr_event(&format!("createworkspacev2>>{n},{n}"));
        }
        self.hypr_event(&format!("workspacev2>>{n},{n}"));
        if !self.managed.iter().any(|m| m.ws == old) && !self.is_shown(old) {
            self.ws_on.remove(&old);
            self.hypr_event(&format!("destroyworkspacev2>>{old},{old}"));
        }
        // what is under the still pointer now: the hidden window is told it left, the shown one
        // that it came (Firefox took no click after a switch until a panel took the pointer)
        self.pointer_refresh();
    }

    /// The pointer to the middle of screen `o` (focus moved there by a key).
    pub fn pointer_to_middle(&mut self, o: &smithay::output::Output) {
        let Some(g) = self.space.output_geometry(o) else { return };
        let mid = (g.loc.x as f64 + g.size.w as f64 / 2.0, g.loc.y as f64 + g.size.h as f64 / 2.0);
        let t = self.now_ms();
        self.pointer_move(mid.into(), t);
    }

    /// The focused window to workspace `n` (it stays there; the view does not follow).
    pub fn move_to_workspace(&mut self, n: i32) {
        if let Some(w) = self.focused.clone() {
            self.move_window_to_workspace(&w, n);
        }
    }

    /// Window `w` to workspace `n`: hidden when no screen shows `n`, else laid out on the screen
    /// that does (the other monitor). It was always hidden, so one moved to the workspace on the
    /// other screen vanished.
    pub fn move_window_to_workspace(&mut self, w: &Window, n: i32) {
        let was_focused = self.focused.as_ref() == Some(w);
        let Some(m) = self.managed.iter_mut().find(|m| &m.window == w) else { return };
        if m.ws == n {
            return;
        }
        m.ws = n;
        if let Some(at) = self.space.element_location(w) {
            m.last_at = at;
        }
        let addr = m.addr;
        if self.is_shown(n) {
            if self.space.element_location(w).is_none() {
                self.space.map_element(w.clone(), (0, 0), false);
            }
            suspend(w, false);
        } else {
            self.space.unmap_elem(w);
            suspend(w, true);
        }
        self.hypr_event(&format!("movewindowv2>>{addr:x},{n},{n}"));
        self.relayout();
        // the focus to the next window here, as Hyprland does (none left: off the moved one)
        if was_focused && !self.is_shown(n) {
            self.focus_next(self.active_ws);
        }
        self.pointer_refresh();
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
        let mut ids: Vec<i32> = self.managed.iter().map(|m| m.ws).chain([self.active_ws]).chain(self.shown_on.values().copied()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub fn class_of(&self, w: &Window) -> String {
        if let Some(x) = w.x11_surface() {
            // an X program that names itself only in the first half (Bitwig: "com.bitwig.BitwigStudio",
            // no class) is known by that, not as ""
            let c = x.class();
            return if c.is_empty() { x.instance() } else { c };
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
        self.foreign_changed(addr);
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

    /// The focus to the window last used on workspace `ws` (else its newest one); none there: the
    /// keyboard to nothing, so keys never go to a window that is gone or on a hidden workspace.
    pub fn focus_next(&mut self, ws: i32) {
        self.focused = None;
        let alive = |w: &Window| smithay::utils::IsAlive::alive(w);
        let next = self
            .focus_history
            .iter()
            .rev()
            .find(|w| alive(w) && self.managed_of(w).is_some_and(|m| m.ws == ws))
            .cloned()
            .or_else(|| self.managed.iter().rev().find(|m| m.ws == ws && alive(&m.window)).map(|m| m.window.clone()));
        match next {
            Some(w) => self.focus_window(&w),
            None => self.focus_nothing(),
        }
    }

    /// No window has the focus: the keyboard off it (a panel that asked for the keyboard keeps it).
    pub fn focus_nothing(&mut self) {
        self.focused = None;
        let kb = self.seat.get_keyboard().expect("keyboard");
        let on_panel = kb.current_focus().and_then(|f| f.surface()).is_some_and(|s| {
            smithay::reexports::wayland_server::Resource::is_alive(&s) && self.layer_kb_wanted.contains(&smithay::reexports::wayland_server::Resource::id(&s))
        });
        if !on_panel && !self.locked() {
            kb.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial());
        }
        self.hypr_event("activewindow>>,");
        self.hypr_event("activewindowv2>>");
        self.foreign_all_changed();
    }

    /// fullscreen: the focused window over the whole output, or back into the tiling.
    pub fn toggle_fullscreen(&mut self) {
        let Some(w) = self.focused.clone() else { return };
        let on = self.fullscreen.as_ref() != Some(&w);
        self.set_fullscreen(&w, on);
    }

    /// `w` fullscreen or not: from a bind, or the program asking (F11, a video's button, a game).
    pub fn set_fullscreen(&mut self, w: &Window, on: bool) {
        if on == (self.fullscreen.as_ref() == Some(w)) {
            return;
        }
        if on {
            self.fullscreen = Some(w.clone());
            self.focus_window(w);
        } else {
            self.fullscreen = None;
        }
        self.hypr_event(&format!("fullscreen>>{}", u8::from(on)));
        self.foreign_all_changed();
        self.relayout();
    }
}

impl Loom {
    /// The window nearest the focused one in direction `dir` (l r u d) on the workspace.
    fn neighbour(&self, dir: &str) -> Option<Window> {
        let cur = self.focused.clone()?;
        let centre = |w: &Window| self.space.element_geometry(w).map(|g| (g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2));
        let (cx, cy) = centre(&cur)?;
        let (dx, dy) = match dir.trim() {
            "l" | "left" => (-1, 0),
            "r" | "right" => (1, 0),
            "u" | "up" | "t" | "top" => (0, -1),
            _ => (0, 1),
        };
        self.managed
            .iter()
            .filter(|m| m.ws == self.active_ws && m.window != cur)
            .filter_map(|m| centre(&m.window).map(|c| (m.window.clone(), c)))
            .filter(|(_, (x, y))| (x - cx) * dx + (y - cy) * dy > 0)
            .min_by_key(|(_, (x, y))| ((x - cx) * dx + (y - cy) * dy).abs() + 2 * ((x - cx) * dy + (y - cy) * dx).abs())
            .map(|(w, _)| w)
    }

    /// movewindow l|r|u|d: a tiled window trades places with its neighbour; a floating one moves.
    pub fn move_window(&mut self, dir: &str) {
        let Some(cur) = self.focused.clone() else { return };
        let Some(i) = self.managed.iter().position(|m| m.window == cur) else { return };
        if self.managed[i].floating {
            let (dx, dy) = match dir.trim() {
                "l" => (-40, 0),
                "r" => (40, 0),
                "u" => (0, -40),
                _ => (0, 40),
            };
            if let Some(loc) = self.space.element_location(&cur) {
                let to: smithay::utils::Point<i32, Logical> = (loc.x + dx, loc.y + dy).into();
                self.space.map_element(cur.clone(), to, true);
                if let Some(x) = cur.x11_surface() {
                    let _ = x.configure(Rectangle::new(to, x.geometry().size));
                }
                self.dirty = true;
            }
            return;
        }
        let Some(other) = self.neighbour(dir) else { return };
        let Some(j) = self.managed.iter().position(|m| m.window == other) else { return };
        self.managed.swap(i, j);
        self.relayout();
    }

    /// pin: a floating window on every workspace, or not.
    pub fn toggle_pin(&mut self) {
        let Some(cur) = self.focused.clone() else { return };
        if let Some(m) = self.managed.iter_mut().find(|m| m.window == cur && m.floating) {
            m.pinned = !m.pinned;
        }
    }

    /// resizeactive exact W H | DX DY: a floating window's size.
    pub fn resize_active(&mut self, arg: &str) {
        let Some(cur) = self.focused.clone() else { return };
        if !self.managed_of(&cur).is_some_and(|m| m.floating) {
            return;
        }
        let parts: Vec<&str> = arg.split_whitespace().collect();
        let size = cur.geometry().size;
        let (w, h) = match parts.as_slice() {
            ["exact", w, h] => (w.parse().unwrap_or(size.w), h.parse().unwrap_or(size.h)),
            [dw, dh] => (size.w + dw.parse::<i32>().unwrap_or(0), size.h + dh.parse::<i32>().unwrap_or(0)),
            _ => return,
        };
        let (w, h) = (w.max(20), h.max(20));
        if let Some(t) = cur.toplevel() {
            t.with_pending_state(|s| s.size = Some((w, h).into()));
            t.send_pending_configure();
        } else if let Some(x) = cur.x11_surface() {
            let loc = self.space.element_location(&cur).unwrap_or_default();
            let _ = x.configure(Rectangle::new(loc, (w, h).into()));
        }
    }

    /// togglespecialworkspace: the special workspace shown over the active one, or hidden.
    pub fn toggle_special(&mut self) {
        self.special_shown = !self.special_shown;
        let here: Vec<Window> = self.managed.iter().filter(|m| m.ws == SPECIAL).map(|m| m.window.clone()).collect();
        for w in &here {
            if self.special_shown {
                self.space.map_element(w.clone(), (0, 0), true);
            } else {
                self.space.unmap_elem(w);
            }
        }
        self.relayout();
        let mon = self.output.as_ref().map(|o| o.name()).unwrap_or_default();
        if self.special_shown {
            if let Some(w) = here.last() {
                self.focus_window(w);
            }
            self.hypr_event(&format!("activespecial>>special:special,{mon}"));
        } else {
            self.focus_next(self.active_ws);
            self.hypr_event(&format!("activespecial>>,{mon}"));
        }
    }

    /// sendshortcut MODS, KEY, TARGET: the key combination to the focused program, past the binds
    /// (vim mode's h → Left).
    pub fn send_shortcut(&mut self, arg: &str) {
        let mut p = arg.splitn(3, ',').map(str::trim);
        let (mods, key) = (p.next().unwrap_or(""), p.next().unwrap_or(""));
        let Some(code) = evdev_of(key) else { return };
        let held: Vec<u32> = [("CTRL", 29), ("CONTROL", 29), ("SHIFT", 42), ("ALT", 56), ("SUPER", 125)]
            .into_iter()
            .filter(|(n, _)| mods.to_ascii_uppercase().split(|c: char| !c.is_ascii_alphabetic()).any(|m| m == *n))
            .map(|(_, c)| c)
            .collect();
        let time = self.now_ms();
        let kb = self.seat.get_keyboard().expect("keyboard");
        let send = |s: &mut Loom, c: u32, st: smithay::backend::input::KeyState| {
            kb.input::<(), _>(s, smithay::input::keyboard::Keycode::new(c + 8), st, smithay::utils::SERIAL_COUNTER.next_serial(), time, |_, _, _| smithay::input::keyboard::FilterResult::Forward);
        };
        use smithay::backend::input::KeyState::{Pressed, Released};
        for &h in &held {
            send(self, h, Pressed);
        }
        send(self, code, Pressed);
        send(self, code, Released);
        for &h in held.iter().rev() {
            send(self, h, Released);
        }
    }
}

/// A key name (as binds write them) to its evdev code.
fn evdev_of(name: &str) -> Option<u32> {
    let n = name.trim();
    let named = match n.to_ascii_lowercase().as_str() {
        "left" => 105,
        "right" => 106,
        "up" => 103,
        "down" => 108,
        "home" => 102,
        "end" => 107,
        "prior" | "pageup" | "page_up" => 104,
        "next" | "pagedown" | "page_down" => 109,
        "delete" => 111,
        "insert" => 110,
        "backspace" => 14,
        "return" | "enter" => 28,
        "tab" => 15,
        "escape" => 1,
        "space" => 57,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    let c = n.chars().next()?;
    if n.chars().count() != 1 {
        return None;
    }
    const LETTERS: &str = "qwertyuiopasdfghjklzxcvbnm";
    const CODES: [u32; 26] = [16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 30, 31, 32, 33, 34, 35, 36, 37, 38, 44, 45, 46, 47, 48, 49, 50];
    if let Some(i) = LETTERS.find(c.to_ascii_lowercase()) {
        return Some(CODES[i]);
    }
    "1234567890".find(c).map(|i| 2 + i as u32)
}

impl Loom {
    /// Whether a window is told to draw its next frame: not when a fullscreen window covers it
    /// (it cannot be seen; its program idles instead of drawing for nothing).
    pub fn wants_frame(&self, w: &Window) -> bool {
        match &self.fullscreen {
            Some(f) if self.managed_of(f).is_some_and(|m| m.ws == self.active_ws) => f == w || self.managed_of(w).is_none(),
            _ => true,
        }
    }
}

impl Loom {
    /// The `exec [workspace N silent]` rule for a window's process (it or one of its parents was
    /// started so): (workspace, silent), used up.
    fn exec_rule_for(&mut self, pid: i32) -> Option<(i32, bool)> {
        if self.exec_rules.is_empty() || pid <= 0 {
            return None;
        }
        let mut p = pid as u32;
        for _ in 0..6 {
            if let Some(i) = self.exec_rules.iter().position(|r| r.0 == p) {
                let (_, ws, silent) = self.exec_rules.remove(i);
                return Some((ws, silent));
            }
            let stat = std::fs::read_to_string(format!("/proc/{p}/stat")).ok()?;
            p = stat[stat.rfind(')')? + 2..].split_whitespace().nth(1)?.parse().ok()?;
            if p <= 1 {
                break;
            }
        }
        None
    }
}

impl Loom {
    /// The area the tiling fills (what the layers leave, less Hyprland's outer gaps).
    pub fn tile_area(&self) -> Option<Rectangle<i32, Logical>> {
        self.area_of(self.output.as_ref()?)
    }

    /// The tiled window whose layout slot holds `at` (counting `with` as tiled), by the layout
    /// itself rather than where the windows are drawn right now.
    pub fn tile_slot_at(&self, at: smithay::utils::Point<f64, Logical>, with: &Window) -> Option<Window> {
        let area = self.tile_area()?;
        let tiled: Vec<Window> = self.managed.iter().filter(|m| m.ws == self.active_ws && (!m.floating || &m.window == with)).map(|m| m.window.clone()).collect();
        let suit = self.layouts.of(&self.active_ws.to_string());
        let rects = crate::layouts::arrange(tiled.len(), area, &suit);
        tiled.into_iter().zip(rects).find(|(_, r)| r.to_f64().contains(at)).map(|(w, _)| w)
    }
}

/// A tiled window is told so on all four sides, as Hyprland does: GTK (Zen, Firefox) then drops the
/// shadow, rounded corners and outline it draws round a floating window, and fills its tile.
fn set_tiled(s: &mut smithay::wayland::shell::xdg::ToplevelState, on: bool) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
    for st in [State::TiledLeft, State::TiledRight, State::TiledTop, State::TiledBottom] {
        if on {
            s.states.set(st);
        } else {
            s.states.unset(st);
        }
    }
}

/// A window hidden on another workspace is told so (xdg_toplevel suspended, as GNOME does): GTK
/// and WebKit stop drawing then; shown again, it is told it is back. X windows are left alone.
fn suspend(w: &Window, on: bool) {
    let Some(t) = w.toplevel() else { return };
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
    let changed = t.with_pending_state(|s| if on { s.states.set(State::Suspended) } else { s.states.unset(State::Suspended) });
    if changed && t.is_initial_configure_sent() {
        t.send_pending_configure();
    }
}

impl Loom {
    /// X's stacking order made the one loom draws: floating windows over tiled ones, then each X
    /// window raised in X from the bottom up. X clips a window's content where it thinks another
    /// covers it: with Bitwig's main window "above" its floating plugin window in X, the plugin's
    /// GPU-drawn content came out black wherever the two overlapped.
    pub fn x_sync_stacking(&mut self) {
        let floats: Vec<Window> = self.space.elements().filter(|w| self.managed_of(w).is_some_and(|m| m.floating && m.ws != SPECIAL)).cloned().collect();
        for w in &floats {
            self.space.raise_element(w, false);
        }
        let xs: Vec<smithay::xwayland::X11Surface> = self.space.elements().filter_map(|w| w.x11_surface().cloned()).collect();
        if let Some(wm) = self.xwm.as_mut() {
            for x in &xs {
                let _ = wm.raise_window(x);
            }
        }
    }
}

impl Loom {
    /// center: a window behind the floating master that takes the focus comes to the front, nothing moves
    /// (swapping it into the master moved windows on every focus change and confused the shell)
    pub fn center_master_on_top(&mut self) {
        let ws = self.active_ws;
        if self.layouts.of(&ws.to_string()).suit != "center" {
            return;
        }
        let Some(w) = self.focused.clone() else { return };
        if !self.managed.iter().any(|m| m.window == w && m.ws == ws && !m.floating) {
            return;
        }
        self.space.raise_element(&w, false);
        let floats: Vec<Window> = self.managed.iter().filter(|m| m.ws == ws && m.floating).map(|m| m.window.clone()).collect();
        for f in floats {
            self.space.raise_element(&f, false);
        }
        // a fullscreen window stays over all of it: raised under it, a window took its clicks (it was drawn on top
        // but no longer first in the stack the pointer looks through)
        if let Some(full) = self.fullscreen.clone().filter(|f| self.managed_of(f).is_some_and(|m| m.ws == ws)) {
            self.space.raise_element(&full, false);
        }
        self.dirty = true;
    }
}
