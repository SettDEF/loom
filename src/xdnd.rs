//! Drag and drop from Wayland programs into X ones (XDND): Xwayland does not turn a Wayland drag
//! into an X one, so over an X window loom is the drag's X source itself. It owns XdndSelection,
//! sends XdndEnter/Position/Drop to the window, and answers its requests for the data by reading
//! it from the Wayland program (a pipe per request).
//!
//! The other way, an X program's drag over a Wayland window: while a button held on an X window
//! moves over anything that is not X, loom shows a see-through XDND window over the whole X screen
//! (the proxy). The X program drags onto it; loom carries the drag on to the Wayland window under
//! the pointer (a Wayland drag it moves by hand) and fetches the data from the X program when the
//! Wayland one asks for it.

use std::{
    io::Read,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    rc::Rc,
};

use smithay::{
    reexports::{
        calloop::{generic::Generic, Interest, Mode, PostAction},
        wayland_server::protocol::{wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_surface::WlSurface},
        wayland_server::Resource,
    },
    utils::{Logical, Point},
    wayland::selection::data_device::with_source_metadata,
};
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{self, Atom, AtomEnum, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass},
        Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    CURRENT_TIME, NONE,
};

use crate::state::Loom;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        XdndAware, XdndProxy, XdndSelection, XdndTypeList, XdndEnter, XdndPosition, XdndStatus,
        XdndLeave, XdndDrop, XdndFinished, XdndActionCopy, TARGETS, UTF8_STRING, TEXT, _LOOM_XDND_DATA,
    }
}

/// The drag over one X window.
struct Over {
    win: Window,
    /// the window's (0, 0) in root coordinates
    origin: (i32, i32),
    accepted: bool,
    /// a position sent, its status not back yet; the newest one waiting meanwhile
    waiting: bool,
    next: Option<(i16, i16)>,
}

pub struct Xdnd {
    conn: Rc<RustConnection>,
    atoms: Atoms,
    root: Window,
    /// loom's window: the drag's X source
    win: Window,
    source: Option<WlDataSource>,
    mimes: Vec<String>,
    over: Option<Over>,
    /// dropped, waiting for XdndFinished
    dropped: Option<Window>,
    /// the stand-in target over the X screen for an X drag (an X program dragging out), shown or not
    proxy: Window,
    proxy_up: bool,
    from_x: Option<FromX>,
    /// where each piece of data asked of the X program goes, in order
    fetching: std::collections::VecDeque<OwnedFd>,
}

/// An X program's drag going on over Wayland windows.
pub struct FromX {
    source: Window,
    grab: smithay::wayland::selection::data_device::ServerDnDGrab<Loom>,
    /// the type names given to the Wayland side, with the X atom each is asked as
    types: Vec<(String, Atom)>,
    accepted: bool,
    time: u32,
}

