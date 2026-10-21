//! Input, scripted for the headless backend: `--keys "text<CR>"` types into the focused window,
//! `--click X,Y` clicks there (and focuses the window). The same seat calls a real keyboard and
//! mouse make, so what a test types reaches clients the way a person's would.

use smithay::{
    backend::input::KeyState,
    input::keyboard::Keycode,
};

use crate::state::Loom;

/// One step of a script.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// an evdev key code, with Shift and Ctrl held around it
    Key { code: u32, shift: bool, ctrl: bool },
    /// `<W-x>`: x with Super held (Super = 125)
    Chord { held: u32, code: u32 },
    Click(f64, f64),
    /// `--sdrag X,Y,X2,Y2[,BUTTON]`: a Super+drag (a mouse bind), button 272 left / 273 right
    SuperDrag { from: (f64, f64), to: (f64, f64), button: u32 },
    /// `--sscroll X,Y,up|down`: Super+scroll there
    SuperScroll { at: (f64, f64), down: bool },
}

/// A US-layout key for a character: (evdev code, shifted).
fn key_of(c: char) -> Option<(u32, bool)> {
    const LETTERS: &str = "qwertyuiopasdfghjklzxcvbnm";
    const CODES: [u32; 26] = [16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 30, 31, 32, 33, 34, 35, 36, 37, 38, 44, 45, 46, 47, 48, 49, 50];
    if let Some(i) = LETTERS.find(c.to_ascii_lowercase()) {
        return Some((CODES[i], c.is_ascii_uppercase()));
    }
    let plain = "1234567890-=[];'`\\,./";
    let shifted = "!@#$%^&*()_+{}:\"~|<>?";
    let codes = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 26, 27, 39, 40, 41, 43, 51, 52, 53];
    if let Some(i) = plain.find(c) {
        return Some((codes[i], false));
    }
    if let Some(i) = shifted.find(c) {
        return Some((codes[i], true));
    }
    match c {
        ' ' => Some((57, false)),
        '\n' => Some((28, false)),
        '\t' => Some((15, false)),
        _ => None,
    }
}

/// `--keys` text into actions: characters, and <CR> <Tab> <BS> <Esc> <C-x>.
pub fn parse_keys(s: &str) -> Vec<Action> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '<' {
            if let Some(end) = rest.find('>') {
                let name = &rest[1..end];
                if let Some(k) = name.strip_prefix("W-").and_then(|n| if n.chars().count() == 1 { key_of(n.chars().next().unwrap_or(' ')).map(|x| x.0) } else { match n { "CR" | "Enter" => Some(28), "Space" => Some(57), "Esc" => Some(1), _ => None } }) {
                    out.push(Action::Chord { held: 125, code: k });
                    rest = &rest[end + 1..];
                    continue;
                }
                let named = match name {
                    "CR" | "Enter" => Some((28, false, false)),
                    "Tab" => Some((15, false, false)),
                    "BS" => Some((14, false, false)),
                    "Esc" => Some((1, false, false)),
                    "Space" => Some((57, false, false)),
                    "Super" | "W" => Some((125, false, false)),
                    n if n.starts_with("C-") && n.chars().count() == 3 => key_of(n.chars().nth(2).unwrap_or(' ')).map(|(k, s)| (k, s, true)),
                    _ => None,
                };
                if let Some((code, shift, ctrl)) = named {
                    out.push(Action::Key { code, shift, ctrl });
                    rest = &rest[end + 1..];
                    continue;
                }
            }
        }
        if let Some((code, shift)) = key_of(c) {
            out.push(Action::Key { code, shift, ctrl: false });
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

impl Loom {
    /// One scripted action, now.
    pub fn act(&mut self, a: &Action) {
        let time = self.now_ms();
        match *a {
            Action::Key { code, shift, ctrl } => {
                let held: Vec<u32> = [(ctrl, 29u32), (shift, 42u32)].into_iter().filter(|h| h.0).map(|h| h.1).collect();
                let press = |s: &mut Loom, k: u32, st: KeyState| {
                    s.key_input(Keycode::new(k + 8), st, time);
                };
                for &h in &held {
                    press(self, h, KeyState::Pressed);
                }
                press(self, code, KeyState::Pressed);
                press(self, code, KeyState::Released);
                for &h in held.iter().rev() {
                    press(self, h, KeyState::Released);
                }
            }
            Action::Chord { held, code } => {
                self.key_input(Keycode::new(held + 8), KeyState::Pressed, time);
                self.key_input(Keycode::new(code + 8), KeyState::Pressed, time);
                self.key_input(Keycode::new(code + 8), KeyState::Released, time);
                self.key_input(Keycode::new(held + 8), KeyState::Released, time);
            }
            Action::Click(x, y) => {
                self.pointer_move((x, y).into(), time);
                self.pointer_button(0x110, true, time);
                self.pointer_button(0x110, false, time);
            }
            Action::SuperDrag { from, to, button } => {
                // Super held, the button down at `from`, moved there in steps, let go
                self.key_input(Keycode::new(125 + 8), KeyState::Pressed, time);
                self.pointer_move(from.into(), time);
                self.pointer_button(button, true, time);
                for i in 1..=8 {
                    let t = i as f64 / 8.0;
                    self.pointer_move((from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t).into(), time);
                }
                self.pointer_button(button, false, time);
                self.key_input(Keycode::new(125 + 8), KeyState::Released, time);
            }
            Action::SuperScroll { at, down } => {
                self.key_input(Keycode::new(125 + 8), KeyState::Pressed, time);
                self.pointer_move(at.into(), time);
                self.pointer_scroll(smithay::backend::input::AxisSource::Wheel, if down { 15.0 } else { -15.0 }, 0.0, Some((if down { 120.0 } else { -120.0 }, 0.0)), time);
                self.key_input(Keycode::new(125 + 8), KeyState::Released, time);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse() {
        assert_eq!(parse_keys("aB<CR>"), vec![Action::Key { code: 30, shift: false, ctrl: false }, Action::Key { code: 48, shift: true, ctrl: false }, Action::Key { code: 28, shift: false, ctrl: false }]);
        assert_eq!(parse_keys("<C-c>"), vec![Action::Key { code: 46, shift: false, ctrl: true }]);
        assert_eq!(parse_keys("!"), vec![Action::Key { code: 2, shift: true, ctrl: false }]);
    }
}
