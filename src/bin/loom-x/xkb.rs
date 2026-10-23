//! XKB (XKEYBOARD 1.0): the keyboard as XKB clients read it (types, keysyms, actions, modifier map, names), state, controls, per-client flags, StateNotify.

use x11rb_protocol::protocol::Request;

use crate::{
    keys,
    server::{Rep, Server},
};

pub const XKB_EVENT: u8 = 97;
const DEVICE: u8 = 3;
// map parts
const TYPES: u16 = 1;
const SYMS: u16 = 2;
const MODMAP: u16 = 4;
const ACTIONS: u16 = 16;

/// The action of a modifier key: SetMods (1) for held modifiers, LockMods (3) for Caps and Num Lock.
fn action(sym: u32, mods: u8) -> [u8; 8] {
    let kind = if matches!(sym, 0xffe5 | 0xff7f) { 3 } else { 1 };
    // flags: ClearLocks for SetMods; the mask in the real modifiers
    let flags = if kind == 1 { 1 } else { 0 };
    [kind, flags, mods, mods, 0, 0, 0, 0]
}

impl Server {
    pub fn handle_xkb<'a>(&mut self, idx: usize, seq: u16, major: u8, req: Request<'a>) -> Result<(), Request<'a>> {
        match req {
            Request::XkbUseExtension(_) => self.reply(idx, Rep::new(seq, 1).u16(1).u16(0)),
            Request::XkbSelectEvents(r) => {
                let clear = u16::from(r.clear) as u32;
                let add = u16::from(r.select_all) as u32;
                let cur = self.xkb_sel.get(&idx).copied().unwrap_or(0);
                // StateNotify (bit 2) and the rest: whatever the client named, minus what it cleared
                let mut m = (cur | add) & !clear;
                if add == 0 && clear == 0 {
                    m |= 1 << 2;
                }
                self.xkb_sel.insert(idx, m);
            }
            Request::XkbBell(_) | Request::XkbLatchLockState(_) | Request::XkbSetControls(_) | Request::XkbSetMap(_) | Request::XkbSetCompatMap(_) | Request::XkbSetIndicatorMap(_) | Request::XkbSetNamedIndicator(_) | Request::XkbSetNames(_) | Request::XkbSetDeviceInfo(_) => {}
            Request::XkbSetDebuggingFlags(_) => self.reply(idx, Rep::new(seq, 0).u32(0).u32(0).u32(0).u32(0)),
            Request::XkbGetState(_) => {
                let m = self.xkb_mods() as u8;
                let rep = Rep::new(seq, DEVICE).u8(m).u8(m).u8(0).u8(0).u8(0).u8(0).u16(0).u16(0).u8(m).u8(m).u8(m).u8(m).u8(m).u8(0).u16(self.xkb_buttons()).pad(6);
                self.reply(idx, rep);
            }
            Request::XkbGetControls(_) => {
                let (delay, rate) = self.repeat_info();
                let interval = (1000 / rate.max(1)) as u16;
                // mouse keys button, groups (1, wrap), internal/ignore-lock mods; delays; enabled: RepeatKeys (1)
                let mut rep = Rep::new(seq, DEVICE).u8(1).u8(1).u8(0).u8(0).u8(0).u8(0).u8(0).pad(1);
                rep = rep.u16(0).u16(0).u16(delay as u16).u16(interval).u16(0).u16(0).u16(160).u16(40).u16(30).u16(30).i16(500).u16(0).u16(0).u16(0).u16(0).pad(2);
                rep = rep.u32(0).u32(0).u32(1);
                // per-key repeat: a bit per keycode
                let mut bits = [0u8; 32];
                for (i, r) in self.keymap.xkb.repeats.iter().enumerate() {
                    let k = i + keys::MIN as usize;
                    if *r {
                        bits[k / 8] |= 1 << (k % 8);
                    }
                }
                self.reply(idx, rep.bytes(&bits));
            }
            Request::XkbPerClientFlags(r) => {
                let change = u32::from(r.change);
                let value = u32::from(r.value);
                let cur = self.xkb_flags.get(&idx).copied().unwrap_or(0);
                let now = (cur & !change) | (value & change);
                self.xkb_flags.insert(idx, now);
                // supported: detectable autorepeat, grabs use XKB state, auto-reset, lookup state when grabbed, send-event state
                self.reply(idx, Rep::new(seq, DEVICE).u32(0x1f).u32(now).u32(0).u32(0));
            }
            Request::XkbGetMap(r) => {
                let want = u16::from(r.full) | u16::from(r.partial);
                let rep = self.xkb_map(seq, want);
                self.reply(idx, rep);
            }
            Request::XkbGetNames(r) => {
                let which = u32::from(r.which);
                let rep = self.xkb_names(seq, which);
                self.reply(idx, rep);
            }
            Request::XkbGetCompatMap(_) => self.reply(idx, Rep::new(seq, DEVICE).u8(0).pad(1).u16(0).u16(0).u16(0).pad(16)),
            Request::XkbGetIndicatorState(_) => self.reply(idx, Rep::new(seq, DEVICE).u32(0)),
            Request::XkbGetIndicatorMap(_) => self.reply(idx, Rep::new(seq, DEVICE).u32(0).u32(0).u8(0).pad(15)),
            Request::XkbGetNamedIndicator(r) => {
                let ind = r.indicator;
                self.reply(idx, Rep::new(seq, DEVICE).u32(ind).u8(0).u8(0).u8(0).u8(0).pad(12).u8(1).pad(3));
            }
            Request::XkbGetDeviceInfo(_) => {
                let name = b"loom keyboard";
                let atom = self.intern(b"KEYBOARD");
                let rep = Rep::new(seq, DEVICE).u16(0).u16(0).u16(0).u16(0).u8(0).u8(0).u8(0).u8(0).u8(0).u8(0).u16(0).u16(0).pad(2).u32(atom).u16(name.len() as u16).bytes(name);
                self.reply(idx, rep);
            }
            Request::XkbListComponents(_) => self.reply(idx, Rep::new(seq, DEVICE).u16(0).u16(0).u16(0).u16(0).u16(0).u16(0).u16(0)),
            Request::XkbGetKbdByName(_) => self.error(idx, 17, 0, major, 23),
            other => return Err(other),
        }
        Ok(())
    }

    /// GetMap: the parts asked for, over all keycodes.
    fn xkb_map(&mut self, seq: u16, want: u16) -> Rep {
        let x = &self.keymap.xkb;
        let n = x.key_type.len() as u8;
        // explicit components (8), virtual modifiers (64) and their map (128) are sent too, empty
        let present = want & (TYPES | SYMS | MODMAP | ACTIONS | 8 | 64 | 128);
        let total_syms: u16 = x.syms.iter().map(|s| s.len() as u16).sum();
        let acts: Vec<Option<[u8; 8]>> = x.syms.iter().zip(&x.modmap).map(|(s, m)| (*m != 0).then(|| action(s[0], *m))).collect();
        let total_acts = acts.iter().filter(|a| a.is_some()).count() as u16;
        let modmap: Vec<(u8, u8)> = x.modmap.iter().enumerate().filter(|(_, m)| **m != 0).map(|(i, m)| (i as u8 + keys::MIN, *m)).collect();
        let has = |p: u16| present & p != 0;
        let (nt, tt) = if has(TYPES) { (x.types.len() as u8, x.types.len() as u8) } else { (0, 0) };
        let mut r = Rep::new(seq, DEVICE).pad(2).u8(keys::MIN).u8(keys::MAX).u16(present);
        r = r.u8(0).u8(nt).u8(tt);
        r = if has(SYMS) { r.u8(keys::MIN).u16(total_syms).u8(n) } else { r.u8(0).u16(0).u8(0) };
        r = if has(ACTIONS) { r.u8(keys::MIN).u16(total_acts).u8(n) } else { r.u8(0).u16(0).u8(0) };
        // behaviors: none; explicit components: the key range, none set
        r = r.u8(0).u8(0).u8(0);
        r = if has(8) { r.u8(keys::MIN).u8(n).u8(0) } else { r.u8(0).u8(0).u8(0) };
        r = if has(MODMAP) { r.u8(keys::MIN).u8(n).u8(modmap.len() as u8) } else { r.u8(0).u8(0).u8(0) };
        // virtual modifier map: the key range, none set; no virtual modifiers
        r = if has(128) { r.u8(keys::MIN).u8(n).u8(0) } else { r.u8(0).u8(0).u8(0) };
        r = r.pad(1).u16(0);
        if has(TYPES) {
            for t in &x.types {
                r = r.u8(t.mods).u8(t.mods).u16(0).u8(t.levels).u8(t.entries.len() as u8).u8(0).pad(1);
                for &(m, l) in &t.entries {
                    r = r.u8(1).u8(m).u8(l).u8(m).u16(0).pad(2);
                }
            }
        }
        if has(SYMS) {
            for (i, syms) in x.syms.iter().enumerate() {
                let t = x.key_type[i];
                r = r.u8(t).u8(t).u8(t).u8(t).u8(1).u8(syms.len() as u8).u16(syms.len() as u16);
                for &s in syms {
                    r = r.u32(s);
                }
            }
        }
        if has(ACTIONS) {
            for a in &acts {
                r = r.u8(a.is_some() as u8);
            }
            r = r.pad((4 - n as usize % 4) % 4);
            for a in acts.iter().flatten() {
                r = r.bytes(a);
            }
        }
        if has(MODMAP) {
            for (k, m) in &modmap {
                r = r.u8(*k).u8(*m);
            }
            r = r.pad((4 - (modmap.len() * 2) % 4) % 4);
        }
        r
    }

    /// GetNames: what it asks for (component, type, level, group and key names).
    fn xkb_names(&mut self, seq: u16, which: u32) -> Rep {
        let n_types = self.keymap.xkb.types.len();
        let levels: Vec<u8> = self.keymap.xkb.types.iter().map(|t| t.levels).collect();
        let n_levels: u16 = levels.iter().map(|l| *l as u16).sum();
        let type_names: Vec<String> = self.keymap.xkb.types.iter().map(|t| t.name.clone()).collect();
        let key_names = self.keymap.xkb.names.clone();
        let mut atoms = |s: &str| self.intern(s.as_bytes());
        let components = [atoms("evdev"), atoms("pc(pc105)"), atoms("pc+loom"), atoms("pc+loom"), atoms("complete"), atoms("complete")];
        let type_atoms: Vec<u32> = type_names.iter().map(|n| atoms(n)).collect();
        let level_names = ["Base", "Shift", "Level3", "Level4", "Level5", "Level6", "Level7", "Level8"];
        let level_atoms: Vec<u32> = levels.iter().flat_map(|l| level_names[..*l as usize].iter().map(|n| atoms(n)).collect::<Vec<_>>()).collect();
        let group = atoms("Group 1");
        let has = |b: u32| which & b != 0;
        let n_keys = key_names.len() as u8;
        let mut r = Rep::new(seq, DEVICE).u32(which).u8(keys::MIN).u8(keys::MAX);
        r = r.u8(if has(64) || has(128) { n_types as u8 } else { 0 }).u8(if has(4096) { 1 } else { 0 }).u16(0);
        r = r.u8(if has(512) { keys::MIN } else { 0 }).u8(if has(512) { n_keys } else { 0 }).u32(0).u8(0).u8(0).u16(if has(128) { n_levels } else { 0 }).pad(4);
        // component names, in bit order
        for (bit, a) in components.iter().enumerate() {
            if has(1 << bit) {
                r = r.u32(*a);
            }
        }
        if has(64) {
            for a in &type_atoms {
                r = r.u32(*a);
            }
        }
        if has(128) {
            for l in &levels {
                r = r.u8(*l);
            }
            r = r.pad((4 - levels.len() % 4) % 4);
            for a in &level_atoms {
                r = r.u32(*a);
            }
        }
        // indicator and virtual modifier names: none
        if has(4096) {
            r = r.u32(group);
        }
        if has(512) {
            for k in &key_names {
                r = r.bytes(k);
            }
        }
        r
    }

    /// The modifier state as XKB counts it (real modifiers).
    fn xkb_mods(&self) -> u16 {
        self.state() & 0xff
    }

    fn xkb_buttons(&self) -> u16 {
        self.state() & 0xff00
    }

    /// StateNotify to the clients that asked for it (the modifiers changed).
    pub fn xkb_state_changed(&mut self) {
        let m = self.xkb_mods() as u8;
        let time = self.now();
        let buttons = self.xkb_buttons();
        let to: Vec<usize> = self.xkb_sel.iter().filter(|(_, mask)| **mask & (1 << 2) != 0).map(|(c, _)| *c).collect();
        for c in to {
            let mut ev = [0u8; 32];
            ev[0] = XKB_EVENT;
            ev[1] = 2;
            ev[4..8].copy_from_slice(&time.to_le_bytes());
            ev[8] = DEVICE;
            // mods, base, latched, locked; group, base/latched group, locked group; compat, grab, compat grab, lookup, compat lookup
            ev[9] = m;
            ev[10] = m;
            for b in &mut ev[19..24] {
                *b = m;
            }
            ev[24..26].copy_from_slice(&buttons.to_le_bytes());
            // changed: the modifier fields
            ev[26..28].copy_from_slice(&0x3fu16.to_le_bytes());
            self.send_event(c, ev);
        }
    }
}