impl Xdnd {
    fn connect(display: u32) -> Result<Xdnd, Box<dyn std::error::Error>> {
        let (conn, screen) = RustConnection::connect(Some(&format!(":{display}")))?;
        let root = conn.setup().roots[screen].root;
        let atoms = Atoms::new(&conn)?.reply()?;
        let win = conn.generate_id()?;
        conn.create_window(0, win, root, -10, -10, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE))?;
        let proxy = conn.generate_id()?;
        conn.create_window(0, proxy, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new().override_redirect(1))?;
        conn.change_property32(PropMode::REPLACE, proxy, atoms.XdndAware, AtomEnum::ATOM, &[5])?;
        conn.flush()?;
        Ok(Xdnd { conn: Rc::new(conn), atoms, root, win, source: None, mimes: Vec::new(), over: None, dropped: None, proxy, proxy_up: false, from_x: None, fetching: Default::default() })
    }

    fn message(&self, to: Window, kind: Atom, data: [u32; 5]) {
        let ev = ClientMessageEvent::new(32, to, kind, data);
        let _ = self.conn.send_event(false, to, EventMask::NO_EVENT, ev);
        let _ = self.conn.flush();
    }

    /// The window that takes XDND for `w` (its XdndProxy, else itself) and its version; None when
    /// it does not speak XDND.
    fn aware(&self, w: Window) -> Option<(Window, u32)> {
        let prop = |w: Window, p: Atom, t: AtomEnum| self.conn.get_property(false, w, p, t, 0, 1).ok()?.reply().ok()?.value32()?.next();
        let target = prop(w, self.atoms.XdndProxy, AtomEnum::WINDOW).unwrap_or(w);
        let version = prop(target, self.atoms.XdndAware, AtomEnum::ATOM)?;
        Some((target, version.min(5)))
    }

    fn atom(&self, name: &str) -> Option<Atom> {
        Some(self.conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
    }

    /// The atoms the source's types are offered as (text also as X's own text names).
    fn type_atoms(&self) -> Vec<Atom> {
        let mut v: Vec<Atom> = self.mimes.iter().filter_map(|m| self.atom(m)).collect();
        if self.mimes.iter().any(|m| m.starts_with("text/plain")) {
            v.extend([self.atoms.UTF8_STRING, AtomEnum::STRING.into(), self.atoms.TEXT]);
        }
        v
    }

    /// The source's type for an X target atom.
    fn mime_for(&self, target: Atom) -> Option<String> {
        if [self.atoms.UTF8_STRING, AtomEnum::STRING.into(), self.atoms.TEXT].contains(&target) {
            return self.mimes.iter().find(|m| m.as_str() == "text/plain;charset=utf-8").or_else(|| self.mimes.iter().find(|m| m.starts_with("text/plain"))).cloned();
        }
        let name = self.conn.get_atom_name(target).ok()?.reply().ok()?.name;
        let name = String::from_utf8(name).ok()?;
        self.mimes.contains(&name).then_some(name)
    }

    fn enter(&mut self, win: Window, version: u32, source: &WlDataSource) {
        self.source = Some(source.clone());
        self.mimes = with_source_metadata(source, |m| m.mime_types.clone()).unwrap_or_default();
        let _ = self.conn.set_selection_owner(self.win, self.atoms.XdndSelection, CURRENT_TIME);
        let types = self.type_atoms();
        if types.len() > 3 {
            let _ = self.conn.change_property32(PropMode::REPLACE, self.win, self.atoms.XdndTypeList, AtomEnum::ATOM, &types);
        }
        let t = |i: usize| types.get(i).copied().unwrap_or(NONE);
        self.message(win, self.atoms.XdndEnter, [self.win, (version << 24) | u32::from(types.len() > 3), t(0), t(1), t(2)]);
        let origin = self.conn.translate_coordinates(win, self.root, 0, 0).ok().and_then(|c| c.reply().ok()).map(|r| (r.dst_x as i32, r.dst_y as i32)).unwrap_or((0, 0));
        self.over = Some(Over { win, origin, accepted: false, waiting: false, next: None });
    }

    fn position(&mut self, x: i16, y: i16) {
        let Some(o) = self.over.as_mut() else { return };
        if o.waiting {
            o.next = Some((x, y));
            return;
        }
        o.waiting = true;
        let (win, packed) = (o.win, ((x as u16 as u32) << 16) | y as u16 as u32);
        self.message(win, self.atoms.XdndPosition, [self.win, 0, packed, CURRENT_TIME, self.atoms.XdndActionCopy]);
    }

    fn leave(&mut self) {
        if let Some(o) = self.over.take() {
            self.message(o.win, self.atoms.XdndLeave, [self.win, 0, 0, 0, 0]);
        }
    }

    /// The X server's events: statuses, finishes and requests for the data.
    fn events(&mut self, loom: &mut Loom) {
        while let Ok(Some(ev)) = self.conn.poll_for_event() {
            match ev {
                Event::ClientMessage(m) if m.type_ == self.atoms.XdndStatus => {
                    let d = m.data.as_data32();
                    let Some(o) = self.over.as_mut().filter(|o| o.win == d[0]) else { continue };
                    o.accepted = d[1] & 1 == 1;
                    o.waiting = false;
                    if let Some(src) = &self.source {
                        if src.version() >= 3 {
                            src.action(if o.accepted { DndAction::Copy } else { DndAction::empty() });
                        }
                        if o.accepted {
                            src.target(self.mimes.first().cloned());
                        } else {
                            src.target(None);
                        }
                    }
                    if let Some((x, y)) = o.next.take() {
                        self.position(x, y);
                    }
                }
                Event::ClientMessage(m) if m.type_ == self.atoms.XdndFinished => {
                    if self.dropped.take().is_some() {
                        if let Some(src) = self.source.take() {
                            if src.version() >= 3 {
                                src.dnd_finished();
                            }
                        }
                    }
                }
                Event::SelectionRequest(r) if r.selection == self.atoms.XdndSelection => self.request(r, loom),
                Event::ClientMessage(m) if m.window == self.proxy => self.from_x_message(m, loom),
                Event::SelectionNotify(n) if n.requestor == self.proxy => self.fetched(n.property),
                _ => {}
            }
        }
        let _ = self.conn.flush();
    }

    /// An X window asks for the data as `r.target`: read from the Wayland program, then given.
    fn request(&mut self, r: SelectionRequestEvent, loom: &mut Loom) {
        let notify = move |conn: &RustConnection, property: Atom| {
            let ev = SelectionNotifyEvent { response_type: xproto::SELECTION_NOTIFY_EVENT, sequence: 0, time: r.time, requestor: r.requestor, selection: r.selection, target: r.target, property };
            let _ = conn.send_event(false, r.requestor, EventMask::NO_EVENT, ev);
            let _ = conn.flush();
        };
        let property = if r.property == NONE { r.target } else { r.property };
        if r.target == self.atoms.TARGETS {
            let mut types = self.type_atoms();
            types.push(self.atoms.TARGETS);
            let _ = self.conn.change_property32(PropMode::REPLACE, r.requestor, property, AtomEnum::ATOM, &types);
            notify(&self.conn, property);
            return;
        }
        let (Some(mime), Some(src)) = (self.mime_for(r.target), self.source.clone()) else {
            notify(&self.conn, NONE);
            return;
        };
        let Ok((read, write)) = std::io::pipe() else {
            notify(&self.conn, NONE);
            return;
        };
        src.send(mime, write.as_fd());
        drop(write);
        // the program writes once it sees the request: read as it comes, given at the end
        let conn = self.conn.clone();
        let read = std::fs::File::from(OwnedFd::from(read));
        let mut got = Vec::new();
        let Some(h) = loom.loop_handle.clone() else { return };
        let _ = h.insert_source(Generic::new(read, Interest::READ, Mode::Level), move |_, fd, _| {
            let mut buf = [0u8; 65536];
            let mut f: &std::fs::File = fd;
            match f.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = conn.change_property8(PropMode::REPLACE, r.requestor, property, r.target, &got);
                    notify(&conn, property);
                    Ok(PostAction::Remove)
                }
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    Ok(PostAction::Continue)
                }
            }
        });
    }
}

