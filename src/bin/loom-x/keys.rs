//! The keyboard as X sees it: keysyms per keycode from loom's xkb keymap (keycode = evdev + 8), and the modifier map.

use xkbcommon::xkb;

pub const MIN: u8 = 8;
pub const MAX: u8 = 255;
/// keysyms a keycode: plain, Shift, then the same again (group 2 = group 1), AltGr, AltGr+Shift
pub const PER: usize = 6;

pub struct Keymap {
    pub syms: Vec<u32>,
    /// the same keyboard as XKB describes it
    pub xkb: Xkb,
}

impl Keymap {
    pub fn from_xkb(km: &xkb::Keymap) -> Keymap {
        let mut syms = vec![0u32; (MAX as usize - MIN as usize + 1) * PER];
        for code in MIN as u32..=MAX as u32 {
            let k = xkb::Keycode::new(code);
            let level = |l: u32| {
                km.key_get_syms_by_level(k, 0, l)
                    .first()
                    .map(|s| s.raw())
                    .unwrap_or(0)
            };
            let (l1, l2, l3, l4) = (level(0), level(1), level(2), level(3));
            let o = (code as usize - MIN as usize) * PER;
            syms[o..o + PER].copy_from_slice(&[l1, l2, l1, l2, l3, l4]);
        }
        Keymap { syms, xkb: Xkb::from_xkb(km) }
    }

    /// US until loom sends its keymap.
    pub fn us() -> Keymap {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        match xkb::Keymap::new_from_names(
            &ctx,
            "",
            "",
            "us",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) {
            Some(km) => Keymap::from_xkb(&km),
            None => Keymap {
                syms: vec![0; (MAX as usize - MIN as usize + 1) * PER],
                xkb: Xkb::default(),
            },
        }
    }

    pub fn from_fd(fd: std::os::fd::OwnedFd, size: usize) -> Option<Keymap> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let km = unsafe {
            xkb::Keymap::new_from_fd(
                &ctx,
                fd,
                size,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        }
        .ok()??;
        Some(Keymap::from_xkb(&km))
    }
}

/// Shift, Lock, Control, Mod1 (Alt), Mod2 (NumLock), Mod3, Mod4 (Super), Mod5 (AltGr): two keycodes each.
pub const MODIFIERS: [[u8; 2]; 8] = [
    [50, 62],
    [66, 0],
    [37, 105],
    [64, 108],
    [77, 0],
    [0, 0],
    [133, 134],
    [92, 0],
];

/// xkb's modifier bits (loom's keymap: Shift, Lock, Control, Mod1..Mod5 in X order) as an X state mask.
pub fn x_state(mods: u32) -> u16 {
    (mods & 0xff) as u16
}

/// A key type: which real modifiers it looks at, its levels, and the modifier combinations leading to each.
#[derive(Clone, PartialEq)]
pub struct KType {
    pub name: String,
    pub mods: u8,
    pub levels: u8,
    /// (modifiers, level), level 0 included only with modifiers
    pub entries: Vec<(u8, u8)>,
}

/// The keyboard for XKB clients: types, each key's type, keysyms per level, the modifier map, key names.
#[derive(Default)]
pub struct Xkb {
    pub types: Vec<KType>,
    /// per keycode MIN..=MAX
    pub key_type: Vec<u8>,
    pub syms: Vec<Vec<u32>>,
    pub modmap: Vec<u8>,
    pub names: Vec<[u8; 4]>,
    pub repeats: Vec<bool>,
}

unsafe extern "C" {
    fn xkb_keymap_key_get_mods_for_level(keymap: *mut xkb::ffi::xkb_keymap, key: u32, layout: u32, level: u32, masks_out: *mut u32, masks_size: usize) -> usize;
    fn xkb_keymap_mod_get_mask2(keymap: *mut xkb::ffi::xkb_keymap, idx: u32) -> u32;
}

/// The four types every XKB keyboard starts with.
fn canonical() -> Vec<KType> {
    let t = |name: &str, mods: u8, levels: u8, entries: &[(u8, u8)]| KType { name: name.into(), mods, levels, entries: entries.to_vec() };
    vec![t("ONE_LEVEL", 0, 1, &[]), t("TWO_LEVEL", 1, 2, &[(1, 1)]), t("ALPHABETIC", 3, 2, &[(1, 1), (2, 1)]), t("KEYPAD", 1 | 16, 2, &[(1, 1), (16, 1)])]
}

/// The real modifier a modifier key's keysym stands for (as the X modifier map has it).
fn modifier_of(sym: u32) -> u8 {
    match sym {
        0xffe1 | 0xffe2 => 1,                   // Shift_L/R
        0xffe5 => 2,                            // Caps_Lock
        0xffe3 | 0xffe4 => 4,                   // Control_L/R
        0xffe9 | 0xffea | 0xffe7 | 0xffe8 => 8, // Alt, Meta
        0xff7f => 16,                           // Num_Lock
        0xffeb | 0xffec | 0xffed | 0xffee => 64, // Super, Hyper
        0xfe03 | 0xff7e => 128,                 // ISO_Level3_Shift, Mode_switch
        _ => 0,
    }
}

impl Xkb {
    pub fn from_xkb(km: &xkb::Keymap) -> Xkb {
        let raw = km.get_raw_ptr();
        // a modifier mask (real and virtual modifiers) as real modifiers only
        let real = |m: u32| -> u8 {
            let mut out = 0u32;
            for i in 0..32 {
                if m & (1 << i) != 0 {
                    out |= unsafe { xkb_keymap_mod_get_mask2(raw, i) };
                }
            }
            (out & 0xff) as u8
        };
        let mut x = Xkb { types: canonical(), ..Default::default() };
        for code in MIN as u32..=MAX as u32 {
            let k = xkb::Keycode::new(code);
            let levels = km.num_levels_for_key(k, 0).clamp(1, 8);
            let syms: Vec<u32> = (0..levels).map(|l| km.key_get_syms_by_level(k, 0, l).first().map(|s| s.raw()).unwrap_or(0)).collect();
            let mut entries = Vec::new();
            for l in 0..levels {
                let mut masks = [0u32; 16];
                let n = unsafe { xkb_keymap_key_get_mods_for_level(raw, code, 0, l, masks.as_mut_ptr(), masks.len()) };
                for &m in &masks[..n.min(16)] {
                    let r = real(m);
                    if r != 0 || l != 0 {
                        if !(r == 0 && l == 0) {
                            entries.push((r, l as u8));
                        }
                    }
                }
            }
            entries.sort();
            entries.dedup();
            let mods = entries.iter().fold(0, |a, e| a | e.0);
            let want = KType { name: String::new(), mods, levels: levels as u8, entries };
            let idx = x.types.iter().position(|t| t.mods == want.mods && t.levels == want.levels && t.entries == want.entries).unwrap_or_else(|| {
                let n = x.types.len();
                x.types.push(KType { name: format!("LOOM_LEVEL{}_{}", want.levels, n), ..want });
                n
            });
            x.key_type.push(idx as u8);
            x.modmap.push(modifier_of(syms[0]));
            let mut name = [0u8; 4];
            if let Some(n) = km.key_get_name(k) {
                for (d, s) in name.iter_mut().zip(n.bytes()) {
                    *d = s;
                }
            }
            x.names.push(name);
            x.repeats.push(km.key_repeats(k));
            x.syms.push(syms);
        }
        x
    }
}
