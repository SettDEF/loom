//! loom's own X server (loom-x) for the programs init.lua names: started with the first of them at the screen's pixels and scale, ending by itself when idle.

use std::process::{Child, Command, Stdio};

use crate::state::Loom;

#[derive(Default)]
pub struct OwnX {
    child: Option<Child>,
    display: u32,
}

/// The program a shell command runs (after `env` and VAR=value words), by its file name.
pub fn program(cmd: &str) -> Option<String> {
    cmd.split_whitespace()
        .filter(|w| *w != "env" && !(w.contains('=') && !w.starts_with('/')) && !w.starts_with('-'))
        .map(|w| w.trim_matches(['\'', '"']))
        .next()
        .map(|w| w.rsplit('/').next().unwrap_or(w).to_string())
}

/// A display number nobody holds.
fn free_display(from: u32) -> u32 {
    (from..from + 64).find(|n| !std::path::Path::new(&format!("/tmp/.X{n}-lock")).exists() && !std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()).unwrap_or(from)
}

impl Loom {
    /// The display for `cmd` when it is one of the own_x programs: loom-x running (started if needed).
    pub fn own_x_for(&self, cmd: &str) -> Option<u32> {
        let name = program(cmd)?;
        if !self.look.own_x.iter().any(|p| *p == name) {
            return None;
        }
        let mut x = self.own_x.borrow_mut();
        if let Some(c) = x.child.as_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                return Some(x.display);
            }
        }
        // next to loom's own binary
        let exe = std::env::current_exe().ok()?.with_file_name("loom-x");
        let o = self.output.as_ref()?;
        let size = o.current_mode().map(|m| m.size).unwrap_or((1920, 1080).into());
        let scale = o.current_scale().fractional_scale();
        let n = free_display(self.x_display.map(|d| d + 1).unwrap_or(20).max(20));
        let mut c = Command::new(exe);
        c.arg(format!(":{n}")).arg(format!("{}x{}", size.w, size.h)).arg("--scale").arg(format!("{scale}")).stdin(Stdio::null());
        if let Some(s) = &self.socket_name {
            c.env("WAYLAND_DISPLAY", s);
        }
        match c.spawn() {
            Ok(child) => {
                eprintln!("loom: loom-x started on DISPLAY=:{n} for {name}");
                x.child = Some(child);
                x.display = n;
                Some(n)
            }
            Err(e) => {
                eprintln!("loom: loom-x did not start ({e}); {name} gets Xwayland");
                None
            }
        }
    }
}

/// `cmd` run once display `n`'s socket is there (loom-x takes a moment to start; loom does not wait).
pub fn wait_for(n: u32, cmd: &str) -> String {
    format!("i=0; while [ ! -S /tmp/.X11-unix/X{n} ] && [ $i -lt 100 ]; do sleep 0.02; i=$((i+1)); done; {cmd}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn program_names() {
        assert_eq!(super::program("xterm -e top").as_deref(), Some("xterm"));
        assert_eq!(super::program("/usr/bin/xeyes").as_deref(), Some("xeyes"));
        assert_eq!(super::program("env FOO=1 xclock -digital").as_deref(), Some("xclock"));
        assert_eq!(super::program("GDK_SCALE=2 'xterm'").as_deref(), Some("xterm"));
    }
}