impl Loom {
    /// The XDND side, connected to Xwayland the first time a drag reaches an X window.
    fn xdnd(&mut self) -> Option<&mut Xdnd> {
        if self.xdnd.is_none() {
            let display = self.x_display?;
            let x = match Xdnd::connect(display) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("loom: X drag and drop: {e}");
                    return None;
                }
            };
            // its events read as they come
            let fd = x.conn.stream().as_raw_fd();
            // SAFETY: the connection outlives the source (both live as long as loom)
            let owned = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned().ok()?;
            let h = self.loop_handle.clone()?;
            let _ = h.insert_source(Generic::new(owned, Interest::READ, Mode::Level), |_, _, s: &mut Loom| {
                if let Some(mut x) = s.xdnd.take() {
                    x.events(s);
                    s.xdnd = Some(x);
                }
                Ok(PostAction::Continue)
            });
            self.xdnd = Some(x);
        }
        self.xdnd.as_mut()
    }

    /// The X window a surface belongs to (an X program's toplevel or popup).
    fn x_window_of(&self, s: &WlSurface) -> Option<Window> {
        let mine = |w: &smithay::desktop::Window| w.x11_surface().filter(|x| x.wl_surface().as_ref() == Some(s)).map(|x| x.window_id());
        self.space.elements().find_map(mine).or_else(|| self.managed.iter().find_map(|m| mine(&m.window)))
    }

    /// X coordinates per logical pixel (X programs drawn sharp are in screen pixels).
    fn x_per_logical(&self) -> f64 {
        if crate::config::xwayland_zero_scaling() { self.output.as_ref().map(|o| o.current_scale().fractional_scale()).unwrap_or(1.0) } else { 1.0 }
    }

    pub fn xdnd_target(&mut self, s: &WlSurface) -> bool {
        let Some(w) = self.x_window_of(s) else { return false };
        self.xdnd().and_then(|x| x.aware(w)).is_some()
    }

    pub fn xdnd_motion(&mut self, s: &WlSurface, at: Point<f64, Logical>, source: &WlDataSource) {
        let k = self.x_per_logical();
        let Some(w) = self.x_window_of(s) else { return };
        let Some(x) = self.xdnd() else { return };
        if x.over.as_ref().map(|o| o.win) != x.aware(w).map(|a| a.0) {
            x.leave();
            let Some((target, version)) = x.aware(w) else { return };
            x.enter(target, version, source);
        }
        let Some(o) = x.over.as_ref() else { return };
        let (rx, ry) = (o.origin.0 + (at.x * k).round() as i32, o.origin.1 + (at.y * k).round() as i32);
        x.position(rx as i16, ry as i16);
        let _ = x.conn.flush();
    }

    pub fn xdnd_leave(&mut self) {
        if let Some(x) = self.xdnd.as_mut() {
            x.leave();
            let _ = x.conn.flush();
        }
    }

    /// Dropped: true when the X window said it takes it.
    pub fn xdnd_drop(&mut self) -> bool {
        let Some(x) = self.xdnd.as_mut() else { return false };
        let Some(o) = x.over.take() else { return false };
        if !o.accepted {
            x.message(o.win, x.atoms.XdndLeave, [x.win, 0, 0, 0, 0]);
            return false;
        }
        x.message(o.win, x.atoms.XdndDrop, [x.win, 0, CURRENT_TIME, 0, 0]);
        x.dropped = Some(o.win);
        true
    }
}

