//! `loom restart`: loom replaced by its (new) binary in place, same process, so the login session
//! stays. The Wayland socket is handed over open: programs reconnecting meanwhile wait in its
//! queue (Qt 6 ones do, QT_WAYLAND_RECONNECT=1); the display name, Hyprland signature and X display
//! stay the same. Programs that cannot reconnect (GTK, X ones) end, as with KDE's restart.

use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

/// The Wayland socket: the one handed over by the loom before this one, else a new one bound
/// (`name`, or the first free wayland-N). Returns it and its name; its lock file stays open.
pub fn wayland_listener(name: Option<&str>) -> Result<(UnixListener, String), Box<dyn std::error::Error>> {
    if let (Some(fd), Ok(n)) = (env_fd("LOOM_WAYLAND_FD"), std::env::var("LOOM_WAYLAND_NAME")) {
        // Safety: the fd was ours before the exec, handed over open on purpose
        let l = unsafe { UnixListener::from_raw_fd(fd) };
        l.set_nonblocking(true)?;
        set_cloexec(fd, true);
        if let Some(lock) = env_fd("LOOM_WAYLAND_LOCK_FD") {
            set_cloexec(lock, true);
            LOCK.with(|c| c.set(lock));
        }
        for k in ["LOOM_WAYLAND_FD", "LOOM_WAYLAND_NAME", "LOOM_WAYLAND_LOCK_FD"] {
            // Safety: one thread so far (before the event loop runs)
            unsafe { std::env::remove_var(k) };
        }
        return Ok((l, n));
    }
    let dir = PathBuf::from(std::env::var("XDG_RUNTIME_DIR")?);
    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => (1..33).map(|i| format!("wayland-{i}")).collect(),
    };
    for n in names {
        let lock_path = dir.join(format!("{n}.lock"));
        let Ok(lock) = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&lock_path) else { continue };
        // Safety: a valid fd; LOCK_NB so a held lock (another compositor) is skipped
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        let path = dir.join(&n);
        let _ = std::fs::remove_file(&path);
        let l = UnixListener::bind(&path)?;
        l.set_nonblocking(true)?;
        LOCK.with(|c| c.set(lock.into_raw_fd()));
        return Ok((l, n));
    }
    Err("no free Wayland socket name".into())
}

/// The Wayland socket (wayland_listener) taking clients in `handle`'s loop; returns its fd and name.
pub fn listen(handle: &smithay::reexports::calloop::LoopHandle<'static, crate::state::Loom>, name: Option<&str>) -> Result<(RawFd, String), Box<dyn std::error::Error>> {
    use smithay::reexports::calloop::{generic::Generic, Interest, Mode, PostAction};
    let (l, n) = wayland_listener(name)?;
    let fd = l.as_raw_fd();
    handle.insert_source(Generic::new(l, Interest::READ, Mode::Level), |_, l, state: &mut crate::state::Loom| {
        while let Ok((s, _)) = l.accept() {
            let _ = state.dh.insert_client(s, crate::state::client_data());
        }
        Ok(PostAction::Continue)
    })?;
    Ok((fd, n))
}

thread_local! {
    static LOCK: std::cell::Cell<RawFd> = const { std::cell::Cell::new(-1) };
}

fn env_fd(k: &str) -> Option<RawFd> {
    std::env::var(k).ok()?.parse().ok()
}

fn set_cloexec(fd: RawFd, on: bool) {
    // Safety: plain fcntl on an fd we own
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        if f >= 0 {
            libc::fcntl(fd, libc::F_SETFD, if on { f | libc::FD_CLOEXEC } else { f & !libc::FD_CLOEXEC });
        }
    }
}

/// What the loom before this one handed over (taken out of the environment at start, so the
/// programs loom starts never see it): restarted, its X display, its Hyprland signature.
static HANDED: std::sync::OnceLock<(bool, Option<u32>, Option<String>)> = std::sync::OnceLock::new();

