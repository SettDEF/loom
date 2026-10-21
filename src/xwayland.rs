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
        let h2 = h.clone();
        let token = h.insert_source(xwayland, move |event, _, state: &mut Loom| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => match X11Wm::start_wm(h2.clone(), x11_socket, client.clone()) {
                Ok(wm) => {
                    state.xwm = Some(wm);
                    state.x.starting = false;
                    eprintln!("loom: Xwayland started for the first X program, DISPLAY=:{display_number}");
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
        let Some(h) = self.loop_handle.clone() else { return };
        if let Some(t) = self.x.server.take() {
            h.remove(t);
        }
        // the old instance lets go of the display's lock as it is dropped: watch it again then
        h.insert_idle(|state| {
            let Some(h) = state.loop_handle.clone() else { return };
            if let Err(e) = start(&h, state) {
                eprintln!("loom: X display not watched again: {e}");
            } else {
                eprintln!("loom: Xwayland ended with its last X program; DISPLAY=:{} waits for the next", state.x_display.unwrap_or(0));
            }
        });
    }
}
/// Floats rather than tiles: dialogs, utilities, splash screens, anything transient for a window.
fn floats(x: &X11Surface) -> bool {
    x.is_transient_for().is_some() || matches!(x.window_type(), Some(WmWindowType::Dialog | WmWindowType::Utility | WmWindowType::Splash | WmWindowType::Toolbar))
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
        }
    }
}

impl XwmHandler for Loom {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("the X window manager")
    }
    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, x: X11Surface) {
        let _ = x.set_mapped(true);
        let float = floats(&x);
        let w = Window::new_x11_window(x.clone());
        if float {
            // centred on the screen, at the size it asked for
            let size = x.geometry().size;
            let screen = self.output.as_ref().and_then(|o| self.space.output_geometry(o)).unwrap_or_default();
            let loc = (screen.loc.x + (screen.size.w - size.w) / 2, screen.loc.y + (screen.size.h - size.h) / 2);
            let _ = x.configure(Rectangle::new(loc.into(), size));
            self.manage_floating(w, loc.into());
        } else {
            self.manage(w);
        }
    }
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, x: X11Surface) {
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
        if property == smithay::xwayland::xwm::WmWindowProperty::Title {
            if let Some(w) = self.x_window(&x) {
                self.title_changed_of(&w);
            }
        }
    }
    fn resize_request(&mut self, _xwm: XwmId, _x: X11Surface, _button: u32, _edge: ResizeEdge) {}
    fn disconnected(&mut self, _xwm: XwmId) {
        self.x_ended();
    }
    fn move_request(&mut self, _xwm: XwmId, _x: X11Surface, _button: u32) {}
}

impl XWaylandShellHandler for Loom {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}
smithay::delegate_xwayland_shell!(Loom);
