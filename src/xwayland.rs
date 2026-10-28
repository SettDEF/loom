//! X programs through Xwayland (the first of loom's two X servers, PLAN.md): Xwayland is started
//! with loom, loom is its window manager. Normal X windows are managed like Wayland ones (tiled,
//! on workspaces, in `hyprctl clients` with xwayland: true); dialogs and other transient windows
//! float centred; menus and tooltips (override-redirect) stand where the program puts them.

use std::process::Stdio;

use smithay::{
    desktop::Window,
    reexports::calloop::{
        generic::{FdWrapper, Generic},
        Interest, LoopHandle, Mode, PostAction, RegistrationToken,
    },
    utils::{Logical, Rectangle},
    wayland::selection::SelectionTarget,
    wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    xwayland::{
        xwm::{Reorder, ResizeEdge, WmWindowType, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
    },
};

use crate::state::Loom;


/// Xwayland's state: a display reserved and watched until the first X program connects, then
/// Xwayland running (it ends with its last client, and the display is watched again): no X
/// program open = no Xwayland (54 MB saved, the battery and memory budget).
#[derive(Default)]
pub struct XState {
    pub reserved: Option<smithay::xwayland::XWaylandReserved>,
    pub watches: Vec<RegistrationToken>,
    pub server: Option<RegistrationToken>,
    pub starting: bool,
    /// Xwayland as a Wayland client (disconnecting it ends it); the pending idle check
    pub client: Option<smithay::reexports::wayland_server::Client>,
    pub idle_check: Option<RegistrationToken>,
    /// xwayland:force_zero_scaling, read at the last layout
    pub zero_scaling: bool,
    /// where X sees the screens (xscreens.rs); None without force_zero_scaling
    pub layout: Option<std::sync::Arc<crate::xscreens::XLayout>>,
}

/// Reserves a free X display (`display` to keep the same one) and watches it; returns its number.
pub fn start(handle: &LoopHandle<'static, Loom>, state: &mut Loom) -> Result<u32, Box<dyn std::error::Error>> {
    let reserved = XWayland::reserve(state.x_display, true)?;
    let n = reserved.display_number();
    for fd in reserved.fds() {
        // Safety: the fd belongs to `reserved`, kept until these watches are removed
        let w = unsafe { FdWrapper::new(fd) };
        let token = handle.insert_source(Generic::new(w, Interest::READ, Mode::Level), |_, _, state: &mut Loom| {
            if !state.x.starting {
                state.x.starting = true;
                if let Some(h) = state.loop_handle.clone() {
                    h.insert_idle(|state| state.x_spawn());
                }
            }
            Ok(PostAction::Continue)
        })?;
        state.x.watches.push(token);
    }
    state.x.reserved = Some(reserved);
    Ok(n)
}

impl Loom {
    /// Xwayland now, not when the first X program knocks (the real session: settings first).
    pub fn x_start_now(&mut self) {
        if !self.x.starting && self.x.client.is_none() && self.x.reserved.is_some() {
            self.x.starting = true;
            self.x_spawn();
        }
    }

    /// The first X client knocked: Xwayland starts on the reserved display and takes it.
    fn x_spawn(&mut self) {
        let Some(h) = self.loop_handle.clone() else { return };
        for t in self.x.watches.drain(..) {
            h.remove(t);
        }
        let Some(reserved) = self.x.reserved.take() else { return };
        let spawned = XWayland::spawn_reserved(&self.dh, reserved, std::iter::empty::<(String, String)>(), Stdio::null(), Stdio::null(), |_| ());
        let (xwayland, client) = match spawned {
            Ok(x) => x,
            Err(e) => {
                eprintln!("loom: Xwayland did not start: {e}");
                return;
            }
        };
        self.x.client = Some(client.clone());
        self.x_layout_apply();
        let h2 = h.clone();
        let token = h.insert_source(xwayland, move |event, _, state: &mut Loom| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => match X11Wm::start_wm(h2.clone(), x11_socket, client.clone()) {
                Ok(wm) => {
                    state.xwm = Some(wm);
                    state.x_layout_apply();
                    state.x.starting = false;
                    eprintln!("loom: Xwayland started for the first X program, DISPLAY=:{display_number}");
                    // the user's X settings (Xft.dpi: X text sized for the screen), as a login does; waited for (a few ms), so programs after this one see them
                    let xres = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".Xresources")).filter(|p| p.exists());
                    if let Some(p) = xres {
                        let _ = std::process::Command::new("xrdb").arg("-merge").arg(p).env("DISPLAY", format!(":{display_number}")).stdout(Stdio::null()).stderr(Stdio::null()).status();
                    }
                    // a program without a window (xclip, xrdb) may be all there is
                    state.x_idle_check();
                }
                Err(e) => eprintln!("loom: the X window manager did not start: {e}"),
            },
            XWaylandEvent::Error => eprintln!("loom: Xwayland did not start"),
        });
        self.x.server = token.ok();
    }

    /// Xwayland ended (its last X program closed): the display is watched again.
    fn x_ended(&mut self) {
        self.xwm = None;
        self.x.starting = false;
        self.x.client = None;
        let Some(h) = self.loop_handle.clone() else { return };
        if let Some(t) = self.x.server.take() {
            h.remove(t);
        }
        // the old instance lets go of the display's lock as it is dropped: watch it again then
        h.insert_idle(|state| state.x_watch_again(0));
    }

    /// The display watched again; while the old Xwayland still holds it, tried again shortly.
    fn x_watch_again(&mut self, tries: u32) {
        let Some(h) = self.loop_handle.clone() else { return };
        match start(&h, self) {
            Ok(_) => eprintln!("loom: Xwayland ended; DISPLAY=:{} waits for the next X program", self.x_display.unwrap_or(0)),
            Err(e) if tries >= 20 => eprintln!("loom: X display not watched again: {e}"),
            Err(_) => {
                let _ = h.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_millis(100)), move |_, _, s: &mut Loom| {
                    s.x_watch_again(tries + 1);
                    smithay::reexports::calloop::timer::TimeoutAction::Drop
                });
            }
        }
    }
}
/// Floats rather than tiles: dialogs, utilities, splash screens, anything transient for a window.
/// An X window that is a menu, a dropdown or a tooltip in all but name: it names its parent and
/// asks for no frame (or says it is one). No title bar from loom, and placed where it asks.
pub fn x_menu_like(x: &X11Surface) -> bool {
    matches!(x.window_type(), Some(WmWindowType::Menu | WmWindowType::PopupMenu | WmWindowType::DropdownMenu | WmWindowType::Tooltip | WmWindowType::Notification))
        || (x.is_transient_for().is_some() && x.is_decorated())
}