// ── an X program's drag, carried on to Wayland windows ──

impl Xdnd {
    pub fn is_proxy(&self, w: Window) -> bool {
        w == self.proxy
    }

    /// The stand-in shown over the whole X screen (on top), or taken away.
    fn proxy_show(&mut self, up: bool) {
        if up == self.proxy_up {
            return;
        }
        self.proxy_up = up;
        use xproto::{ConfigureWindowAux, StackMode};
        if up {
            let g = self.conn.get_geometry(self.root).ok().and_then(|c| c.reply().ok());
            let (w, h) = g.map(|g| (g.width as u32, g.height as u32)).unwrap_or((8192, 8192));
            let _ = self.conn.configure_window(self.proxy, &ConfigureWindowAux::new().x(0).y(0).width(w).height(h).stack_mode(StackMode::ABOVE));
            let _ = self.conn.map_window(self.proxy);
        } else {
            let _ = self.conn.unmap_window(self.proxy);
        }
        let _ = self.conn.flush();
    }

    /// An X program owns the drag selection: it may be dragging.
    fn x_dragging(&self) -> bool {
        let owner = self.conn.get_selection_owner(self.atoms.XdndSelection).ok().and_then(|c| c.reply().ok()).map(|r| r.owner).unwrap_or(NONE);
        owner != NONE && owner != self.win
    }

    fn name_of(&self, a: Atom) -> Option<String> {
        let n = self.conn.get_atom_name(a).ok()?.reply().ok()?.name;
        String::from_utf8(n).ok()
    }

    /// The X program's messages to the stand-in: Enter, Position, Leave, Drop.
    fn from_x_message(&mut self, m: ClientMessageEvent, loom: &mut Loom) {
        let d = m.data.as_data32();
        let a = &self.atoms;
        if m.type_ == a.XdndEnter {
            let source = d[0];
            let atoms: Vec<Atom> = if d[1] & 1 == 1 {
                self.conn.get_property(false, source, a.XdndTypeList, AtomEnum::ATOM, 0, 64).ok().and_then(|c| c.reply().ok()).and_then(|r| r.value32().map(|v| v.collect())).unwrap_or_default()
            } else {
                d[2..].iter().copied().filter(|t| *t != NONE).collect()
            };
            // the X types as Wayland ones: mime names as they are, X text as utf-8 text
            let mut types: Vec<(String, Atom)> = atoms.iter().filter_map(|t| Some((self.name_of(*t)?, *t))).filter(|(n, _)| n.contains('/')).collect();
            if atoms.contains(&self.atoms.UTF8_STRING) && !types.iter().any(|(n, _)| n == "text/plain;charset=utf-8") {
                types.push(("text/plain;charset=utf-8".into(), self.atoms.UTF8_STRING));
            }
            let meta = smithay::wayland::selection::data_device::SourceMetadata { mime_types: types.iter().map(|t| t.0.clone()).collect(), dnd_action: DndAction::Copy };
            let grab = smithay::wayland::selection::data_device::ServerDnDGrab::detached(&loom.dh, meta, loom.seat.clone());
            self.from_x = Some(FromX { source, grab, types, accepted: false, time: CURRENT_TIME });
        } else if m.type_ == a.XdndPosition {
            let k = loom.x_per_logical();
            let at: Point<f64, Logical> = (((d[2] >> 16) as i16) as f64 / k, ((d[2] & 0xffff) as i16) as f64 / k).into();
            // the Wayland window there (never an X one: over those the proxy is away)
            let under = loom.surface_at(at).filter(|(s, _)| loom.x_window_of(s).is_none());
            let Some(f) = self.from_x.as_mut() else { return };
            f.grab.hover(under, at, smithay::utils::SERIAL_COUNTER.next_serial(), d[3]);
            let (source, accepted) = (f.source, f.accepted);
            self.message(source, self.atoms.XdndStatus, [self.proxy, u32::from(accepted), 0, 0, if accepted { self.atoms.XdndActionCopy } else { NONE }]);
        } else if m.type_ == a.XdndLeave {
            if let Some(mut f) = self.from_x.take() {
                f.grab.hover(None::<(WlSurface, Point<f64, Logical>)>, Point::default(), smithay::utils::SERIAL_COUNTER.next_serial(), 0);
            }
        } else if m.type_ == a.XdndDrop {
            let Some(f) = self.from_x.as_mut() else { return };
            f.time = d[2];
            if !f.grab.finish(loom) {
                let source = f.source;
                self.from_x = None;
                self.message(source, self.atoms.XdndFinished, [self.proxy, 0, 0, 0, 0]);
            }
        }
    }