/// Reads (and clears) the handed-over values; call first thing in main.
pub fn take_env() {
    let v = (std::env::var_os("LOOM_RESTARTED").is_some(), std::env::var("LOOM_X_DISPLAY").ok().and_then(|s| s.parse().ok()), std::env::var("LOOM_SIGNATURE").ok());
    for k in ["LOOM_RESTARTED", "LOOM_X_DISPLAY", "LOOM_SIGNATURE"] {
        // Safety: called before any thread starts
        unsafe { std::env::remove_var(k) };
    }
    let _ = HANDED.set(v);
    // the shell's open event-socket connections (quickshell does not reconnect them)
    if let Ok(s) = std::env::var("LOOM_HYPR_LISTENERS") {
        let fds: Vec<RawFd> = s.split(',').filter_map(|f| f.parse().ok()).collect();
        for &fd in &fds {
            set_cloexec(fd, true);
        }
        *LISTENERS.lock().unwrap() = fds;
        // Safety: called before any thread starts
        unsafe { std::env::remove_var("LOOM_HYPR_LISTENERS") };
    }
}

static LISTENERS: std::sync::Mutex<Vec<RawFd>> = std::sync::Mutex::new(Vec::new());

/// The event-socket connections handed over by the loom before this one.
pub fn hypr_listeners() -> Vec<std::os::unix::net::UnixStream> {
    // Safety: each fd was handed over open and is taken once
    std::mem::take(&mut *LISTENERS.lock().unwrap()).into_iter().map(|fd| unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) }).collect()
}

/// True when this loom came from `loom restart` (the session's programs are running already).
pub fn restarted() -> bool {
    HANDED.get().is_some_and(|h| h.0)
}

/// The Hyprland signature the loom before this one had.
pub fn signature() -> Option<String> {
    HANDED.get().and_then(|h| h.2.clone())
}

/// The new loom in place of this one. Called after the event loop and the session are dropped;
/// returns only when the exec failed.
pub fn exec(socket: RawFd, name: &str, signature: &str, x_display: Option<u32>) -> std::io::Error {
    set_cloexec(socket, false);
    let lock = LOCK.with(|c| c.get());
    // Safety: one thread left (the event loop is gone) and the exec follows
    unsafe {
        std::env::set_var("LOOM_WAYLAND_FD", socket.to_string());
        std::env::set_var("LOOM_WAYLAND_NAME", name);
        if lock >= 0 {
            set_cloexec(lock, false);
            std::env::set_var("LOOM_WAYLAND_LOCK_FD", lock.to_string());
        }
        std::env::set_var("LOOM_SIGNATURE", signature);
        if let Some(n) = x_display {
            std::env::set_var("LOOM_X_DISPLAY", n.to_string());
        }
        std::env::set_var("LOOM_RESTARTED", "1");
    }
    // the X display's lock holds this pid, alive after the exec: let go so it can be taken again
    if let Some(n) = x_display {
        let lock = format!("/tmp/.X{n}-lock");
        if std::fs::read_to_string(&lock).is_ok_and(|s| s.trim().parse::<u32>().ok() == Some(std::process::id())) {
            let _ = std::fs::remove_file(&lock);
            let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{n}"));
        }
    }
    // the installed binary (argv[0]); /proc/self/exe is the old, replaced file
    let mut args = std::env::args();
    let me = args.next().unwrap_or_default();
    let me = if me.contains('/') { me } else { std::env::current_exe().map(|p| p.to_string_lossy().replace(" (deleted)", "")).unwrap_or(me) };
    eprintln!("loom: restarting as {me}");
    use std::os::unix::process::CommandExt;
    std::process::Command::new(&me).args(args).exec()
}

/// The X display the loom before this one had (kept across a restart).
pub fn x_display() -> Option<u32> {
    HANDED.get().and_then(|h| h.1)
}

