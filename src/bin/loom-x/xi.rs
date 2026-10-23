//! XInput 2.2: the devices (master pointer and keyboard, one physical device each), event selection per window, XI2 device, crossing, focus and raw events.

use x11rb_protocol::protocol::Request;

use crate::server::{Rep, Server, ROOT};

pub const XI_MAJOR: u8 = 134;
const POINTER: u16 = 2;
const KEYBOARD: u16 = 3;
const SLAVE_POINTER: u16 = 4;
const SLAVE_KEYBOARD: u16 = 5;

// XI2 event types
pub const KEY_PRESS: u16 = 2;
pub const KEY_RELEASE: u16 = 3;
pub const BUTTON_PRESS: u16 = 4;
pub const BUTTON_RELEASE: u16 = 5;
pub const MOTION: u16 = 6;
pub const ENTER: u16 = 7;
pub const LEAVE: u16 = 8;
pub const FOCUS_IN: u16 = 9;
pub const FOCUS_OUT: u16 = 10;
const RAW_KEY_PRESS: u16 = 13;
const RAW_BUTTON_PRESS: u16 = 15;
const RAW_MOTION: u16 = 17;

/// What the input event is about, for its XI2 copy.
pub struct Input {
    pub evtype: u16,
    pub detail: u32,
    pub root: (f64, f64),
    pub buttons: u16,
    pub mods: u16,
}

fn fp1616(v: f64) -> u32 {
    (v * 65536.0).round() as i32 as u32
}

