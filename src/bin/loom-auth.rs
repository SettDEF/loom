//! loom-auth: the login screen's password check (docs/greeter.md), the one place PAM runs. The
//! greeter's page connects to its socket and sends one line, {"user": …, "password": …}; the answer
//! is one line, {"ok": true} or {"ok": false, "message": …}. Runs as root on the real seat; only the
//! greeter's uid may ask (SO_PEERCRED). `--mock USER:PASS` answers without PAM (headless tests).
//!
//! Usage: loom-auth [--socket PATH] [--service NAME] [--allow-uid UID] [--mock USER:PASS]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_CONV_ERR: c_int = 19;

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
}

/// What the conversation needs: the password to give, the messages PAM said (shown to the user).
struct Talk {
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
                PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => (*replies.add(i)).resp = libc::strdup(talk.password.as_ptr()),
                PAM_ERROR_MSG | PAM_TEXT_INFO if !m.msg.is_null() => talk.said.push(CStr::from_ptr(m.msg).to_string_lossy().into_owned()),
                _ => {}
            }
        }
        *out = replies;
    }
    PAM_SUCCESS
}

/// The password checked by PAM's `service` for `user` (and the account allowed now: not expired).
fn pam_check(service: &str, user: &str, password: String) -> Result<(), String> {
    let (Ok(svc), Ok(usr)) = (CString::new(service), CString::new(user)) else { return Err("bad name".into()) };
    let mut bytes = password.into_bytes();
    let pw = CString::new(bytes.clone()).map_err(|_| "bad password".to_string());
    bytes.fill(0);
    let mut talk = Talk { password: pw?, said: Vec::new() };
    let conv = PamConv { conv: converse, appdata: &mut talk as *mut Talk as *mut c_void };
    let mut h: *mut c_void = std::ptr::null_mut();
    // Safety: plain PAM calls on a handle we started and end here; talk outlives them
    let r = unsafe {
        let mut r = pam_start(svc.as_ptr(), usr.as_ptr(), &conv, &mut h);
        if r == PAM_SUCCESS {
            r = pam_authenticate(h, 0);
        }
        if r == PAM_SUCCESS {
            r = pam_acct_mgmt(h, 0);
        }
        let why = if r == PAM_SUCCESS { String::new() } else { CStr::from_ptr(pam_strerror(h, r)).to_string_lossy().into_owned() };
        if !h.is_null() {
            pam_end(h, r);
        }
        (r, why)
    };
    // the password's bytes cleared before the memory goes back
    let mut left = std::mem::take(&mut talk.password).into_bytes_with_nul();
    left.fill(0);
    if r.0 == PAM_SUCCESS {
        Ok(())
    } else {
        Err(talk.said.pop().unwrap_or(r.1))
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
}

fn answer(o: &Opts, line: &str) -> serde_json::Value {
    let Ok(mut req) = serde_json::from_str::<serde_json::Value>(line) else { return serde_json::json!({ "ok": false, "message": "bad request" }) };
    let user = req["user"].as_str().unwrap_or("").to_string();
    let password = req["password"].as_str().unwrap_or("").to_string();
    req["password"] = serde_json::Value::Null;
    let r = match &o.mock {
        Some((u, p)) => (user == *u && password == *p).then_some(()).ok_or_else(|| "Authentication failure".to_string()),
        None => pam_check(&o.service, &user, password),
    };
    match r {
        Ok(()) => {
            eprintln!("loom-auth: {user} signed in");
            serde_json::json!({ "ok": true })
        }
        Err(m) => {
            eprintln!("loom-auth: {user}: {m}");
            serde_json::json!({ "ok": false, "message": m })
        }
    }
}

fn main() {
    // Safety: getuid has no preconditions
    let me = unsafe { libc::getuid() };
    let mut o = Opts { socket: "/run/loom/auth.sock".into(), service: "loom".into(), allow_uid: me, mock: None };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => o.socket = args.next().unwrap_or_default(),
            "--service" => o.service = args.next().unwrap_or_default(),
            "--allow-uid" => o.allow_uid = args.next().and_then(|v| v.parse().ok()).unwrap_or(me),
            "--mock" => o.mock = args.next().and_then(|v| v.split_once(':').map(|(u, p)| (u.to_string(), p.to_string()))),
            _ => {
                eprintln!("usage: loom-auth [--socket PATH] [--service NAME] [--allow-uid UID] [--mock USER:PASS]");
                std::process::exit(2);
            }
        }
    }
    let _ = std::fs::remove_file(&o.socket);
    let l = match UnixListener::bind(&o.socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("loom-auth: {}: {e}", o.socket);
            std::process::exit(1);
        }
    };
    eprintln!("loom-auth: on {} (service {}, uid {} may ask{})", o.socket, o.service, o.allow_uid, if o.mock.is_some() { ", mock" } else { "" });
    // one request at a time: PAM's own delay after a failure slows guessing
    for s in l.incoming().flatten() {
        if peer_uid(&s) != Some(o.allow_uid) {
            eprintln!("loom-auth: a connection from uid {:?} refused", peer_uid(&s));
            continue;
        }
        let mut line = String::new();
        // one line, at most 4 KiB (a password is not longer)
        if BufReader::new((&s).take(4096)).read_line(&mut line).is_err() {
            continue;
        }
        let a = answer(&o, &line);
        line.clear();
        let _ = writeln!(&s, "{a}");
    }
}
