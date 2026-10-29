//! Several screens, Hyprland's way: each shows its own workspace, a workspace belongs to the
//! screen it was first shown on, the focused screen is the one the pointer (or the focus) is on,
//! and `active_ws` is that screen's workspace.

use smithay::{
    desktop::layer_map_for_output,
    output::Output,
    utils::{Logical, Point, Rectangle},
};

use crate::state::Loom;

impl Loom {
    /// A screen joins at `at` (logical): it shows the active workspace if it is the first, else
    /// the lowest one no screen shows.
    pub fn add_output(&mut self, o: Output, at: Point<i32, Logical>) {
        self.space.map_output(&o, at);
        o.change_current_state(None, None, None, Some(at));
        self.outputs.push(o.clone());
        self.kde_outputs_changed();
        let ws = if self.shown_on.is_empty() { self.active_ws } else { (1..).find(|n| !self.shown_on.values().any(|v| v == n) && self.ws_on.get(n).is_none_or(|m| *m == o.name())).unwrap_or(1) };
        self.shown_on.insert(o.name(), ws);
        self.ws_on.insert(ws, o.name());
        if self.output.is_none() {
            self.output = Some(o.clone());
            self.active_ws = ws;
        }
        let here: Vec<_> = self.managed.iter().filter(|m| m.ws == ws).map(|m| m.window.clone()).collect();
        for w in here {
            self.show_window(&w, false);
        }
        eprintln!("loom: screen {} at {},{} shows workspace {ws}", o.name(), at.x, at.y);
        self.hypr_event(&format!("monitoradded>>{}", o.name()));
        self.x_layout_apply();
        self.relayout();
        self.greeter_snapshot();
    }

    /// A screen gone (unplugged): its workspaces move to the focused screen (or the first).
    pub fn remove_output(&mut self, name: &str) {
        let Some(i) = self.outputs.iter().position(|o| o.name() == name) else { return };
        let o = self.outputs.remove(i);
        self.kde_outputs_changed();
        // its layers (bar, dock …) close; the shell makes new ones where it wants them
        let layers: Vec<_> = layer_map_for_output(&o).layers().cloned().collect();
        for l in layers {
            l.layer_surface().send_close();
            layer_map_for_output(&o).unmap_layer(&l);
        }
        self.space.unmap_output(&o);
        let shown = self.shown_on.remove(name);
        if self.output.as_ref() == Some(&o) {
            self.output = self.outputs.first().cloned();
        }
        let to = self.output.as_ref().map(|o| o.name()).unwrap_or_default();
        for v in self.ws_on.values_mut().filter(|v| v.as_str() == name) {
            *v = to.clone();
        }
        // its windows were shown: hidden now (their workspace is not shown anywhere)
        if let Some(ws) = shown {
            let here: Vec<_> = self.managed.iter().filter(|m| m.ws == ws).map(|m| m.window.clone()).collect();
            for w in here {
                self.hide_window(&w);
            }
        }
        if let Some(f) = self.output.clone() {
            self.active_ws = self.shown_on.get(&f.name()).copied().unwrap_or(1);
        }
        // the focus was on a window of that screen: it is hidden now, so keys went to nothing visible
        if self.focused.as_ref().and_then(|w| self.managed_of(w)).is_some_and(|m| !self.is_shown(m.ws)) {
            self.focus_next(self.active_ws);
        }
        eprintln!("loom: screen {name} gone");
        self.hypr_event(&format!("monitorremoved>>{name}"));
        self.x_layout_apply();
        self.relayout();
    }

    pub fn output_named(&self, name: &str) -> Option<Output> {
        self.outputs.iter().find(|o| o.name() == name).cloned()
    }

    /// Workspace `ws` is on some screen now.
    pub fn is_shown(&self, ws: i32) -> bool {
        // the special workspace is shown over another, not by a screen of its own
        if ws == crate::wm::SPECIAL {
            return self.special_shown;
        }
        self.shown_on.values().any(|v| *v == ws)
    }

    /// The screen showing `ws`, if one does.
    pub fn output_showing(&self, ws: i32) -> Option<Output> {
        self.shown_on.iter().find(|(_, v)| **v == ws).and_then(|(n, _)| self.output_named(n))
    }

    /// The screen under `p` (compositor space).
    pub fn output_at(&self, p: Point<f64, Logical>) -> Option<Output> {
        self.outputs.iter().find(|o| self.space.output_geometry(o).is_some_and(|g| g.to_f64().contains(p))).cloned()
    }