/// A generic event under construction (XI2: its length counts past 32 bytes).
struct Gen(Vec<u8>);
impl Gen {
    fn new(evtype: u16, device: u16, time: u32) -> Gen {
        let mut v = vec![35, XI_MAJOR, 0, 0, 0, 0, 0, 0];
        v.extend_from_slice(&evtype.to_le_bytes());
        v.extend_from_slice(&device.to_le_bytes());
        v.extend_from_slice(&time.to_le_bytes());
        Gen(v)
    }
    fn u8(mut self, v: u8) -> Gen {
        self.0.push(v);
        self
    }
    fn u16(mut self, v: u16) -> Gen {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Gen {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn mods(self, m: u16) -> Gen {
        // base, latched, locked, effective; then the group (four bytes)
        self.u32(m as u32).u32(0).u32(0).u32(m as u32).u32(0)
    }
    fn done(mut self) -> Vec<u8> {
        while self.0.len() < 32 || self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        let len = ((self.0.len() - 32) / 4) as u32;
        self.0[4..8].copy_from_slice(&len.to_le_bytes());
        self.0
    }
}

/// X buttons held as XI2's button mask (bit n = button n).
fn button_mask(buttons: u16) -> u32 {
    let mut m = 0;
    for b in 1..=5u32 {
        if buttons & (0x80 << b) != 0 {
            m |= 1 << b;
        }
    }
    m
}

impl Server {
    /// Clients selecting XI2 event `evtype` on window `w` for a pointer (or keyboard) device.
    fn xi_selecting(&self, w: u32, evtype: u16, keyboard: bool) -> Vec<usize> {
        let (master, slave) = if keyboard { (KEYBOARD, SLAVE_KEYBOARD) } else { (POINTER, SLAVE_POINTER) };
        self.xi_sel
            .iter()
            .filter(|(_, win, dev, mask)| *win == w && (*dev == 0 || *dev == 1 || *dev == master || *dev == slave) && mask & (1 << evtype) != 0)
            .map(|(c, ..)| *c)
            .collect()
    }

    /// The XI2 copy of a pointer or key event, from `start` up to the first window selecting it.
    pub fn xi_input(&mut self, start: u32, under: u32, ev: &Input, offset_of: impl Fn(&Server, u32) -> (i32, i32)) {
        if self.xi_sel.is_empty() {
            return;
        }
        let keyboard = matches!(ev.evtype, KEY_PRESS | KEY_RELEASE);
        let time = self.now();
        let mut w = start;
        loop {
            let to = self.xi_selecting(w, ev.evtype, keyboard);
            if !to.is_empty() {
                let (ox, oy) = offset_of(self, w);
                let child = self.child_toward(w, under);
                let (dev, src) = if keyboard { (KEYBOARD, SLAVE_KEYBOARD) } else { (POINTER, SLAVE_POINTER) };
                let mut g = Gen::new(ev.evtype, dev, time).u32(ev.detail).u32(ROOT).u32(w).u32(child);
                g = g.u32(fp1616(ev.root.0)).u32(fp1616(ev.root.1)).u32(fp1616(ev.root.0 - ox as f64)).u32(fp1616(ev.root.1 - oy as f64));
                // one word of buttons; motion carries x and y as valuators 0 and 1
                let motion = ev.evtype == MOTION;
                g = g.u16(1).u16(if motion { 1 } else { 0 }).u16(src).u16(0).u32(0).mods(ev.mods).u32(button_mask(ev.buttons));
                if motion {
                    g = g.u32(0b11);
                    for v in [ev.root.0, ev.root.1] {
                        g = g.u32(v.trunc() as i32 as u32).u32((v.fract() * 4294967296.0) as u32);
                    }
                }
                let bytes = g.done();
                for c in to {
                    self.send_generic(c, bytes.clone());
                }
                return;
            }
            let Some(win) = self.wins.get(&w) else { return };
            if win.parent == ROOT || win.parent == 0 {
                return;
            }
            w = win.parent;
        }
    }

    /// Raw events (selected on the root window): every motion, button and key, focus or not.
    pub fn xi_raw(&mut self, evtype: u16, detail: u32, delta: Option<(f64, f64)>) {
        let raw = match evtype {
            MOTION => RAW_MOTION,
            BUTTON_PRESS | BUTTON_RELEASE => RAW_BUTTON_PRESS + (evtype - BUTTON_PRESS),
            _ => RAW_KEY_PRESS + (evtype - KEY_PRESS),
        };
        let keyboard = matches!(evtype, KEY_PRESS | KEY_RELEASE);
        let to = self.xi_selecting(ROOT, raw, keyboard);
        if to.is_empty() {
            return;
        }
        let time = self.now();
        let (dev, src) = if keyboard { (KEYBOARD, SLAVE_KEYBOARD) } else { (POINTER, SLAVE_POINTER) };
        let mut g = Gen::new(raw, dev, time).u32(detail).u16(src).u16(if delta.is_some() { 1 } else { 0 }).u32(0).u32(0);
        if let Some((dx, dy)) = delta {
            g = g.u32(0b11);
            // the values, then the same unaccelerated
            for _ in 0..2 {
                for v in [dx, dy] {
                    g = g.u32(v.trunc() as i32 as u32).u32((v.fract().abs() * 4294967296.0) as u32);
                }
            }
        }
        let bytes = g.done();
        for c in to {
            self.send_generic(c, bytes.clone());
        }
    }

    /// XI2 crossing (enter, leave) and focus events to a window's selecting clients.
    pub fn xi_crossing(&mut self, w: u32, evtype: u16, root: (f64, f64), win_pos: (i32, i32), buttons: u16, mods: u16) {
        let keyboard = matches!(evtype, FOCUS_IN | FOCUS_OUT);
        let to = self.xi_selecting(w, evtype, keyboard);
        if to.is_empty() {
            return;
        }
        let time = self.now();
        let (dev, src) = if keyboard { (KEYBOARD, SLAVE_KEYBOARD) } else { (POINTER, SLAVE_POINTER) };
        let g = Gen::new(evtype, dev, time).u16(src).u8(0).u8(3).u32(ROOT).u32(w).u32(0);
        let g = g.u32(fp1616(root.0)).u32(fp1616(root.1)).u32(fp1616(root.0 - win_pos.0 as f64)).u32(fp1616(root.1 - win_pos.1 as f64));
        let bytes = g.u8(1).u8(1).u16(1).mods(mods).u32(button_mask(buttons)).done();
        for c in to {
            self.send_generic(c, bytes.clone());
        }
    }

    pub fn handle_xi<'a>(&mut self, idx: usize, seq: u16, major: u8, req: Request<'a>) -> Result<(), Request<'a>> {
        match req {
            // ---- XI2 ----
            Request::XinputXIQueryVersion(r) => {
                let (maj, min) = if (r.major_version, r.minor_version) < (2, 2) { (r.major_version, r.minor_version) } else { (2, 2) };
                self.reply(idx, Rep::new(seq, 0).u16(maj).u16(min));
            }
            Request::XinputXIQueryDevice(r) => {
                let want = u16::from(r.deviceid);
                let devices: Vec<u16> = match want {
                    0 => vec![POINTER, KEYBOARD, SLAVE_POINTER, SLAVE_KEYBOARD],
                    1 => vec![POINTER, KEYBOARD],
                    d if (POINTER..=SLAVE_KEYBOARD).contains(&d) => vec![d],
                    d => return Ok(self.error(idx, 139, d as u32, major, 48)),
                };
                let mut rep = Rep::new(seq, 0).u16(devices.len() as u16).pad(22);
                for d in devices {
                    rep = self.device_info(rep, d);
                }
                self.reply(idx, rep);
            }
            Request::XinputXISelectEvents(r) => {
                for m in r.masks.iter() {
                    let dev = u16::from(m.deviceid);
                    let bits = m.mask.first().map(|v| u32::from(*v)).unwrap_or(0);
                    self.xi_sel.retain(|(c, w, d, _)| !(*c == idx && *w == r.window && *d == dev));
                    if bits != 0 {
                        self.xi_sel.push((idx, r.window, dev, bits));
                    }
                }
            }
            Request::XinputXIGetSelectedEvents(r) => {
                let mine: Vec<(u16, u32)> = self.xi_sel.iter().filter(|(c, w, ..)| *c == idx && *w == r.window).map(|(_, _, d, m)| (*d, *m)).collect();
                let mut rep = Rep::new(seq, 0).u16(mine.len() as u16).pad(22);
                for (d, m) in mine {
                    rep = rep.u16(d).u16(1).u32(m);
                }
                self.reply(idx, rep);
            }
            Request::XinputXIGetClientPointer(_) => self.reply(idx, Rep::new(seq, 0).u8(1).u8(0).u16(POINTER)),
            Request::XinputXIQueryPointer(r) => {
                let (root, win, child, buttons, mods) = self.pointer_state(r.window);
                let rep = Rep::new(seq, 0).u32(ROOT).u32(child).u32(fp1616(root.0)).u32(fp1616(root.1)).u32(fp1616(win.0)).u32(fp1616(win.1)).u8(1).u8(0).u16(1);
                let rep = rep.u32(mods as u32).u32(0).u32(0).u32(mods as u32).u32(0).u32(button_mask(buttons));
                self.reply(idx, rep);
            }
            Request::XinputXIGetFocus(_) => {
                let f = self.focus_window();
                self.reply(idx, Rep::new(seq, 0).u32(f));
            }
            Request::XinputXISetFocus(r) => self.set_focus(r.window),
            Request::XinputXIGrabDevice(_) => self.reply(idx, Rep::new(seq, 0).u8(0)),
            Request::XinputXIPassiveGrabDevice(_) => self.reply(idx, Rep::new(seq, 0).u16(0)),
            Request::XinputXIListProperties(_) => self.reply(idx, Rep::new(seq, 0).u16(0)),
            Request::XinputXIGetProperty(_) => self.reply(idx, Rep::new(seq, 0).u32(0).u32(0).u32(0).u8(0)),
            Request::XinputXIUngrabDevice(_)
            | Request::XinputXIAllowEvents(_)
            | Request::XinputXIPassiveUngrabDevice(_)
            | Request::XinputXIChangeCursor(_)
            | Request::XinputXIWarpPointer(_)
            | Request::XinputXIChangeProperty(_)
            | Request::XinputXIDeleteProperty(_)
            | Request::XinputXIChangeHierarchy(_)
            | Request::XinputXISetClientPointer(_)
            | Request::XinputXIBarrierReleasePointer(_) => {}
            // ---- XInput 1 (enough for programs that list devices the old way) ----
            Request::XinputGetExtensionVersion(_) => self.reply(idx, Rep::new(seq, 1).u16(2).u16(2).u8(1)),
            Request::XinputListInputDevices(_) => {
                let names: [&[u8]; 2] = [b"Virtual core pointer", b"Virtual core keyboard"];
                let atom_mouse = self.intern(b"MOUSE");
                let atom_kbd = self.intern(b"KEYBOARD");
                let mut rep = Rep::new(seq, 2).u8(2).pad(23);
                // device infos (type, id, classes, use: 0 = core pointer, 1 = core keyboard), then the names
                rep = rep.u32(atom_mouse).u8(POINTER as u8).u8(0).u8(0).u8(0);
                rep = rep.u32(atom_kbd).u8(KEYBOARD as u8).u8(0).u8(1).u8(0);
                for n in names {
                    rep = rep.u8(n.len() as u8).bytes(n);
                }
                self.reply(idx, rep);
            }
            Request::XinputOpenDevice(_) => self.reply(idx, Rep::new(seq, 3).u8(0)),
            Request::XinputCloseDevice(_) | Request::XinputSelectExtensionEvent(_) | Request::XinputUngrabDevice(_) | Request::XinputAllowDeviceEvents(_) => {}
            Request::XinputGetSelectedExtensionEvents(_) => self.reply(idx, Rep::new(seq, 0).u16(0).u16(0)),
            other => return Err(other),
        }
        Ok(())
    }