    /// The Wayland program asks for the data as `mime`: asked of the X program, written to `fd`.
    fn fetch(&mut self, mime: &str, fd: OwnedFd) {
        let Some(f) = self.from_x.as_ref() else { return };
        let Some(&(_, atom)) = f.types.iter().find(|t| t.0 == mime) else { return };
        let _ = self.conn.convert_selection(self.proxy, self.atoms.XdndSelection, atom, self.atoms._LOOM_XDND_DATA, f.time);
        let _ = self.conn.flush();
        self.fetching.push_back(fd);
    }

    /// The X program gave it (or not, NONE): the next waiting fd gets it.
    fn fetched(&mut self, property: Atom) {
        let Some(fd) = self.fetching.pop_front() else { return };
        if property == NONE {
            return;
        }
        let data = self.conn.get_property(true, self.proxy, property, AtomEnum::ANY, 0, 1 << 24).ok().and_then(|c| c.reply().ok()).map(|r| r.value).unwrap_or_default();
        // written off the loop (the reader may be slow)
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::File::from(fd).write_all(&data);
        });
    }

    /// The Wayland program is done with the drop: the X program hears it went in.
    fn from_x_finished(&mut self, ok: bool) {
        if let Some(f) = self.from_x.take() {
            self.message(f.source, self.atoms.XdndFinished, [self.proxy, u32::from(ok), if ok { self.atoms.XdndActionCopy } else { NONE }, 0, 0]);
        }
    }
}

impl Loom {
    /// The pointer moved (or a button went): a button held on an X window and over something that
    /// is not X, with an X program owning the drag: the stand-in comes up; else it goes.
    pub fn xdnd_pointer(&mut self, at: Point<f64, Logical>) {
        let Some(pointer) = self.seat.get_pointer() else { return };
        let held_on_x = pointer.is_grabbed() && pointer.grab_start_data().and_then(|g| g.focus).is_some_and(|(s, _)| self.x_window_of(&s).is_some());
        if !held_on_x {
            if let Some(x) = self.xdnd.as_mut().filter(|x| x.proxy_up) {
                x.proxy_show(false);
            }
            return;
        }
        let over_x = self.surface_at(at).is_some_and(|(s, _)| self.x_window_of(&s).is_some());
        let Some(x) = self.xdnd() else { return };
        let up = !over_x && (x.proxy_up || x.x_dragging());
        x.proxy_show(up);
    }

    /// Whether an X window is loom's own stand-in (not one to show).
    pub fn xdnd_is_proxy(&self, w: Window) -> bool {
        self.xdnd.as_ref().is_some_and(|x| x.is_proxy(w))
    }

    pub fn xdnd_accept(&mut self, mime: Option<String>) {
        if let Some(f) = self.xdnd.as_mut().and_then(|x| x.from_x.as_mut()) {
            f.accepted = mime.is_some();
        }
    }

    pub fn xdnd_send(&mut self, mime: &str, fd: OwnedFd) {
        if let Some(x) = self.xdnd.as_mut() {
            x.fetch(mime, fd);
        }
    }

    pub fn xdnd_finished(&mut self, ok: bool) {
        if let Some(x) = self.xdnd.as_mut() {
            x.from_x_finished(ok);
        }
    }
}