    /// `p` kept on some screen (the nearest edge of the nearest one when it left them all).
    pub fn clamp_to_outputs(&self, p: Point<f64, Logical>) -> Point<f64, Logical> {
        if self.output_at(p).is_some() {
            return p;
        }
        let near = self
            .outputs
            .iter()
            .filter_map(|o| self.space.output_geometry(o))
            .map(|g| {
                let g = g.to_f64();
                let q: Point<f64, Logical> = (p.x.clamp(g.loc.x, g.loc.x + g.size.w - 1.0), p.y.clamp(g.loc.y, g.loc.y + g.size.h - 1.0)).into();
                ((q.x - p.x).powi(2) + (q.y - p.y).powi(2), q)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        near.map(|n| n.1).unwrap_or(p)
    }

    /// The focus to screen `o`: its workspace becomes the active one.
    pub fn focus_output(&mut self, o: &Output) {
        if self.output.as_ref() == Some(o) {
            return;
        }
        self.output = Some(o.clone());
        self.active_ws = self.shown_on.get(&o.name()).copied().unwrap_or(self.active_ws);
        self.hypr_event(&format!("focusedmon>>{},{}", o.name(), self.active_ws));
        self.hypr_event(&format!("focusedmonv2>>{},{}", o.name(), self.active_ws));
    }

    /// What the tiling fills on screen `o`: what its layers leave, less Hyprland's outer gaps.
    pub fn area_of(&self, o: &Output) -> Option<Rectangle<i32, Logical>> {
        let geo = self.space.output_geometry(o)?;
        let mut area = layer_map_for_output(o).non_exclusive_zone();
        area.loc += geo.loc;
        let (gin, gout) = (self.look.gaps_in.round() as i32, self.look.gaps_out.round() as i32);
        let inset = gout - gin;
        Some(Rectangle::new((area.loc.x + inset, area.loc.y + inset).into(), (area.size.w - 2 * inset, area.size.h - 2 * inset).into()))
    }

    /// The screen a layer surface is on, and the layer.
    pub fn layer_output_of(&self, s: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> Option<(Output, smithay::desktop::LayerSurface)> {
        self.outputs.iter().find_map(|o| {
            let l = layer_map_for_output(o).layer_for_surface(s, smithay::desktop::WindowSurfaceType::TOPLEVEL).cloned()?;
            Some((o.clone(), l))
        })
    }
}

impl Loom {
    /// Frame callbacks for every screen's windows and layers (and the lock screen).
    /// Frame callbacks for one screen (its refresh came): only to what is on it. Sent to every
    /// screen's surfaces at every screen's refresh, an animating shell panel drew at 180 + 60 Hz
    /// (Quickshell sent ~20 000 buffers a second, 2026-10-23).
    pub fn frames_to_output(&mut self, o: &smithay::output::Output, ms: std::time::Duration) {
        let zero = Some(std::time::Duration::ZERO);
        for w in self.space.elements() {
            if !self.wants_frame(w) {
                continue;
            }
            let on = self.space.outputs_for_element(w);
            if on.contains(o) || (on.is_empty() && self.output.as_ref() == Some(o)) {
                // the scale of the screen it is shown on (and its menus')
                let scale = self.window_scale(w).unwrap_or_else(|| o.current_scale().fractional_scale());
                if let Some(s) = smithay::wayland::seat::WaylandFocus::wl_surface(w) {
                    crate::scale::set_tree_scale(&s, scale);
                    for (p, _) in smithay::desktop::PopupManager::popups_for_surface(&s) {
                        crate::scale::set_tree_scale(p.wl_surface(), scale);
                    }
                }
                w.send_frame(o, ms, zero, |_, _| Some(o.clone()));
            }
        }
        for l in layer_map_for_output(o).layers() {
            crate::scale::set_tree_scale(l.wl_surface(), o.current_scale().fractional_scale());
            l.send_frame(o, ms, zero, |_, _| Some(o.clone()));
        }
        if self.output.as_ref() == Some(o) {
            self.lock_frame(o, ms);
            self.cursor_frame(o, ms);
        }
        // windows on hidden workspaces: a frame callback once a second (as niri), or a program
        // waiting for its next frame stalls, and Qt's unanswered requests fill its socket
        if self.output.as_ref() == Some(o) && self.hidden_frames_at.elapsed() >= std::time::Duration::from_secs(1) {
            self.hidden_frames_at = std::time::Instant::now();
            let hidden: Vec<smithay::desktop::Window> = self.managed.iter().filter(|m| self.space.element_location(&m.window).is_none()).map(|m| m.window.clone()).collect();
            for w in hidden {
                w.send_frame(o, ms, zero, |_, _| Some(o.clone()));
            }
        }
    }

    pub fn frames_to_all(&mut self, ms: std::time::Duration) {
        let zero = Some(std::time::Duration::ZERO);
        for w in self.space.elements() {
            if !self.wants_frame(w) {
                continue;
            }
            // the screen it is on (the focused one when it is on none)
            let o = self.space.outputs_for_element(w).into_iter().next().or_else(|| self.output.clone());
            if let Some(o) = o {
                if let Some(s) = smithay::wayland::seat::WaylandFocus::wl_surface(w) {
                    let scale = self.window_scale(w).unwrap_or_else(|| o.current_scale().fractional_scale());
                    crate::scale::set_tree_scale(&s, scale);
                    for (p, _) in smithay::desktop::PopupManager::popups_for_surface(&s) {
                        crate::scale::set_tree_scale(p.wl_surface(), scale);
                    }
                }
                w.send_frame(&o, ms, zero, |_, _| Some(o.clone()));
            }
        }
        for o in &self.outputs {
            for l in layer_map_for_output(o).layers() {
                crate::scale::set_tree_scale(l.wl_surface(), o.current_scale().fractional_scale());
                l.send_frame(o, ms, zero, |_, _| Some(o.clone()));
            }
        }
        if let Some(o) = self.output.clone() {
            self.lock_frame(&o, ms);
            self.cursor_frame(&o, ms);
        }
    }

    /// The cursor surface's frame callback: Xwayland waits for it before the next X cursor, so
    /// without it an X program's cursor (Bitwig) stayed the first one until a click re-entered
    fn cursor_frame(&self, o: &smithay::output::Output, ms: std::time::Duration) {
        if let smithay::input::pointer::CursorImageStatus::Surface(s) = &self.cursor_status {
            smithay::desktop::utils::send_frames_surface_tree(s, o, ms, Some(std::time::Duration::ZERO), |_, _| Some(o.clone()));
        }
    }

    /// Hyprland's monitor id: its place in the list.
    pub fn output_id(&self, name: &str) -> i64 {
        self.outputs.iter().position(|o| o.name() == name).map(|i| i as i64).unwrap_or(-1)
    }
}

impl Loom {
    /// A monitor as Hyprland's dispatchers name one: its name, its id, `desc:TEXT`, `current`,
    /// `+1` / `-1` (next / previous in the list), or `l` `r` `u` `d` (the nearest that way).
    pub fn monitor_arg(&self, a: &str) -> Option<Output> {
        let a = a.trim();
        let cur = self.output.clone()?;
        let i = self.outputs.iter().position(|o| *o == cur).unwrap_or(0) as i64;
        let n = self.outputs.len() as i64;
        match a {
            "" | "current" => return Some(cur),
            "+1" | "-1" => return self.outputs.get((i + a.parse::<i64>().unwrap_or(1)).rem_euclid(n.max(1)) as usize).cloned(),
            "l" | "r" | "u" | "d" | "left" | "right" | "up" | "down" => {
                let c = self.space.output_geometry(&cur)?;
                let mid = |g: Rectangle<i32, Logical>| (g.loc.x as f64 + g.size.w as f64 / 2.0, g.loc.y as f64 + g.size.h as f64 / 2.0);
                let (cx, cy) = mid(c);
                return self
                    .outputs
                    .iter()
                    .filter(|o| **o != cur)
                    .filter_map(|o| Some((o.clone(), mid(self.space.output_geometry(o)?))))
                    .filter(|(_, (x, y))| match &a[..1] {
                        "l" => *x < cx,
                        "r" => *x > cx,
                        "u" => *y < cy,
                        _ => *y > cy,
                    })
                    .min_by(|p, q| ((p.1 .0 - cx).hypot(p.1 .1 - cy)).total_cmp(&(q.1 .0 - cx).hypot(q.1 .1 - cy)))
                    .map(|p| p.0);
            }
            _ => {}
        }
        if let Some(d) = a.strip_prefix("desc:") {
            return self.outputs.iter().find(|o| format!("{} {}", o.physical_properties().make, o.physical_properties().model).contains(d)).cloned();
        }
        if let Ok(id) = a.parse::<usize>() {
            return self.outputs.get(id).cloned();
        }
        self.output_named(a)
    }

    /// Workspace `n` to screen `o`, shown there. The screen it was shown on shows another one
    /// (the lowest free), and what `o` showed before is hidden.
    pub fn workspace_to_monitor(&mut self, n: i32, o: &Output) {
        let to = o.name();
        let from = self.output_showing(n).map(|f| f.name());
        if from.as_deref() == Some(to.as_str()) {
            return;
        }
        // what `o` showed: hidden
        if let Some(old) = self.shown_on.get(&to).copied().filter(|old| *old != n) {
            let here: Vec<_> = self.managed.iter().filter(|m| m.ws == old && !m.pinned).map(|m| m.window.clone()).collect();
            for w in here {
                if let (Some(at), Some(m)) = (self.space.element_location(&w), self.managed.iter_mut().find(|m| m.window == w)) {
                    m.last_at = at;
                }
                self.space.unmap_elem(&w);
            }
        }
        self.shown_on.insert(to.clone(), n);
        self.ws_on.insert(n, to.clone());
        // the screen it left: the lowest workspace nobody shows
        if let Some(f) = from {
            let free = (1..).find(|k| *k != n && !self.is_shown(*k) && self.ws_on.get(k).is_none_or(|m| *m == f)).unwrap_or(n + 1);
            self.shown_on.insert(f.clone(), free);
            self.ws_on.insert(free, f);
            let here: Vec<_> = self.managed.iter().filter(|m| m.ws == free).map(|m| m.window.clone()).collect();
            for w in here {
                self.show_window(&w, false);
            }
        }
        let here: Vec<_> = self.managed.iter().filter(|m| m.ws == n).map(|m| m.window.clone()).collect();
        for w in here {
            self.show_window(&w, false);
        }
        if let Some(f) = self.output.clone() {
            self.active_ws = self.shown_on.get(&f.name()).copied().unwrap_or(self.active_ws);
        }
        self.hypr_event(&format!("moveworkspacev2>>{n},{n},{to}"));
        self.hypr_event(&format!("moveworkspace>>{n},{to}"));
        self.relayout();
    }

    /// The workspaces two screens show, swapped.
    pub fn swap_active_workspaces(&mut self, p: &Output, q: &Output) {
        let (Some(a), Some(b)) = (self.shown_on.get(&p.name()).copied(), self.shown_on.get(&q.name()).copied()) else { return };
        // floating windows go along to the other screen (the tiling moves only the tiled ones)
        if let (Some(pg), Some(qg)) = (self.space.output_geometry(p), self.space.output_geometry(q)) {
            let d = qg.loc - pg.loc;
            let moving: Vec<(smithay::desktop::Window, Point<i32, Logical>)> = self
                .managed
                .iter()
                .filter(|m| m.floating && !m.pinned && (m.ws == a || m.ws == b))
                .filter_map(|m| Some((m.window.clone(), self.space.element_location(&m.window)? + if m.ws == a { d } else { Point::from((-d.x, -d.y)) })))
                .collect();
            for (w, to) in moving {
                if let Some(x) = w.x11_surface() {
                    let _ = x.configure(smithay::utils::Rectangle::new(to, x.geometry().size));
                }
                self.space.map_element(w, to, false);
            }
        }
        self.shown_on.insert(p.name(), b);
        self.shown_on.insert(q.name(), a);
        self.ws_on.insert(b, p.name());
        self.ws_on.insert(a, q.name());
        if let Some(f) = self.output.clone() {
            self.active_ws = self.shown_on.get(&f.name()).copied().unwrap_or(self.active_ws);
        }
        self.hypr_event(&format!("moveworkspacev2>>{a},{a},{}", q.name()));
        self.hypr_event(&format!("moveworkspacev2>>{b},{b},{}", p.name()));
        self.relayout();
    }
}

impl Loom {
    /// The one scale a window draws at: that of the screen its middle is on. A window whose shadow
    /// reached the other screen was told both scales in turn (one per screen's frame), and Zen
    /// resized its pages in a loop.
    pub fn window_scale(&self, w: &smithay::desktop::Window) -> Option<f64> {
        let g = self.space.element_geometry(w)?;
        let mid = Point::from((g.loc.x as f64 + g.size.w as f64 / 2.0, g.loc.y as f64 + g.size.h as f64 / 2.0));
        let o = self.output_at(mid).or_else(|| self.space.outputs_for_element(w).into_iter().next())?;
        Some(o.current_scale().fractional_scale())
    }
}
