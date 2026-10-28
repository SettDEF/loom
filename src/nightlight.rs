//! loom's own night light, in place of hyprsunset: the same socket in the instance folder
//! (`.hyprsunset.sock`) and the same text commands, so `hyprctl hyprsunset temperature 4150` (ii's
//! toggle and slider) works unchanged. The colour is set by loom on every screen's CTM (tty.rs
//! set_ctm), a screen plugged in later gets it too, and `loom restart` keeps it (restart.rs):
//! hyprsunset, a separate client, ended with its connection and took the night light with it.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::Duration;

use smithay::reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction};

use crate::state::Loom;

const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

/// What the night light is set to: a temperature (None: off) and a gamma in percent.
#[derive(Debug, Clone, Copy)]
pub struct Night {
    pub temperature: Option<u32>,
    pub gamma: u32,
}

impl Default for Night {
    fn default() -> Self {
        Night { temperature: None, gamma: 100 }
    }
}

impl Night {
    /// hyprsunset's colour for a temperature (Tanner Helland's fit, in its steps of 100 K), times
    /// the gamma: 4150 K = 1.0, 0.82, 0.67 as hyprsunset gave
    pub fn matrix(&self) -> [f64; 9] {
        let Some(k) = self.temperature else {
            if self.gamma == 100 {
                return IDENTITY;
            }
            let g = self.gamma as f64 / 100.0;
            return [g, 0.0, 0.0, 0.0, g, 0.0, 0.0, 0.0, g];
        };
        let t = (k / 100) as f64;
        let (r, g, b) = if t <= 66.0 {
            let g = (99.4708025861 * t.ln() - 161.1195681661).clamp(0.0, 255.0);
            let b = if t <= 19.0 { 0.0 } else { ((t - 10.0).ln() * 138.5177312231 - 305.0447927307).clamp(0.0, 255.0) };
            (255.0, g, b)
        } else {
            ((329.698727446 * (t - 60.0).powf(-0.1332047592)).clamp(0.0, 255.0), (288.1221695283 * (t - 60.0).powf(-0.0755148492)).clamp(0.0, 255.0), 255.0)
        };
        let s = self.gamma as f64 / 100.0;
        [r / 255.0 * s, 0.0, 0.0, 0.0, g / 255.0 * s, 0.0, 0.0, 0.0, b / 255.0 * s]
    }

    /// one command as hyprsunset takes it: `temperature [N|+N|-N]`, `gamma [N|+N|-N]`, `identity`;
    /// off reads as 6500, hyprsunset's own default (ii takes 6500 for off)
    pub fn command(&mut self, line: &str) -> String {
        let mut w = line.split_whitespace();
        let (what, arg) = (w.next().unwrap_or(""), w.next());
        // a number, or a step from the current value
        let value = |cur: u32, a: &str| -> Option<u32> {
            if let Some(d) = a.strip_prefix('+') {
                d.parse::<u32>().ok().map(|d| cur.saturating_add(d))
            } else if let Some(d) = a.strip_prefix('-') {
                d.parse::<u32>().ok().map(|d| cur.saturating_sub(d))
            } else {
                a.parse().ok()
            }
        };
        match (what, arg) {
            ("temperature", None) => self.temperature.unwrap_or(6500).to_string(),
            ("temperature", Some(a)) => match value(self.temperature.unwrap_or(6500), a) {
                Some(k) => {
                    self.temperature = Some(k.clamp(1000, 20000));
                    "ok".into()
                }
                None => "invalid temperature".into(),
            },
            ("gamma", None) => self.gamma.to_string(),
            ("gamma", Some(a)) => match value(self.gamma, a) {
                Some(g) => {
                    self.gamma = g.clamp(10, 200);
                    "ok".into()
                }
                None => "invalid gamma".into(),
            },
            ("identity", _) | ("reset", _) => {
                *self = Night::default();
                "ok".into()
            }
            _ => "unknown command (temperature, gamma, identity)".into(),
        }
    }
}

impl Loom {
    /// Every screen's colour from the night light.
    pub fn night_apply(&mut self) {
        let m = self.night.matrix();
        for o in self.outputs.clone() {
            self.set_ctm(&o.name(), m);
        }
    }
}

/// The socket hyprsunset would have (`.hyprsunset.sock` beside loom's own in the instance folder).
pub fn start(handle: &LoopHandle<'static, Loom>, dir: &Path) -> std::io::Result<()> {
    let path = dir.join(".hyprsunset.sock");
    let _ = std::fs::remove_file(&path);
    let l = UnixListener::bind(&path)?;
    l.set_nonblocking(true)?;
    handle
        .insert_source(Generic::new(l, Interest::READ, Mode::Level), |_, l, state: &mut Loom| {
            while let Ok((mut s, _)) = l.accept() {
                // a command is a few bytes, sent at once (hyprctl); a short wait for it at most
                let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
                let mut buf = [0u8; 256];
                let n = s.read(&mut buf).unwrap_or(0);
                let line = String::from_utf8_lossy(&buf[..n]).to_string();
                let before = state.night.matrix();
                let reply = state.night.command(&line);
                if state.night.matrix() != before {
                    state.night_apply();
                }
                let _ = s.set_write_timeout(Some(Duration::from_millis(50)));
                let _ = s.write_all(reply.as_bytes());
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_as_hyprsunset() {
        let n = Night { temperature: Some(4150), gamma: 100 };
        let m = n.matrix();
        assert!((m[0] - 1.0).abs() < 1e-3 && (m[4] - 0.8164).abs() < 2e-3 && (m[8] - 0.6680).abs() < 2e-3, "{m:?}");
        assert_eq!(Night::default().matrix(), IDENTITY);
    }

    #[test]
    fn commands() {
        let mut n = Night::default();
        assert_eq!(n.command("temperature"), "6500");
        assert_eq!(n.command("temperature 4150"), "ok");
        assert_eq!(n.command("temperature +100"), "ok");
        assert_eq!(n.command("temperature"), "4250");
        assert_eq!(n.command("gamma 80"), "ok");
        assert_eq!(n.command("identity"), "ok");
        assert_eq!(n.temperature, None);
        assert_eq!(n.gamma, 100);
        assert_eq!(n.command("temperature abc"), "invalid temperature");
    }
}
