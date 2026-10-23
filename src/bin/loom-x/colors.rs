//! X colour names (no rgb.txt here): the common ones, grayN/greyN, #rgb forms and rgb:r/g/b.

const NAMES: &[(&str, (u8, u8, u8))] = &[
    ("black", (0, 0, 0)),
    ("white", (255, 255, 255)),
    ("red", (255, 0, 0)),
    ("green", (0, 255, 0)),
    ("blue", (0, 0, 255)),
    ("yellow", (255, 255, 0)),
    ("cyan", (0, 255, 255)),
    ("magenta", (255, 0, 255)),
    ("gray", (190, 190, 190)),
    ("grey", (190, 190, 190)),
    ("darkgray", (169, 169, 169)),
    ("darkgrey", (169, 169, 169)),
    ("lightgray", (211, 211, 211)),
    ("lightgrey", (211, 211, 211)),
    ("dimgray", (105, 105, 105)),
    ("dimgrey", (105, 105, 105)),
    ("slategray", (112, 128, 144)),
    ("navy", (0, 0, 128)),
    ("navyblue", (0, 0, 128)),
    ("orange", (255, 165, 0)),
    ("darkorange", (255, 140, 0)),
    ("brown", (165, 42, 42)),
    ("pink", (255, 192, 203)),
    ("purple", (160, 32, 240)),
    ("violet", (238, 130, 238)),
    ("gold", (255, 215, 0)),
    ("beige", (245, 245, 220)),
    ("wheat", (245, 222, 179)),
    ("tan", (210, 180, 140)),
    ("khaki", (240, 230, 140)),
    ("maroon", (176, 48, 96)),
    ("darkred", (139, 0, 0)),
    ("darkgreen", (0, 100, 0)),
    ("darkblue", (0, 0, 139)),
    ("lightblue", (173, 216, 230)),
    ("skyblue", (135, 206, 235)),
    ("steelblue", (70, 130, 180)),
    ("royalblue", (65, 105, 225)),
    ("forestgreen", (34, 139, 34)),
    ("seagreen", (46, 139, 87)),
    ("limegreen", (50, 205, 50)),
    ("turquoise", (64, 224, 208)),
    ("coral", (255, 127, 80)),
    ("salmon", (250, 128, 114)),
    ("tomato", (255, 99, 71)),
    ("orchid", (218, 112, 214)),
    ("plum", (221, 160, 221)),
    ("ivory", (255, 255, 240)),
    ("linen", (250, 240, 230)),
    ("snow", (255, 250, 250)),
    ("gainsboro", (220, 220, 220)),
    ("silver", (192, 192, 192)),
    ("whitesmoke", (245, 245, 245)),
    ("lightyellow", (255, 255, 224)),
    ("lightgoldenrodyellow", (250, 250, 210)),
    ("darkslategray", (47, 79, 79)),
    ("midnightblue", (25, 25, 112)),
    ("cornflowerblue", (100, 149, 237)),
    ("dodgerblue", (30, 144, 255)),
    ("deepskyblue", (0, 191, 255)),
    ("firebrick", (178, 34, 34)),
    ("indianred", (205, 92, 92)),
    ("sienna", (160, 82, 45)),
    ("chocolate", (210, 105, 30)),
    ("peru", (205, 133, 63)),
];

fn hex(s: &str) -> Option<u8> {
    // X scales each field to 8 bits by its own width
    let v = u32::from_str_radix(s, 16).ok()?;
    let bits = s.len() as u32 * 4;
    Some(if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        (v * 255 / ((1 << bits) - 1)) as u8
    })
}

pub fn lookup(name: &[u8]) -> Option<(u8, u8, u8)> {
    let n: String = String::from_utf8_lossy(name)
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if let Some(h) = n.strip_prefix('#') {
        let k = h.len() / 3;
        if k == 0 || h.len() % 3 != 0 || k > 4 {
            return None;
        }
        return Some((hex(&h[..k])?, hex(&h[k..2 * k])?, hex(&h[2 * k..])?));
    }
    if let Some(r) = n.strip_prefix("rgb:") {
        let p: Vec<&str> = r.split('/').collect();
        return (p.len() == 3)
            .then_some(())
            .and_then(|_| Some((hex(p[0])?, hex(p[1])?, hex(p[2])?)));
    }
    for pre in ["gray", "grey"] {
        if let Some(v) = n
            .strip_prefix(pre)
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v <= 100)
        {
            let g = ((v * 255 + 50) / 100) as u8;
            return Some((g, g, g));
        }
    }
    if let Some(c) = NAMES.iter().find(|(k, _)| *k == n).map(|(_, c)| *c) {
        return Some(c);
    }
    // name1..name4 (blue2, cyan3): name1 is the colour at full brightness, then 238, 205, 139 of 255 of it
    let digit = n
        .chars()
        .last()?
        .to_digit(10)
        .filter(|d| (1..=4).contains(d))?;
    let (r, g, b) = NAMES
        .iter()
        .find(|(k, _)| **k == n[..n.len() - 1])
        .map(|(_, c)| *c)?;
    let top = r.max(g).max(b).max(1) as u32;
    let f = [255u32, 238, 205, 139][digit as usize - 1];
    let ch = |v: u8| ((v as u32 * 255 / top) * f / 255).min(255) as u8;
    Some((ch(r), ch(g), ch(b)))
}
