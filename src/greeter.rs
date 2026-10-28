//! loom as the login screen (docs/greeter.md). The user's loom keeps a snapshot of its look in the
//! greeter folder (/var/lib/loom/greeter, or LOOM_GREETER_DIR); `loom --greeter DIR` runs with
//! HOME=DIR, so the snapshot's .config/hypr/hyprland.conf gives it the user's screens and keyboard
//! through the config code every loom uses, and greeter.json the colours, font, cursor and name.

use std::path::{Path, PathBuf};

use crate::state::Loom;

/// Where the snapshot goes: LOOM_GREETER_DIR, else /var/lib/loom/greeter when this user may write it.
pub fn snapshot_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("LOOM_GREETER_DIR") {
        return Some(PathBuf::from(d));
    }
    let d = PathBuf::from("/var/lib/loom/greeter");
    let probe = d.join(".probe");
    std::fs::write(&probe, b"").ok()?;
    let _ = std::fs::remove_file(probe);
    Some(d)
}

/// `path` written whole or not at all (a greeter starting mid-write reads the old one).
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)
}

/// ~/.Xresources' Xcursor.theme and Xcursor.size (the desktop's cursor; the greeter used Adwaita 24).
fn xcursor() -> (String, u32) {
    let x = std::fs::read_to_string(crate::config::home().join(".Xresources")).unwrap_or_default();
    let get = |k: &str| x.lines().find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim_start_matches(':').trim().to_string()));
    (get("Xcursor.theme").unwrap_or_else(|| "default".into()), get("Xcursor.size").and_then(|s| s.parse().ok()).unwrap_or(24))
}

/// The user's full name (passwd's GECOS), else the login name.
fn real_name(user: &str) -> String {
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.first() == Some(&user)).then(|| f.get(4).map(|g| g.split(',').next().unwrap_or("").to_string()).unwrap_or_default())
    }).filter(|n| !n.is_empty()).unwrap_or_else(|| user.to_string())
}

impl Loom {
    /// The greeter's snapshot of this session's look, written when screens or the config change.
    pub fn greeter_snapshot(&self) {
        // the greeter itself never writes it (its home is the snapshot)
        if std::env::var_os("LOOM_GREETER_MODE").is_some() {
            return;
        }
        let Some(dir) = snapshot_dir() else { return };
        if let Err(e) = self.greeter_snapshot_to(&dir) {
            eprintln!("loom: greeter snapshot in {}: {e}", dir.display());
        }
    }