    /// XIQueryDevice's description of device `d`: buttons (and two axes) for pointers, keycodes for keyboards.
    fn device_info(&mut self, rep: Rep, d: u16) -> Rep {
        let (kind, attach, name): (u16, u16, &[u8]) = match d {
            POINTER => (1, KEYBOARD, b"Virtual core pointer"),
            KEYBOARD => (2, POINTER, b"Virtual core keyboard"),
            SLAVE_POINTER => (3, POINTER, b"loom pointer"),
            _ => (4, KEYBOARD, b"loom keyboard"),
        };
        let pointer = matches!(d, POINTER | SLAVE_POINTER);
        let classes = if pointer { 3 } else { 1 };
        let mut rep = rep.u16(d).u16(kind).u16(attach).u16(classes).u16(name.len() as u16).u8(1).u8(0).bytes(name).pad((4 - name.len() % 4) % 4);
        if pointer {
            // buttons: 10, labels none; then the x and y axes (absolute, screen size)
            let (w, h) = self.screen();
            // its length in words: the 8-byte head, one word of state, ten labels
            rep = rep.u16(1).u16(2 + 1 + 10).u16(d).u16(10).u32(0);
            for _ in 0..10 {
                rep = rep.u32(0);
            }
            let labels = [self.intern(b"Abs X"), self.intern(b"Abs Y")];
            for (n, (label, max)) in labels.into_iter().zip([w, h]).enumerate() {
                rep = rep.u16(2).u16(11).u16(d).u16(n as u16).u32(label).u32(0).u32(0).u32(max as u32).u32(0).u32(0).u32(0).u32(1).u8(1).pad(3);
            }
        } else {
            let n = (crate::keys::MAX - crate::keys::MIN) as u16 + 1;
            rep = rep.u16(0).u16(2 + n).u16(d).u16(n);
            for k in crate::keys::MIN..=crate::keys::MAX {
                rep = rep.u32(k as u32);
            }
        }
        rep
    }
}