/// One size only (min = max): a plugin's own window, a fixed dialog. Stretched to a tile it would
/// draw in a corner of it.
fn fixed_size(x: &X11Surface) -> bool {
    matches!((x.min_size(), x.max_size()), (Some(a), Some(b)) if a == b && a.w > 0 && a.h > 0)
}

fn floats(x: &X11Surface) -> bool {
    fixed_size(x) || x.is_transient_for().is_some() || matches!(x.window_type(), Some(WmWindowType::Dialog | WmWindowType::Utility | WmWindowType::Splash | WmWindowType::Toolbar))
}

impl Loom {
    fn x_window(&self, x: &X11Surface) -> Option<Window> {
        self.space.elements().find(|w| w.x11_surface() == Some(x)).cloned().or_else(|| self.managed.iter().find(|m| m.window.x11_surface() == Some(x)).map(|m| m.window.clone()))
    }

    /// An X window gone (unmapped or destroyed): off the screen, out of the managed ones.
    fn x_gone(&mut self, x: &X11Surface) {
        let Some(w) = self.x_window(x) else { return };
        if self.managed_of(&w).is_some() {
            self.unmanage(&w);
        } else {
            self.space.unmap_elem(&w);
            self.dirty = true;
            // (a menu can no longer be the focus; one that was, from an older loom, hands it on)
            if self.focused.as_ref() == Some(&w) {
                self.focus_next(self.active_ws);
            }
        }
        // a restart waiting for the X programs: the last one gone, it goes ahead
        if self.restart_pending && !self.managed.iter().any(|m| m.window.x11_surface().is_some()) {
            self.restart = true;
            self.signal.stop();
        }
        self.x_idle_check();
    }
}