    fn greeter_snapshot_to(&self, dir: &Path) -> std::io::Result<()> {
        let home = crate::config::home();
        std::fs::create_dir_all(dir.join(".config/hypr"))?;
        // the screens as they are now (modes, places, scales loom resolved): no "auto" guessing
        let mut conf = String::from("# written by loom for the login screen (greeter.rs); the user's own config is not readable before login\n");
        for o in &self.outputs {
            let p = o.physical_properties();
            let (Some(m), Some(g)) = (o.current_mode(), self.space.output_geometry(o)) else { continue };
            conf += &format!("monitor=desc:{} {},{}x{}@{:.2},{}x{},{}\n", p.make, p.model, m.size.w, m.size.h, m.refresh as f64 / 1000.0, g.loc.x, g.loc.y, o.current_scale().fractional_scale());
        }
        let [model, layout, variant, options] = crate::config::xkb_names();
        let (delay, rate) = crate::config::key_repeat();
        conf += &format!("input {{\n    kb_model = {model}\n    kb_layout = {layout}\n    kb_variant = {variant}\n    kb_options = {options}\n    repeat_delay = {delay}\n    repeat_rate = {rate}\n}}\n");
        // ii's lock is see-through: with xray its wallpaper shows under it (else black)
        conf += &format!("misc {{\n    session_lock_xray = {}\n}}\n", self.look.lock_xray);
        write_atomic(&dir.join(".config/hypr/hyprland.conf"), conf.as_bytes())?;

        // ii's look: its generated colours, its font, its wallpaper (copied: the home is closed)
        let colors: serde_json::Value = std::fs::read_to_string(home.join(".local/state/quickshell/user/generated/colors.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let ii: serde_json::Value = std::fs::read_to_string(home.join(".config/illogical-impulse/config.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let font = ii.pointer("/appearance/fonts/main").or_else(|| ii.pointer("/font/family")).and_then(|v| v.as_str()).unwrap_or("sans-serif").to_string();
        let wall = ii.pointer("/background/wallpaperPath").and_then(|v| v.as_str()).map(PathBuf::from).filter(|p| p.is_file());
        let mut wall_name = None;
        if let Some(src) = wall {
            let name = format!("wallpaper.{}", src.extension().and_then(|e| e.to_str()).unwrap_or("png"));
            let dst = dir.join(&name);
            let same = |a: &Path, b: &Path| a.metadata().ok().zip(b.metadata().ok()).is_some_and(|(a, b)| a.len() == b.len() && a.modified().ok() <= b.modified().ok());
            if !same(&src, &dst) {
                let tmp = dst.with_extension("tmp");
                std::fs::copy(&src, &tmp)?;
                std::fs::rename(tmp, &dst)?;
            }
            wall_name = Some(name);
        }
        // the user's ii itself (their real lock screen, side panel and all) at the same paths under
        // the greeter's home, with loom's greeter entry and its LockContext put over ii's in the copy
        let (from, to) = (home.clone(), dir.to_path_buf());
        std::thread::spawn(move || mirror_ii(&from, &to));
        let user = std::env::var("USER").unwrap_or_default();
        let (cursor, cursor_size) = xcursor();
        let j = serde_json::json!({
            "user": user, "name": real_name(&user),
            "cursor": cursor, "cursorSize": cursor_size,
            "font": font, "wallpaper": wall_name, "colors": colors,
            // the lock screen's clock (ii's desktop clock settings: family, size, weight)
            "clock": ii.pointer("/background/widgets/clock/digital/font").cloned().unwrap_or_default(),
        });
        write_atomic(&dir.join("greeter.json"), serde_json::to_string_pretty(&j).unwrap_or_default().as_bytes())
    }
}

/// The greeter's entry (only ii's lock, locked from the start) and its LockContext (the password to
/// the login backend, the screen kept locked for the handover); dist/greeter/ii.
const II_GREETER: &str = include_str!("../dist/greeter/ii/greeter.qml");
const II_LOCK_CONTEXT: &str = include_str!("../dist/greeter/ii/LockContext.qml");
const II_SESSION_PICKER: &str = include_str!("../dist/greeter/ii/SessionPicker.qml");

/// ii, its config, its generated colours and the user's fonts mirrored into the greeter's home
/// (rsync: only changes; ii's .git left out), then the greeter's two files put into the copy.
fn mirror_ii(home: &Path, dir: &Path) {
    let ii = home.join(".config/quickshell/ii");
    if !ii.join("shell.qml").is_file() {
        return;
    }
    for rel in [".config/quickshell/ii", ".config/illogical-impulse", ".local/state/quickshell/user/generated", ".local/share/fonts"] {
        let (src, dst) = (home.join(rel), dir.join(rel));
        if !src.exists() {
            continue;
        }
        let _ = std::fs::create_dir_all(&dst);
        let ok = std::process::Command::new("rsync")
            .args(["-a", "--delete", "--exclude", ".git", "--exclude", "greeter.qml", "--exclude", "SessionPicker.qml"])
            .arg(format!("{}/", src.display()))
            .arg(format!("{}/", dst.display()))
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("loom: greeter snapshot: {rel} not copied (rsync)");
            return;
        }
    }
    let copy = dir.join(".config/quickshell/ii");
    let put = |rel: &str, text: &str| {
        let p = copy.join(rel);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(text) {
            let _ = write_atomic(&p, text.as_bytes());
        }
    };
    put("modules/common/panels/lock/LockContext.qml", II_LOCK_CONTEXT);
    put("greeter.qml", II_GREETER);
    put("SessionPicker.qml", II_SESSION_PICKER);
}

/// `loom --greeter DIR`: this process (and what it starts) sees DIR as its home, the user's cursor,
/// and runs the greeter QML (LOOM_GREETER_QML, else /usr/share/loom/greeter).
pub fn enter(dir: &Path, run: &mut Vec<String>) {
    let j: serde_json::Value = std::fs::read_to_string(dir.join("greeter.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    // Safety: still single-threaded (main, before any backend or thread starts)
    unsafe {
        std::env::set_var("HOME", dir);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("LOOM_GREETER_DIR", dir);
        std::env::set_var("LOOM_GREETER_MODE", "1");
        if let Some(c) = j["cursor"].as_str() {
            std::env::set_var("XCURSOR_THEME", c);
        }
        if let Some(s) = j["cursorSize"].as_u64() {
            std::env::set_var("XCURSOR_SIZE", s.to_string());
        }
    }
    if run.is_empty() {
        // the user's own lock screen (the snapshot's ii), else loom's plain greeter
        let ii = dir.join(".config/quickshell/ii/greeter.qml");
        let qml = if ii.is_file() && std::env::var_os("LOOM_GREETER_QML").is_none() {
            ii.to_string_lossy().into_owned()
        } else {
            std::env::var("LOOM_GREETER_QML").unwrap_or_else(|_| "/usr/share/loom/greeter".into())
        };
        *run = vec!["qs".into(), "-p".into(), qml];
    }
    eprintln!("loom: greeter from {} ({} {})", dir.display(), j["user"].as_str().unwrap_or("?"), j["cursor"].as_str().unwrap_or("default cursor"));
}