/// After the event loop ended: when a restart was asked, everything of this loom let go (the GPU,
/// the seat, Xwayland) and the new one exec'd with the Wayland socket kept open.
pub fn after_loop(state: crate::state::Loom, event_loop: smithay::reexports::calloop::EventLoop<'static, crate::state::Loom>, socket: RawFd, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !state.restart {
        return Ok(());
    }
    // a copy that outlives the loop's own (closed with it); dup clears close-on-exec
    // Safety: the fd is open (the loop still holds it)
    let kept = unsafe { libc::dup(socket) };
    let signature = state.hypr.as_ref().map(|h| h.signature.clone()).unwrap_or_default();
    // Safety: each listener is open; dup clears close-on-exec
    let listeners: Vec<String> = state.hypr.as_ref().map(|h| h.listeners.iter().map(|s| unsafe { libc::dup(s.as_raw_fd()) }).filter(|&fd| fd >= 0).map(|fd| fd.to_string()).collect()).unwrap_or_default();
    // Safety: one thread (the loop is done)
    unsafe { std::env::set_var("LOOM_HYPR_LISTENERS", listeners.join(",")) };
    save_workspaces(&state);
    let x = state.x_display;
    drop(state);
    drop(event_loop);
    if kept < 0 {
        return Err("restart: the Wayland socket could not be kept".into());
    }
    Err(Box::new(exec(kept, name, &signature, x)))
}

fn state_file() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("loom-restart.json")
}

/// Before the exec: which workspace each screen shows and which screen has the focus, and each
/// window's process and workspace (a program that reconnects keeps its process).
fn save_workspaces(state: &crate::state::Loom) {
    let windows: Vec<serde_json::Value> = state.managed.iter().filter(|m| m.pid > 0).map(|m| serde_json::json!([m.pid, m.ws])).collect();
    let v = serde_json::json!({
        "shown": state.shown_on,
        "focused": state.output.as_ref().map(|o| o.name()),
        "windows": windows,
        "night": [state.night.temperature, state.night.gamma],
    });
    let _ = std::fs::write(state_file(), v.to_string());
}

/// After a restart, once the screens are there: the workspaces they showed, the focus where it was,
/// and each reconnecting window back on its workspace (through the exec rules: first window of a
/// process). The shell hears the workspace events and redraws its bar.
pub fn restore_workspaces(state: &mut crate::state::Loom) {
    if !restarted() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(state_file()) else { return };
    let _ = std::fs::remove_file(state_file());
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    // the night light as it was (loom's own: nightlight.rs)
    if let Some(n) = v["night"].as_array() {
        state.night.temperature = n.first().and_then(|t| t.as_u64()).map(|t| t as u32);
        state.night.gamma = n.get(1).and_then(|g| g.as_u64()).map(|g| g as u32).unwrap_or(100);
        state.night_apply();
    }
    for w in v["windows"].as_array().into_iter().flatten() {
        if let (Some(pid), Some(ws)) = (w[0].as_u64(), w[1].as_i64()) {
            // silent: the window goes there without switching the screen to it
            state.exec_rules.push((pid as u32, ws as i32, true));
        }
    }
    let outputs = state.outputs.clone();
    if let Some(shown) = v["shown"].as_object() {
        for o in &outputs {
            if let Some(ws) = shown.get(&o.name()).and_then(|n| n.as_i64()) {
                state.focus_output(o);
                state.switch_workspace(ws as i32);
            }
        }
    }
    if let Some(f) = v["focused"].as_str().and_then(|n| outputs.iter().find(|o| o.name() == n)).cloned() {
        state.focus_output(&f);
    }
    // the shell kept its event socket through the restart, so it never read the state again:
    // its workspaces and windows stayed as before (old addresses). configreloaded makes Quickshell
    // read monitors, workspaces and windows anew (ii also re-reads keybinds, layouts, the night
    // light); once now and once when the windows that reconnect are back
    state.hypr_event("configreloaded>>");
    if let Some(h) = state.loop_handle.clone() {
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        let _ = h.insert_source(Timer::from_duration(std::time::Duration::from_secs(3)), |_, _, s: &mut crate::state::Loom| {
            s.hypr_event("configreloaded>>");
            TimeoutAction::Drop
        });
    }
}
