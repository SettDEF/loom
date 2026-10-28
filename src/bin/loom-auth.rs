//! loom-auth: the login screen's password check (docs/greeter.md), the one place PAM runs. The
//! greeter's page connects to its socket and sends one line, {"user": …, "password": …}; the answer
//! is one line, {"ok": true} or {"ok": false, "message": …}. Runs as root on the real seat; only the
//! greeter's uid may ask (SO_PEERCRED). `--mock USER:PASS` answers without PAM (headless tests).
//!
//! Usage: loom-auth [--socket PATH] [--service NAME] [--allow-uid UID | --allow-uid-of USER] [--mock USER:PASS]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_CONV_ERR: c_int = 19;
const PAM_ESTABLISH_CRED: c_int = 0x2;
const PAM_NEW_AUTHTOK_REQD: c_int = 12;
const PAM_USER: c_int = 2;
const PAM_DELETE_CRED: c_int = 0x4;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const PamConv, handle: *mut *mut c_void) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut c_void, err: c_int) -> *const c_char;
    fn pam_setcred(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_open_session(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_close_session(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_putenv(handle: *mut c_void, name_value: *const c_char) -> c_int;
    fn pam_getenvlist(handle: *mut c_void) -> *mut *mut c_char;
    fn pam_get_item(handle: *mut c_void, item: c_int, out: *mut *const c_void) -> c_int;
}

/// What the conversation needs: the password to give, the messages PAM said (shown to the user).
struct Talk {
    user: CString,
    password: CString,
    said: Vec<String>,
}

/// PAM's questions: a hidden prompt gets the password, an echoed one too (some modules ask so),
/// messages are kept. Responses are malloc'd: PAM frees them.
extern "C" fn converse(n: c_int, msgs: *mut *const PamMessage, out: *mut *mut PamResponse, data: *mut c_void) -> c_int {
    if n <= 0 || msgs.is_null() || out.is_null() || data.is_null() {
        return PAM_CONV_ERR;
    }
    // Safety: PAM hands n messages and our Talk; the replies are calloc'd as PAM expects
    unsafe {
        let talk = &mut *(data as *mut Talk);
        let replies = libc::calloc(n as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
        if replies.is_null() {
            return PAM_CONV_ERR;
        }
        for i in 0..n as usize {
            // Linux-PAM: msgs points to an array of n message pointers
            let m = &**msgs.add(i);
            match m.msg_style {
                // hidden: the password; echoed: PAM asking who (the name, never the password: it
                // would land in the journal as a user name)
                PAM_PROMPT_ECHO_OFF => (*replies.add(i)).resp = libc::strdup(talk.password.as_ptr()),
                PAM_PROMPT_ECHO_ON => (*replies.add(i)).resp = libc::strdup(talk.user.as_ptr()),
                PAM_ERROR_MSG | PAM_TEXT_INFO if !m.msg.is_null() => talk.said.push(CStr::from_ptr(m.msg).to_string_lossy().into_owned()),
                _ => {}
            }
        }
        *out = replies;
    }
    PAM_SUCCESS
}

/// A PAM transaction that passed: kept for the session it opens (pam_end when dropped).
struct Pam {
    h: *mut c_void,
    _talk: Box<Talk>,
}

impl Pam {
    /// the user as PAM has it now (a module may have changed the spelling)
    fn user(&self) -> Option<String> {
        let mut out: *const c_void = std::ptr::null();
        // Safety: our handle; PAM_USER is a C string owned by PAM
        let r = unsafe { pam_get_item(self.h, PAM_USER, &mut out) };
        (r == PAM_SUCCESS && !out.is_null()).then(|| unsafe { CStr::from_ptr(out as *const c_char) }.to_string_lossy().into_owned())
    }
}

impl Drop for Pam {
    fn drop(&mut self) {
        // Safety: our handle, ended once
        unsafe { pam_end(self.h, PAM_SUCCESS) };
    }
}

/// The password checked by PAM's `service` for `user` (and the account allowed now: not expired).
fn pam_login(service: &str, user: &str, password: String) -> Result<Pam, String> {
    let (Ok(svc), Ok(usr)) = (CString::new(service), CString::new(user)) else { return Err("bad name".into()) };
    let mut bytes = password.into_bytes();
    let pw = CString::new(bytes.clone()).map_err(|_| "bad password".to_string());
    bytes.fill(0);
    let mut talk = Box::new(Talk { user: usr.clone(), password: pw?, said: Vec::new() });
    // Linux-PAM copies the conv struct; appdata (the boxed Talk) lives in Pam as long as the handle
    let conv = PamConv { conv: converse, appdata: &mut *talk as *mut Talk as *mut c_void };
    let mut h: *mut c_void = std::ptr::null_mut();
    // Safety: plain PAM calls on a handle we started; ended on failure here, else by Pam's drop
    let (r, why) = unsafe {
        let mut r = pam_start(svc.as_ptr(), usr.as_ptr(), &conv, &mut h);
        if r == PAM_SUCCESS {
            r = pam_authenticate(h, 0);
        }
        if r == PAM_SUCCESS {
            r = pam_acct_mgmt(h, 0);
        }
        let why = match r {
            PAM_SUCCESS => String::new(),
            PAM_NEW_AUTHTOK_REQD => "The password has expired: change it on a console (Ctrl+Alt+F3, passwd)".into(),
            _ => CStr::from_ptr(pam_strerror(h, r)).to_string_lossy().into_owned(),
        };
        if r != PAM_SUCCESS && !h.is_null() {
            pam_end(h, r);
        }
        (r, why)
    };
    // the password's bytes cleared: PAM has asked for it already
    let mut left = std::mem::take(&mut talk.password).into_bytes_with_nul();
    left.fill(0);
    if r == PAM_SUCCESS {
        Ok(Pam { h, _talk: talk })
    } else {
        Err(if r == PAM_NEW_AUTHTOK_REQD { why } else { talk.said.pop().unwrap_or(why) })
    }
}

/// The uid on the other end of the socket.
fn peer_uid(s: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // Safety: getsockopt into a ucred of the right size
    let r = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut c_void, &mut len) };
    (r == 0).then_some(cred.uid)
}

struct Opts {
    socket: String,
    service: String,
    allow_uid: u32,
    mock: Option<(String, String)>,
    /// what the session runs, by the user's login shell (-l -c)
    session: String,
    vt: Option<u32>,
    /// the greeter's systemd unit: stopped once the session has the screen, started when it ends
    greeter_unit: Option<String>,
}

/// The user's passwd entry: uid, gid, home, shell.
struct User {
    name: CString,
    uid: u32,
    gid: u32,
    home: String,
    shell: String,
}

fn user(name: &str) -> Option<User> {
    let c = CString::new(name).ok()?;
    // Safety: getpwnam's result is read at once, before another passwd call
    unsafe {
        let p = libc::getpwnam(c.as_ptr());
        if p.is_null() {
            return None;
        }
        let s = |x: *const c_char| CStr::from_ptr(x).to_string_lossy().into_owned();
        Some(User { name: c.clone(), uid: (*p).pw_uid, gid: (*p).pw_gid, home: s((*p).pw_dir), shell: s((*p).pw_shell) })
    }
}

/// The session's process: as the user (groups, gid, uid dropped in that order), in their home, the
/// environment PAM gave (pam_systemd's XDG_RUNTIME_DIR, XDG_SESSION_ID …) and the login basics,
/// the session command run by their login shell. Only returns on failure.
fn exec_session(u: &User, env: &[String], cmd: &str) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new(&u.shell);
    c.arg("-l").arg("-c").arg(cmd).current_dir(&u.home).env_clear();
    for kv in env {
        if let Some((k, v)) = kv.split_once('=') {
            c.env(k, v);
        }
    }
    let name = u.name.to_string_lossy().into_owned();
    c.env("HOME", &u.home).env("USER", &name).env("LOGNAME", &name).env("SHELL", &u.shell).env("PATH", "/usr/local/bin:/usr/bin");
    // Safety: getuid has no preconditions
    if unsafe { libc::getuid() } == 0 {
        let (uid, gid, name) = (u.uid, u.gid, u.name.clone());
        // Safety: async-signal-safe calls only, in the child before exec
        unsafe {
            c.pre_exec(move || {
                if libc::initgroups(name.as_ptr(), gid) != 0 || libc::setgid(gid) != 0 || libc::setuid(uid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    c.exec()
}

/// After a sign-in, the session: as root through PAM (credentials, pam_open_session: logind makes
/// the session, the runtime dir), in a worker process that waits for it and closes it; as an
/// ordinary user (tests) the command only. The worker's pid is returned.
fn start_session(o: &Opts, name: &str, pam: Option<Pam>) -> Result<u32, String> {
    let u = user(name).ok_or_else(|| format!("no user {name}"))?;
    // the worker says how the start went: nothing written and closed = the session runs
    let mut fds = [0; 2];
    // Safety: a pipe for the worker's report
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // Safety: fork in a single-threaded program; the child only does PAM and exec
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if pid > 0 {
        // the transaction is the worker's now (its copy); ours goes without closing anything
        if let Some(p) = pam {
            std::mem::forget(p);
        }
        // Safety: our pipe's ends
        unsafe { libc::close(fds[1]) };
        let report = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut m = String::new();
        let _ = (&report).take(4096).read_to_string(&mut m);
        return if m.is_empty() { Ok(pid as u32) } else { Err(m.trim().to_string()) };
    }
    // the worker: it waits for the session, so children are its own to reap again (main ignores them)
    // Safety: plain signal disposition; our pipe's ends
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
        libc::close(fds[0]);
    }
    let report = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    let restart_greeter = |o: &Opts| {
        if let Some(unit) = &o.greeter_unit {
            let ok = std::process::Command::new("systemctl").args(["restart", "--no-block", unit]).status().is_ok_and(|s| s.success());
            eprintln!("loom-auth: systemctl restart {unit}: {}", if ok { "ok" } else { "failed" });
        }
    };
    let fail = |m: String| -> ! {
        eprintln!("loom-auth: session for {name}: {m}");
        let _ = writeln!(&report, "{m}");
        std::process::exit(1)
    };
    let mut env: Vec<String> = Vec::new();
    if let Some(p) = &pam {
        let vt = o.vt.map(|v| v.to_string()).unwrap_or_default();
        // pam_systemd reads these for the logind session it makes
        for kv in ["XDG_SESSION_TYPE=wayland", "XDG_SESSION_CLASS=user", "XDG_SESSION_DESKTOP=loom", "XDG_CURRENT_DESKTOP=Hyprland", "XDG_SEAT=seat0", &format!("XDG_VTNR={vt}")] {
            if kv.ends_with('=') {
                continue;
            }
            let c = CString::new(kv).unwrap_or_default();
            // Safety: our handle, a valid C string PAM copies
            unsafe { pam_putenv(p.h, c.as_ptr()) };
        }
        // Safety: our handle; the env list is malloc'd strings PAM gives us to free
        unsafe {
            let r = pam_setcred(p.h, PAM_ESTABLISH_CRED);
            if r != PAM_SUCCESS {
                fail(format!("setcred: {}", CStr::from_ptr(pam_strerror(p.h, r)).to_string_lossy()));
            }
            let r = pam_open_session(p.h, 0);
            if r != PAM_SUCCESS {
                fail(format!("The session could not be opened: {}", CStr::from_ptr(pam_strerror(p.h, r)).to_string_lossy()));
            }
            let list = pam_getenvlist(p.h);
            if !list.is_null() {
                let mut i = 0;
                while !(*list.add(i)).is_null() {
                    env.push(CStr::from_ptr(*list.add(i)).to_string_lossy().into_owned());
                    libc::free(*list.add(i) as *mut c_void);
                    i += 1;
                }
                libc::free(list as *mut c_void);
            }
        }
    } else {
        // tests: this user's own environment (the runtime dir, the display)
        env = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    }
    eprintln!("loom-auth: session for {name}: {}", o.session);
    // the session's exec reported on a second pipe: closed by a successful exec (CLOEXEC)
    let mut ex = [0; 2];
    // Safety: a pipe for the exec's result
    if unsafe { libc::pipe2(ex.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        fail(std::io::Error::last_os_error().to_string());
    }
    // Safety: fork again: the session process, the worker waits for it
    let child = unsafe { libc::fork() };
    if child == 0 {
        let e = exec_session(&u, &env, &o.session);
        let w = unsafe { std::fs::File::from_raw_fd(ex[1]) };
        let _ = writeln!(&w, "The session did not start: {e}");
        std::process::exit(127);
    }
    // Safety: our pipe's ends
    unsafe { libc::close(ex[1]) };
    let mut m = String::new();
    let _ = (&unsafe { std::fs::File::from_raw_fd(ex[0]) }).take(4096).read_to_string(&mut m);
    if !m.is_empty() {
        // Safety: reaping the failed child
        unsafe { libc::waitpid(child, std::ptr::null_mut(), 0) };
        if let Some(p) = &pam {
            unsafe {
                pam_close_session(p.h, 0);
                pam_setcred(p.h, PAM_DELETE_CRED);
            }
        }
        fail(m.trim().to_string());
    }
    // started: the greeter hears it now
    drop(report);
    // the greeter goes once the session has the screen (its loom shows the greeter's last picture by then)
    if let Some(unit) = o.greeter_unit.clone() {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(8));
            let ok = std::process::Command::new("systemctl").args(["stop", "--no-block", &unit]).status().is_ok_and(|s| s.success());
            eprintln!("loom-auth: systemctl stop {unit}: {}", if ok { "ok" } else { "failed" });
        });
    }
    let mut status = 0;
    // Safety: waiting for our own child
    unsafe { libc::waitpid(child, &mut status, 0) };
    eprintln!("loom-auth: session for {name} ended ({status})");
    if let Some(p) = &pam {
        // Safety: our handle
        unsafe {
            pam_close_session(p.h, 0);
            pam_setcred(p.h, PAM_DELETE_CRED);
        }
    }
    drop(pam);
    // logged out (or the session died early, the greeter maybe still up): the login screen, fresh
    restart_greeter(o);
    std::process::exit(0)
}

/// A session's Exec= from /usr/share/wayland-sessions (only a plain file name there: nothing else runs).
fn session_exec(file: &str) -> Option<String> {
    if file.contains('/') || !file.ends_with(".desktop") {
        return None;
    }
    let text = std::fs::read_to_string(std::path::Path::new("/usr/share/wayland-sessions").join(file)).ok()?;
    let exec = text.lines().find_map(|l| l.strip_prefix("Exec="))?.trim();
    // desktop-entry field codes (%f, %u …) are not for a session
    Some(exec.split_whitespace().filter(|w| !w.starts_with('%')).collect::<Vec<_>>().join(" "))
}

/// One sign-in: {user, password} → {ok} or {ok: false, message}; after ok, {start: true} →
/// {started: true}: the greeter took its last picture first (the new session's curtain).
fn serve(o: &Opts, s: &UnixStream) {
    // a caller that connects and says nothing must not hold the one sign-in there is
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    let mut rd = BufReader::new(s);
    let mut line = String::new();
    // one line, at most 4 KiB (a password is not longer)
    if (&mut rd).take(4096).read_line(&mut line).is_err() {
        return;
    }
    let Ok(mut req) = serde_json::from_str::<serde_json::Value>(&line) else {
        let _ = writeln!(&*s, "{}", serde_json::json!({ "ok": false, "message": "bad request" }));
        return;
    };
    // the request line and the parsed copy overwritten (the password in them), not only freed
    // Safety: zero bytes are valid UTF-8
    unsafe { line.as_bytes_mut().fill(0) };
    line.clear();
    let user = req["user"].as_str().unwrap_or("").to_string();
    let password = req["password"].as_str().unwrap_or("").to_string();
    if let serde_json::Value::String(mut p) = req["password"].take() {
        unsafe { p.as_bytes_mut().fill(0) };
    }
    if user.is_empty() || user.contains(['/', '\0', ':']) {
        let _ = writeln!(&*s, "{}", serde_json::json!({ "ok": false, "message": "No user to sign in" }));
        return;
    }
    let r = match &o.mock {
        Some((u, p)) => {
            let ok = user == *u && password == *p;
            let mut password = password;
            unsafe { password.as_bytes_mut().fill(0) };
            ok.then_some(None).ok_or_else(|| "Authentication failure".to_string())
        }
        None => pam_login(&o.service, &user, password).map(Some),
    };
    let pam = match r {
        Ok(p) => p,
        Err(m) => {
            eprintln!("loom-auth: {user}: {m}");
            let _ = writeln!(&*s, "{}", serde_json::json!({ "ok": false, "message": m }));
            return;
        }
    };
    // the name PAM settled on (a module may spell it differently)
    let user = pam.as_ref().and_then(|p| p.user()).unwrap_or(user);
    eprintln!("loom-auth: {user} signed in");
    if writeln!(&*s, "{}", serde_json::json!({ "ok": true })).is_err() {
        return;
    }
    // the greeter's "start" (after its pictures); a closed connection or anything else: no session
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    if (&mut rd).take(4096).read_line(&mut line).is_err() || !serde_json::from_str::<serde_json::Value>(&line).is_ok_and(|v| v["start"] == true) {
        eprintln!("loom-auth: {user}: no start asked, no session");
        return;
    }
    let pam = if unsafe { libc::getuid() } == 0 { pam } else { None };
    // the desktop picked on the login screen; loom (and no choice) keeps loom's own start with the curtain
    let cmd = serde_json::from_str::<serde_json::Value>(&line).ok().and_then(|v| v["session"].as_str().map(String::from)).filter(|f| !f.starts_with("loom")).and_then(|f| session_exec(&f));
    let o2;
    let o = match cmd {
        Some(c) => {
            o2 = Opts { session: c, socket: o.socket.clone(), service: o.service.clone(), allow_uid: o.allow_uid, mock: o.mock.clone(), vt: o.vt, greeter_unit: o.greeter_unit.clone() };
            &o2
        }
        None => o,
    };
    match start_session(o, &user, pam) {
        Ok(pid) => {
            let _ = writeln!(&*s, "{}", serde_json::json!({ "started": true, "pid": pid }));
        }
        Err(m) => {
            eprintln!("loom-auth: session for {user}: {m}");
            let _ = writeln!(&*s, "{}", serde_json::json!({ "started": false, "message": m }));
        }
    }
}

fn main() {
    // Safety: getuid has no preconditions
    let me = unsafe { libc::getuid() };
    let mut o = Opts {
        socket: "/run/loom/auth.sock".into(),
        service: "loom".into(),
        allow_uid: me,
        mock: None,
        session: "loom-session --curtain /run/loom/handover".into(),
        vt: None,
        greeter_unit: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => o.socket = args.next().unwrap_or_default(),
            "--service" => o.service = args.next().unwrap_or_default(),
            "--allow-uid" => match args.next().and_then(|v| v.parse().ok()) {
                Some(u) => o.allow_uid = u,
                None => {
                    eprintln!("loom-auth: --allow-uid needs a number (--allow-uid-of takes a name)");
                    std::process::exit(2);
                }
            },
            // the greeter's user by name (the systemd unit: its uid is the system's choice)
            "--allow-uid-of" => match args.next().and_then(|n| user(&n)) {
                Some(u) => o.allow_uid = u.uid,
                None => {
                    eprintln!("loom-auth: --allow-uid-of: no such user");
                    std::process::exit(2);
                }
            },
            "--mock" => o.mock = args.next().and_then(|v| v.split_once(':').map(|(u, p)| (u.to_string(), p.to_string()))),
            "--session" => o.session = args.next().unwrap_or_default(),
            "--vt" => o.vt = args.next().and_then(|v| v.parse().ok()),
            "--greeter-unit" => o.greeter_unit = args.next(),
            _ => {
                eprintln!("usage: loom-auth [--socket PATH] [--service NAME] [--allow-uid UID | --allow-uid-of USER] [--mock USER:PASS] [--session CMD] [--vt N] [--greeter-unit UNIT]");
                std::process::exit(2);
            }
        }
    }
    // the test mode signs anyone in without PAM: never as root
    if o.mock.is_some() && me == 0 {
        eprintln!("loom-auth: --mock is for tests, not as root");
        std::process::exit(2);
    }
    // finished session workers reaped, not left as zombies
    // Safety: plain signal disposition
    unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
    let _ = std::fs::remove_file(&o.socket);
    let l = match UnixListener::bind(&o.socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("loom-auth: {}: {e}", o.socket);
            std::process::exit(1);
        }
    };
    // the greeter (another user) connects; who may ask is checked per connection (SO_PEERCRED)
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&o.socket, std::fs::Permissions::from_mode(0o666));
    }
    eprintln!("loom-auth: on {} (service {}, uid {} may ask{})", o.socket, o.service, o.allow_uid, if o.mock.is_some() { ", mock" } else { "" });
    // one caller at a time, in this one thread: the worker is forked from here and then runs PAM, which
    // is only safe with no other thread in the process (one could hold the allocator's lock). A
    // caller that says nothing is dropped after 3 s (only the greeter's own user may connect at all)
    for s in l.incoming().flatten() {
        if peer_uid(&s) != Some(o.allow_uid) {
            eprintln!("loom-auth: a connection from uid {:?} refused", peer_uid(&s));
            continue;
        }
        serve(&o, &s);
    }
}
