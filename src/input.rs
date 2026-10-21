//! Input, scripted for the headless backend: `--keys "text<CR>"` types into the focused window,
//! `--click X,Y` clicks there (and focuses the window). The same seat calls a real keyboard and
//! mouse make, so what a test types reaches clients the way a person's would.

use smithay::{
    backend::input::{ButtonState, KeyState},
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{ButtonEvent, MotionEvent},
    },
    utils::{Point, SERIAL_COUNTER},
};

use crate::state::Loom;

/// One step of a script.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// an evdev key code, with Shift and Ctrl held around it
    Key { code: u32, shift: bool, ctrl: bool },
    Click(f64, f64),
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
                let named = match name {
                    "CR" | "Enter" => Some((28, false, false)),
                    "Tab" => Some((15, false, false)),
                    "BS" => Some((14, false, false)),
                    "Esc" => Some((1, false, false)),
                    "Space" => Some((57, false, false)),
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
                let kb = self.seat.get_keyboard().expect("keyboard");
                let held: Vec<u32> = [(ctrl, 29u32), (shift, 42u32)].into_iter().filter(|h| h.0).map(|h| h.1).collect();
                let press = |s: &mut Loom, k: u32, st: KeyState| {
                    kb.input::<(), _>(s, Keycode::new(k + 8), st, SERIAL_COUNTER.next_serial(), time, |_, _, _| FilterResult::Forward);
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
            Action::Click(x, y) => {
                let at: Point<f64, _> = (x, y).into();
                let under = self.space.element_under(at).map(|(w, loc)| (w.clone(), loc));
                let pointer = self.seat.get_pointer().expect("pointer");
                let focus = under.as_ref().and_then(|(w, loc)| {
                    w.surface_under(at - loc.to_f64(), smithay::desktop::WindowSurfaceType::ALL).map(|(s, sloc)| (s, (sloc + *loc).to_f64()))
                });
                pointer.motion(self, focus, &MotionEvent { location: at, serial: SERIAL_COUNTER.next_serial(), time });
                for st in [ButtonState::Pressed, ButtonState::Released] {
                    pointer.button(self, &ButtonEvent { button: 0x110, state: st, serial: SERIAL_COUNTER.next_serial(), time });
                }
                pointer.frame(self);
                // the window clicked comes up and has the keyboard
                if let Some((w, _)) = under {
                    self.focus_window(&w);
                }
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