impl XwmHandler for Loom {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("the X window manager")
    }
    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, x: X11Surface) {
        let float = floats(&x);
        let w = Window::new_x11_window(x.clone());
        if float {
            // a menu-like one (it names its parent and draws its own frame: Bitwig's menus) where it
            // put itself; others centred on the screen, at the size they asked for. Centred, Bitwig's
            // dropdowns opened far from their box
            let size = x.geometry().size;
            let screen = self.output.as_ref().and_then(|o| self.space.output_geometry(o)).unwrap_or_default();
            let own = x.geometry().loc;
            let loc = if x_menu_like(&x) && (own.x != 0 || own.y != 0) { (own.x, own.y) } else { (screen.loc.x + (screen.size.w - size.w) / 2, screen.loc.y + (screen.size.h - size.h) / 2) };
            let _ = x.configure(Rectangle::new(loc.into(), size));
            self.manage_floating(w, loc.into());
        } else {
            self.manage(w);
        }
        // shown once in place: a program that scales per screen (Bitwig) draws its first frame
        // at its tile, not at the size and screen it asked for and then half-repainted
        let _ = x.set_mapped(true);
    }
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, x: X11Surface) {
        // loom's own drag stand-in is no window to show
        if self.xdnd_is_proxy(x.window_id()) {
            return;
        }
        let loc = x.geometry().loc;
        self.space.map_element(Window::new_x11_window(x), loc, true);
        self.dirty = true;
    }
    fn unmapped_window(&mut self, _xwm: XwmId, x: X11Surface) {
        self.x_gone(&x);
        if !x.is_override_redirect() {
            let _ = x.set_mapped(false);
        }
    }
    fn destroyed_window(&mut self, _xwm: XwmId, x: X11Surface) {
        self.x_gone(&x);
    }

    fn configure_request(&mut self, _xwm: XwmId, x: X11Surface, nx: Option<i32>, ny: Option<i32>, nw: Option<u32>, nh: Option<u32>, _reorder: Option<Reorder>) {
        let tiled = self.x_window(&x).and_then(|w| self.managed_of(&w).map(|m| !m.floating)).unwrap_or(false);
        if tiled {
            // the tiling decides its size: it is told again where it is
            let _ = x.configure(None);
            return;
        }
        let mut g = x.geometry();
        if let Some(v) = nx {
            g.loc.x = v;
        }
        if let Some(v) = ny {
            g.loc.y = v;
        }
        if let Some(v) = nw {
            g.size.w = v as i32;
        }
        if let Some(v) = nh {
            g.size.h = v as i32;
        }
        let _ = x.configure(g);
        if let Some(w) = self.x_window(&x) {
            self.space.map_element(w, g.loc, false);
            self.dirty = true;
        }
    }
    fn configure_notify(&mut self, _xwm: XwmId, x: X11Surface, geometry: Rectangle<i32, Logical>, _above: Option<u32>) {
        // a menu or tooltip moved itself
        if x.is_override_redirect() {
            if let Some(w) = self.x_window(&x) {
                self.space.map_element(w, geometry.loc, false);
                self.dirty = true;
            }
        }
    }
    fn property_notify(&mut self, _xwm: XwmId, x: X11Surface, property: smithay::xwayland::xwm::WmWindowProperty) {
        use smithay::xwayland::xwm::WmWindowProperty as P;
        match property {
            P::Title => {
                if let Some(w) = self.x_window(&x) {
                    self.title_changed_of(&w);
                }
            }
            // a size fixed after mapping (nih-plug's standalone sets it once its editor is in):
            // out of the layout, centred at that size
            P::NormalHints if fixed_size(&x) => {
                let Some(w) = self.x_window(&x) else { return };
                if !self.managed_of(&w).is_some_and(|m| !m.floating) {
                    return;
                }
                let size = x.max_size().unwrap_or_default();
                let screen = self.output.as_ref().and_then(|o| self.space.output_geometry(o)).unwrap_or_default();
                let loc = (screen.loc.x + (screen.size.w - size.w) / 2, screen.loc.y + (screen.size.h - size.h) / 2);
                let _ = x.configure(Rectangle::new(loc.into(), size));
                if let Some(m) = self.managed.iter_mut().find(|m| m.window == w) {
                    m.floating = true;
                }
                self.space.map_element(w, loc, true);
                self.relayout();
            }
            _ => {}
        }
    }
    // a game or X program going fullscreen (_NET_WM_STATE_FULLSCREEN): as the bind does
    fn fullscreen_request(&mut self, _xwm: XwmId, x: X11Surface) {
        match self.x_window(&x) {
            Some(w) => self.set_fullscreen(&w, true),
            None => {
                let _ = x.set_fullscreen(true);
            }
        }
    }
    fn unfullscreen_request(&mut self, _xwm: XwmId, x: X11Surface) {
        match self.x_window(&x) {
            Some(w) => self.set_fullscreen(&w, false),
            None => {
                let _ = x.set_fullscreen(false);
            }
        }
    }
    fn resize_request(&mut self, _xwm: XwmId, _x: X11Surface, _button: u32, _edge: ResizeEdge) {}
    fn disconnected(&mut self, _xwm: XwmId) {
        self.x_ended();
    }
    fn move_request(&mut self, _xwm: XwmId, _x: X11Surface, _button: u32) {}

    // the clipboard and selection, shared with Wayland (clipboard.rs)
    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        self.x_may_read_selection(xwm)
    }
    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        self.x_new_selection(selection, mime_types);
    }
    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        self.x_cleared_selection(selection);
    }
    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: std::os::fd::OwnedFd) {
        self.x_send_selection(selection, mime_type, fd);
    }
}

