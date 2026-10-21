//! Every pointer event, from any backend: motion, buttons, scroll. The user's mouse binds come
//! first (bindm Super+drag moves or resizes the window under the pointer, Super+scroll switches
//! workspaces, side buttons), then focus-on-click, then the program under the pointer.

use smithay::{
    backend::input::{Axis, AxisSource, ButtonState},
    desktop::Window,
    input::pointer::{AxisFrame, ButtonEvent, MotionEvent},
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
};

use crate::{binds::Key, state::Loom};

/// A window being dragged with a mouse bind.
pub struct Drag {
    pub resize: bool,
    pub window: Window,
    pub button: u32,
    pub start_ptr: Point<f64, Logical>,
    pub start: Rectangle<i32, Logical>,
    /// a tiled window floats while it is moved, and is tiled again where it is dropped
    pub was_tiled: bool,
}

/// Scroll binds fire at most this often (Hyprland's binds:scroll_event_delay).
const SCROLL_DELAY_MS: u32 = 300;

impl Loom {
    fn mods_mask(&self) -> u8 {
        self.seat.get_keyboard().map(|k| crate::binds::mask(&k.modifier_state())).unwrap_or(0)
    }

    /// The pointer to `at` (compositor space).
    pub fn pointer_move(&mut self, at: Point<f64, Logical>, time: u32) {
        self.pointer_at = at;
        if self.drag.is_some() {
            self.drag_update(at);
            return;
        }
        let focus = self.surface_at(at);
        let pointer = self.seat.get_pointer().expect("pointer");
        pointer.motion(self, focus, &MotionEvent { location: at, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    /// A button (evdev code, 272 = left) pressed or let go.
    pub fn pointer_button(&mut self, button: u32, pressed: bool, time: u32) {
        // a drag ends with its button
        if !pressed && self.drag.as_ref().is_some_and(|d| d.button == button) {
            self.drag_end();
            return;
        }
        if pressed {
            self.grab_press(self.pointer_at);
        }
        let mods = self.mods_mask();
        let (acts, consume) = self.binds.mouse(Key::Mouse(button), mods, pressed);
        for a in acts.unwrap_or_default() {
            match a.strip_prefix("__drag ") {
                Some(kind) => self.drag_start(kind.starts_with("resize"), button),
                None => {
                    let r = self.hypr_dispatch(&a);
                    if r != "ok" {
                        eprintln!("loom: mouse bind `{a}`: {r}");
                    }
                }
            }
        }
        if consume {
            return;
        }
        // a press on a window: it comes up and has the keyboard
        if pressed {
            if let Some(w) = self.space.element_under(self.pointer_at).map(|(w, _)| w.clone()) {
                self.focus_window(&w);
            }
        }
        let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
        let pointer = self.seat.get_pointer().expect("pointer");
        pointer.button(self, &ButtonEvent { button, state, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    /// Scroll: `v` vertical, `h` horizontal (positive = down / right), with `v120` notches if the
    /// device has them. Scroll binds take it when one matches.
    pub fn pointer_scroll(&mut self, source: AxisSource, v: f64, h: f64, v120: Option<(f64, f64)>, time: u32) {
        let mods = self.mods_mask();
        let dir = if v < 0.0 { Some(0) } else if v > 0.0 { Some(1) } else if h < 0.0 { Some(2) } else if h > 0.0 { Some(3) } else { None };
        if let Some(d) = dir {
            let (acts, consume) = self.binds.mouse(Key::Scroll(d), mods, true);
            if let Some(acts) = acts {
                if time.wrapping_sub(self.last_scroll_bind) >= SCROLL_DELAY_MS || self.last_scroll_bind == 0 {
                    self.last_scroll_bind = time.max(1);
                    for a in acts {
                        self.hypr_dispatch(&a);
                    }
                }
                if consume {
                    return;
                }
            }
        }
        let mut frame = AxisFrame::new(time).source(source);
        for (axis, amount, notch) in [(Axis::Vertical, v, v120.map(|n| n.0)), (Axis::Horizontal, h, v120.map(|n| n.1))] {
            if amount != 0.0 {
                frame = frame.value(axis, amount);
                if let Some(n) = notch.filter(|n| *n != 0.0) {
                    frame = frame.v120(axis, n as i32);
                }
            }
        }
        let pointer = self.seat.get_pointer().expect("pointer");
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn drag_start(&mut self, resize: bool, button: u32) {
        let Some(w) = self.space.element_under(self.pointer_at).map(|(w, _)| w.clone()) else { return };
        let Some(m) = self.managed_of(&w).cloned() else { return };
        let Some(start) = self.space.element_geometry(&w) else { return };
        let was_tiled = !m.floating;
        self.focus_window(&w);
        if was_tiled && !resize {
            // it leaves the tiling while it moves (the others close up behind it)
            if let Some(mm) = self.managed.iter_mut().find(|x| x.window == w) {
                mm.floating = true;
            }
            self.relayout();
            self.space.map_element(w.clone(), start.loc, true);
        }
        self.drag = Some(Drag { resize, window: w, button, start_ptr: self.pointer_at, start, was_tiled });
    }

    fn drag_update(&mut self, at: Point<f64, Logical>) {
        let Some(d) = &self.drag else { return };
        let (dx, dy) = ((at.x - d.start_ptr.x) as i32, (at.y - d.start_ptr.y) as i32);
        let w = d.window.clone();
        if !d.resize {
            let to: Point<i32, Logical> = (d.start.loc.x + dx, d.start.loc.y + dy).into();
            self.space.map_element(w.clone(), to, true);
            if let Some(x) = w.x11_surface() {
                let _ = x.configure(Rectangle::new(to, x.geometry().size));
            }
            // it follows the hand: no glide
            if let Some(a) = self.managed_of(&w).map(|m| m.addr).and_then(|addr| self.anims.get_mut(&addr)) {
                a.jump_to(to.x as f64, to.y as f64);
            }
        } else if d.was_tiled {
            // a tiled window: the layout's master share follows the drag
            let width = self.output.as_ref().and_then(|o| self.space.output_geometry(o)).map(|g| g.size.w).unwrap_or(1).max(1) as f64;
            let step = dx as f64 / width;
            if step.abs() >= 0.01 {
                let ws = self.active_ws.to_string();
                let _ = self.layouts.message(&ws, &format!("mwfact {:+.3}", step));
                if let Some(dd) = self.drag.as_mut() {
                    dd.start_ptr.x = at.x;
                }
                self.relayout();
            }
        } else {
            let size: Size<i32, Logical> = ((d.start.size.w + dx).max(80), (d.start.size.h + dy).max(60)).into();
            if let Some(t) = w.toplevel() {
                t.with_pending_state(|s| s.size = Some(size));
                t.send_pending_configure();
            } else if let Some(x) = w.x11_surface() {
                let _ = x.configure(Rectangle::new(d.start.loc, size));
            }
        }
        self.dirty = true;
    }

    fn drag_end(&mut self) {
        let Some(d) = self.drag.take() else { return };
        if d.was_tiled && !d.resize {
            // dropped: tiled again, in the place of the window it was dropped on
            let target = self.tile_slot_at(self.pointer_at, &d.window).filter(|w| w != &d.window);
            let i = self.managed.iter().position(|m| m.window == d.window);
            if let Some(i) = i {
                self.managed[i].floating = false;
                if let Some(j) = target.and_then(|t| self.managed.iter().position(|m| m.window == t)) {
                    self.managed.swap(i, j);
                }
            }
            self.relayout();
        }
    }

    /// A window by Hyprland's selectors: `address:0x…`, `class:REGEX`, `title:REGEX`, `pid:N`,
    /// `initialclass:`/`initialtitle:`; a bare word is a class regex.
    pub fn find_window(&self, sel: &str) -> Option<crate::wm::Managed> {
        let sel = sel.trim();
        let (kind, val) = sel.split_once(':').unwrap_or(("class", sel));
        match kind {
            "address" => {
                let want = val.trim_start_matches("0x");
                self.managed.iter().find(|m| format!("{:x}", m.addr) == want).cloned()
            }
            "pid" => val.parse::<i32>().ok().and_then(|p| self.managed.iter().find(|m| m.pid == p).cloned()),
            "class" | "initialclass" | "title" | "initialtitle" => {
                let re = regex::Regex::new(val).ok()?;
                let title = kind.ends_with("title");
                // the most recently focused match first, as Hyprland picks
                let mut hits: Vec<&crate::wm::Managed> = self.managed.iter().filter(|m| re.is_match(&if title { self.title_of(&m.window) } else { self.class_of(&m.window) })).collect();
                hits.sort_by_key(|m| self.focused.as_ref() != Some(&m.window));
                hits.first().map(|m| (*m).clone())
            }
            _ => None,
        }
    }
}