impl XWaylandShellHandler for Loom {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    /// an X window's surface: at its screen's scale before its first frame is read
    fn surface_associated(&mut self, _xwm: XwmId, wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface, surface: X11Surface) {
        smithay::wayland::compositor::set_surface_scale(&wl_surface, Some(self.x_scale_at(surface.geometry())));
    }
}
smithay::delegate_xwayland_shell!(Loom);

/// How long Xwayland waits with no X window before it is checked for programs (and between checks
/// while a windowless one, like xclip holding a copy, keeps it). Minutes, not seconds: the
/// clipboard bridge's xclip comes and goes with every copy (42 starts in one hour at 15 s).
const X_IDLE: std::time::Duration = std::time::Duration::from_secs(300);

impl Loom {
    fn x_windows(&self) -> bool {
        self.managed.iter().any(|m| m.window.x11_surface().is_some()) || self.space.elements().any(|w| w.x11_surface().is_some())
    }

    /// No X window left: in a while, if no X program is connected either, Xwayland ends (it
    /// starts again with the next one). One timer, none while X windows are open.
    pub fn x_idle_check(&mut self) {
        // on the real screen it stays (started at login with the X settings); only headless ends it
        if !self.headless {
            return;
        }
        if self.x.client.is_none() || self.x.idle_check.is_some() || self.x_windows() {
            return;
        }
        let Some(h) = self.loop_handle.clone() else { return };
        if std::env::var_os("LOOM_X_DEBUG").is_some() {
            eprintln!("loom: x idle check in {X_IDLE:?}");
        }
        let t = h.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(X_IDLE), |_, _, state: &mut Loom| {
            state.x.idle_check = None;
            if state.x.client.is_none() || state.x_windows() {
                return smithay::reexports::calloop::timer::TimeoutAction::Drop;
            }
            let display = format!(":{}", state.x_display.unwrap_or(0));
            // asked off the loop: a stuck X server must not stall the screen
            let (tx, rx) = smithay::reexports::calloop::channel::channel::<Option<usize>>();
            std::thread::spawn(move || {
                let _ = tx.send(x_programs(&display));
            });
            if let Some(h) = state.loop_handle.clone() {
                let token = std::rc::Rc::new(std::cell::Cell::new(None));
                let t2 = token.clone();
                token.set(h.insert_source(rx, move |e, _, state: &mut Loom| {
                    if let smithay::reexports::calloop::channel::Event::Msg(n) = e {
                        state.x_idle_answer(n);
                    }
                    // asked once: the channel goes
                    if let (Some(t), Some(h)) = (t2.take(), state.loop_handle.clone()) {
                        h.insert_idle(move |s| if let Some(h) = s.loop_handle.clone() { h.remove(t) });
                    }
                }).ok());
            }
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
        self.x.idle_check = t.ok();
    }

    fn x_idle_answer(&mut self, programs: Option<usize>) {
        if std::env::var_os("LOOM_X_DEBUG").is_some() {
            eprintln!("loom: x idle check: {programs:?} programs, windows {}", self.x_windows());
        }
        if self.x_windows() {
            return;
        }
        match programs {
            Some(0) => {
                if let Some(c) = self.x.client.take() {
                    eprintln!("loom: no X program left: Xwayland ends");
                    // told directly: a dropped connection is only noticed at the next client event
                    // (not by the connection's credentials: that socketpair's peer is loom itself)
                    if let Some(pid) = c.get_data::<smithay::xwayland::XWaylandClientData>().and_then(|d| d.pid()).filter(|p| *p != std::process::id()) {
                        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                    }
                    self.dh.backend_handle().kill_client(c.id(), smithay::reexports::wayland_server::backend::DisconnectReason::ConnectionClosed);
                }
            }
            // a windowless program still connected (or no answer): asked again later
            _ => self.x_idle_check(),
        }
    }
}

/// The X programs connected (XRes), not counting loom's window manager and this question itself.
fn x_programs(display: &str) -> Option<usize> {
    use x11rb::protocol::res::ConnectionExt;
    let (conn, _) = x11rb::rust_connection::RustConnection::connect(Some(display)).ok()?;
    let clients = conn.res_query_clients().ok()?.reply().ok()?.clients;
    if std::env::var_os("LOOM_X_DEBUG").is_some() {
        eprintln!("loom: x clients: {:?}", clients.iter().map(|c| format!("{:#x}", c.resource_base)).collect::<Vec<_>>());
    }
    // resource base 0 is the server itself
    let n = clients.iter().filter(|c| c.resource_base != 0).count();
    Some(n.saturating_sub(2))
}

impl Loom {
    /// The scale X programs are drawn at (x_apply_scale; 1 without force_zero_scaling).
    pub fn x_client_scale(&self) -> f64 {
        self.x.client.as_ref().and_then(|c| c.get_data::<smithay::xwayland::XWaylandClientData>().map(|d| d.compositor_state.client_scale())).unwrap_or(1.0)
    }
}
