//! The compositor's state: Smithay's protocol states, the windows in a Space, the seat; and the
//! handlers each protocol calls into.

use std::{os::unix::io::OwnedFd, sync::Arc, time::Instant};

use smithay::{
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{layer_map_for_output, LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType},
    input::{Seat, SeatHandler, SeatState},
    output::Output,
    reexports::{
        calloop::LoopSignal,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
            Client, DisplayHandle,
        },
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{get_parent, is_sync_subsurface, with_states, CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
        },
        shell::wlr_layer::{Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState},
        shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
        shm::{ShmHandler, ShmState},
    },
};

use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;

pub struct Loom {
    pub dh: DisplayHandle,
    pub start: Instant,
    pub signal: LoopSignal,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Loom>,
    pub data_device_state: DataDeviceState,
    pub clipboard: crate::clipboard::Clipboard,
    /// kept alive: it serves wl_output and xdg-output
    pub _output_manager_state: OutputManagerState,
    pub seat: Seat<Loom>,
    /// the windows and where they are
    pub space: Space<Window>,
    /// each window's animation (place, fade, scale), by its address (wm.rs)
    pub anims: std::collections::HashMap<u64, crate::anim::WindowAnim>,
    /// something changed since the last frame (a commit, a move): draw again
    pub dirty: bool,
    /// the nested backend's window and renderer (None headless)
    pub nested: Option<crate::nested::Nested>,
    /// where the pointer is (compositor space)
    pub pointer_at: smithay::utils::Point<f64, smithay::utils::Logical>,
    /// frames drawn so far (a test checks an idle desktop draws none)
    pub frames: u64,
    /// menus and other popups, with the window they belong to
    pub popups: PopupManager,
    /// the one output while there is one (headless, nested)
    pub output: Option<Output>,
    /// every screen; `output` is the focused one
    pub outputs: Vec<Output>,
    /// which workspace each screen shows (by screen name), and which screen each workspace belongs to
    pub shown_on: std::collections::HashMap<String, i32>,
    pub ws_on: std::collections::HashMap<i32, String>,
    /// every window with its workspace and address (wm.rs); the workspace shown; the focused one
    pub managed: Vec<crate::wm::Managed>,
    pub next_addr: u64,
    pub active_ws: i32,
    pub focused: Option<Window>,
    /// focused before it had anything to show: the keyboard goes to it when it maps (an enter
    /// sent to an unmapped surface is ignored by Firefox and GTK, and was never sent again)
    pub focus_on_map: Option<Window>,
    /// scripted input sent over IPC (`dispatch loomtest …`), headless only: played one step
    /// every 25 ms, so a test can mix clicks with workspace switches
    pub test_input: std::collections::VecDeque<crate::input::Action>,
    /// when the windows on hidden workspaces last got a frame callback (once a second)
    pub hidden_frames_at: std::time::Instant,
    pub headless: bool,
    /// windows in the order they last had the focus (latest last): who gets it when one goes
    pub focus_history: Vec<Window>,
    /// Hyprland's IPC (hypr_ipc.rs): the event socket's listeners
    pub hypr: Option<crate::hypr_ipc::HyprIpc>,
    /// each workspace's AwesomeWM layout (layouts.rs)
    pub layouts: crate::layouts::Layouts,
    /// Hyprland's Wayland protocols' state (hypr_proto.rs)
    pub proto: crate::hypr_proto::HyprProto,
    /// the docks' window list (foreign.rs)
    pub foreign: crate::foreign::Foreign,
    /// window copies asked for (capture.rs), drawn on the next frame
    pub captures: Vec<crate::capture::Capture>,
    /// screen copies asked for (screencopy.rs)
    pub screen_captures: Vec<crate::screencopy::ScreenCapture>,
    /// frames that changed the screen (copy_with_damage waits for the next one)
    pub shown: u64,
    /// per screencopy client: the frame its last copy showed (its next copy_with_damage waits for a newer one)
    pub copy_seen: std::collections::HashMap<smithay::reexports::wayland_server::backend::ClientId, u64>,
    /// commits per window, and the count each client's last copy saw (copies wait for a change)
    pub commit_count: std::collections::HashMap<u64, u64>,
    pub captured_at: std::collections::HashMap<(Option<smithay::reexports::wayland_server::backend::ClientId>, u64), u64>,
    /// linux-dmabuf when a GPU renderer is there; the headless GPU renderer (gpu.rs)
    /// server-side decorations (decoration.rs): kept alive
    pub _xdg_decoration_state: smithay::wayland::shell::xdg::decoration::XdgDecorationState,
    pub kde_decoration_state: smithay::wayland::shell::kde::decoration::KdeDecorationState,
    pub dmabuf: Option<(smithay::wayland::dmabuf::DmabufState, smithay::wayland::dmabuf::DmabufGlobal)>,
    /// the GPU (dev_t) and the formats + modifiers a screen copy can be drawn into there
    pub copy_dmabuf: Option<(u64, Vec<(u32, Vec<u64>)>)>,
    pub gpu: Option<crate::gpu::Shared>,
    /// the user's Hyprland binds (binds.rs); the actions a key just matched
    pub binds: crate::binds::Binds,
    pub bind_actions: Vec<String>,
    /// global shortcuts a key or button pressed (`dispatch global`): released when it comes up
    /// (Hyprland sends both; the shell's onReleased waits for it), whatever the mods are by then
    pub globals_held: Vec<(crate::hypr_proto::Held, String)>,
    /// the fullscreen window, if one is
    /// the fullscreen windows, one per workspace at most (Hyprland's): one for all screens took a
    /// video out of fullscreen when a game on the other screen went fullscreen
    pub fullscreen: Vec<Window>,
    /// a floating window's place and size before it went fullscreen, by address: given back when it leaves
    pub full_restore: std::collections::HashMap<u64, smithay::utils::Rectangle<i32, smithay::utils::Logical>>,
    /// the repeating bind held (its key, its actions), its timer's generation; the event loop
    pub repeat: Option<(u32, Vec<String>)>,
    pub repeat_gen: u64,
    pub loop_handle: Option<smithay::reexports::calloop::LoopHandle<'static, Loom>>,
    /// a config reload is on its way (config.rs: saves close together become one)
    pub reload_pending: bool,
    /// the special workspace (Super+S) shown over the active one
    pub special_shown: bool,
    /// a window dragged by a mouse bind; the last scroll bind's time; `exec [workspace N] …` rules by pid
    pub drag: Option<crate::pointer::Drag>,
    pub last_scroll_bind: u32,
    pub exec_rules: Vec<(u32, i32, bool)>,
    /// the real screen (tty.rs); the cursor the pointer's program asks for; frame callbacks owed
    pub tty: Option<crate::tty::Tty>,
    pub cursor_status: smithay::input::pointer::CursorImageStatus,
    pub frames_owed: bool,
    /// per-second counts for the stats line (LOOM_STATS=1, always on a console)
    /// the settings (config.rs: Hyprland's, then init.lua)
    pub look: crate::config::Look,
    /// the ii-decor title bars (decor.rs); the maximized window, if one is
    pub decor: std::cell::RefCell<crate::decor::Decor>,
    /// blur behind layers and windows: Hyprland's settings and rules, the shell's Glass files
    pub blur: crate::blur::Config,
    /// the lock screen: open, or locked (with the locker's surface)
    pub lock: crate::lock::Lock,
    pub lock_manager: smithay::wayland::session_lock::SessionLockManagerState,
    /// idle notify (made with the event loop), the surfaces keeping it from idling, the screen off by dpms
    pub idle: Option<smithay::wayland::idle_notify::IdleNotifierState<Loom>>,
    pub inhibitors: Vec<WlSurface>,
    pub screen_off: bool,
    /// touchpad gestures: the `gesture =` lines, the one under way
    pub gesture_cfg: crate::gestures::Config,
    pub gesture: Option<crate::gestures::Active>,
    /// the surface an open hold gesture began on
    pub hold_to: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    /// Hyprland's window rules; floating windows to centre once their size is known
    pub win_rules: crate::winrules::Rules,
    /// borders and shadows (Hyprland's general / decoration:shadow)
    pub frames_cfg: crate::frames::Config,
    /// decoration:screen_shader as the config says (read at load and reload, not every frame)
    pub shader_conf: String,
    pub center_pending: std::collections::HashSet<u64>,
    /// colour matrices per screen (night light), applied again when a screen comes (back)
    pub ctm: std::collections::HashMap<String, [f64; 9]>,
    /// loom's night light (nightlight.rs): every screen's colour unless a CTM client sets its own
    pub night: crate::nightlight::Night,
    /// options set at run time by `hyprctl keyword` (getoption answers them)
    pub keywords: std::collections::HashMap<String, String>,
    /// the input devices and their settings (TTY)
    pub inputs: crate::input_cfg::Devices,
    /// the touchscreen's fingers down, and the surface each went down on
    pub touch_slots: crate::touch::Slots,
    /// xdg-activation, shortcuts inhibitors, xdg-foreign
    pub protocols: crate::protocols::Protocols,
    pub maximized: Option<Window>,
    /// KDE's effects (kwin_fx; loom.opt.animations = "kwin"), the windows they were told of, closed windows still drawn
    pub fx: Option<crate::kwin_fx::Fx>,
    pub fx_seen: std::collections::HashSet<u64>,
    pub ghosts: Vec<crate::kwin_fx::glue::Ghost>,
    /// layer animations (Hyprland's layersIn/Out): settings, the opening ones, the shown layers, closing ghosts
    pub layer_cfg: crate::layer_anim::Config,
    pub layer_anims: std::collections::HashMap<smithay::reexports::wayland_server::backend::ObjectId, crate::layer_anim::LayerAnim>,
    pub layer_shown: std::collections::HashSet<smithay::reexports::wayland_server::backend::ObjectId>,
    /// layers asking for the keyboard (on demand or exclusive) while shown
    pub layer_kb_wanted: std::collections::HashSet<smithay::reexports::wayland_server::backend::ObjectId>,
    /// menus that asked for a grab (xdg_popup.grab), the top one last: they have the keyboard,
    /// and a click outside them closes them all
    pub popup_grab: Vec<smithay::wayland::shell::xdg::PopupSurface>,
    /// where the keyboard was before the first of them: it goes back there when they close
    pub popup_grab_before: Option<crate::focus::KbFocus>,
    pub layer_ghosts: Vec<crate::layer_anim::LayerGhost>,
    /// closed windows animating out (windowsOut)
    pub win_ghosts: Vec<crate::layer_anim::WinGhost>,
    pub fx_last: std::collections::HashMap<u64, (smithay::backend::renderer::gles::GlesTexture, smithay::utils::Size<i32, smithay::utils::Logical>, smithay::utils::Point<i32, smithay::utils::Logical>)>,
    pub stats_on: bool,
    pub stats: Stats,
    /// frame pacing (event driven, no ticking timer): the last frame drawn, a wake-up for the next one armed
    pub last_frame: Instant,
    pub frame_wake_armed: bool,
    /// the Wayland socket programs connect to (WAYLAND_DISPLAY for what loom starts)
    pub socket_name: Option<String>,
    /// Xwayland's window manager once it runs, its display (DISPLAY=:N for what loom starts)
    pub xwm: Option<smithay::xwayland::X11Wm>,
    pub x_display: Option<u32>,
    /// `loom restart` asked: the loop stops and the new binary takes over (restart.rs)
    pub restart: bool,
    /// a restart waiting for the X programs it asked to close
    pub restart_pending: bool,
    /// hyprctl dispatch loomdebug cursor: cursor changes logged
    pub debug_cursor: bool,
    /// hyprctl dispatch loomdebug clicks: each press and release, what is under it, who took it
    pub debug_clicks: bool,
    /// the login screen's last picture over a session just started (--curtain DIR; curtain.rs)
    pub curtain: Option<crate::curtain::Curtain>,
    /// drag and drop into X programs (XDND), connected at the first such drag
    pub xdnd: Option<crate::xdnd::Xdnd>,
    /// KDE's output devices and their clients (System Settings › Display, kscreen-doctor)
    pub kde: crate::kde_output::State,
    /// loom-x, the own X server, for the programs init.lua gives it (own_x.rs)
    pub own_x: std::cell::RefCell<crate::own_x::OwnX>,
    pub x: crate::xwayland::XState,
    pub xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState,
}

impl Loom {
    pub fn new(dh: DisplayHandle, signal: LoopSignal) -> Loom {
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        let (delay, rate) = crate::config::key_repeat();
        // the layout the config names; the default one if it does not load
        let [model, layout, variant, options] = crate::config::xkb_names();
        let xkb = smithay::input::keyboard::XkbConfig { model: &model, layout: &layout, variant: &variant, options: Some(options).filter(|o| !o.is_empty()), ..Default::default() };
        if seat.add_keyboard(xkb, delay, rate).is_err() {
            eprintln!("loom: keyboard layout {layout:?} ({variant:?}) does not load; using the default");
            seat.add_keyboard(Default::default(), delay, rate).expect("keyboard");
        }
        seat.add_pointer();
        seat.add_touch();
        crate::hypr_proto::init(&dh);
        crate::scale::init(&dh);
        crate::foreign::init(&dh);
        let (xdg_deco, kde_deco) = crate::decoration::init(&dh);
        crate::hypr_proto::init_mapping(&dh);
        crate::capture::init(&dh);
        crate::screencopy::init(&dh);
        crate::image_copy::init(&dh);
        crate::idle::init(&dh);
        crate::tablet::init(&dh);
        crate::gestures::init(&dh);
        let mut s = Loom {
            compositor_state: CompositorState::new::<Loom>(&dh),
            xdg_shell_state: XdgShellState::new::<Loom>(&dh),
            layer_shell_state: WlrLayerShellState::new::<Loom>(&dh),
            shm_state: ShmState::new::<Loom>(&dh, vec![]),
            data_device_state: DataDeviceState::new::<Loom>(&dh),
            clipboard: crate::clipboard::Clipboard::new(&dh),
            _output_manager_state: OutputManagerState::new_with_xdg_output::<Loom>(&dh),
            seat_state,
            seat,
            space: Space::default(),
            popups: PopupManager::default(),
            anims: Default::default(),
            dirty: true,
            frames: 0,
            nested: None,
            pointer_at: (0.0, 0.0).into(),
            output: None,
            outputs: Vec::new(),
            shown_on: Default::default(),
            ws_on: Default::default(),
            managed: Vec::new(),
            next_addr: 0,
            active_ws: 1,
            focused: None,
            focus_on_map: None,
            test_input: Default::default(),
            hidden_frames_at: std::time::Instant::now(),
            headless: false,
            focus_history: Vec::new(),
            hypr: None,
            layouts: crate::layouts::Layouts::load(),
            proto: Default::default(),
            foreign: Default::default(),
            captures: Vec::new(),
            screen_captures: Vec::new(),
            shown: 0,
            copy_seen: Default::default(),
            commit_count: Default::default(),
            captured_at: Default::default(),
            _xdg_decoration_state: xdg_deco,
            kde_decoration_state: kde_deco,
            dmabuf: None,
            copy_dmabuf: None,
            gpu: None,
            binds: Default::default(),
            bind_actions: Vec::new(),
            globals_held: Vec::new(),
            fullscreen: Vec::new(),
            full_restore: Default::default(),
            repeat: None,
            repeat_gen: 0,
            loop_handle: None,
            reload_pending: false,
            special_shown: false,
            drag: None,
            last_scroll_bind: 0,
            exec_rules: Vec::new(),
            tty: None,
            cursor_status: smithay::input::pointer::CursorImageStatus::default_named(),
            frames_owed: false,
            look: crate::config::load().0,
            decor: Default::default(),
            blur: crate::blur::Config::load(),
            lock: Default::default(),
            lock_manager: crate::lock::init(&dh),
            idle: None,
            inhibitors: Vec::new(),
            screen_off: false,
            gesture_cfg: crate::gestures::Config::load(),
            gesture: None,
            hold_to: None,
            win_rules: crate::winrules::Rules::load(),
            frames_cfg: crate::frames::Config::load(),
            shader_conf: crate::config::screen_shader_conf(),
            center_pending: Default::default(),
            ctm: Default::default(),
            night: Default::default(),
            keywords: Default::default(),
            inputs: Default::default(),
            touch_slots: Vec::new(),
            protocols: crate::protocols::init(&dh),
            kde: {
                crate::kde_output::init(&dh);
                Default::default()
            },
            maximized: None,
            fx: None,
            fx_seen: Default::default(),
            ghosts: Vec::new(),
            layer_cfg: crate::layer_anim::Config::load(),
            layer_anims: Default::default(),
            layer_shown: Default::default(),
            layer_kb_wanted: Default::default(),
            popup_grab: Vec::new(),
            popup_grab_before: None,
            layer_ghosts: Vec::new(),
            win_ghosts: Vec::new(),
            fx_last: Default::default(),
            stats_on: std::env::var_os("LOOM_STATS").is_some(),
            stats: Default::default(),
            last_frame: Instant::now(),
            frame_wake_armed: false,
            socket_name: None,
            xwm: None,
            x_display: crate::restart::x_display(),
            restart: false,
            restart_pending: false,
            debug_cursor: false,
            debug_clicks: false,
            curtain: std::env::var_os("LOOM_CURTAIN").map(|d| crate::curtain::Curtain::new(d.into())),
            xdnd: None,
            own_x: Default::default(),
            x: Default::default(),
            xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState::new::<Loom>(&dh),
            start: Instant::now(),
            signal,
            dh,
        };
        s.apply_animations_setting();
        s
    }

    /// The surface under a point, and where it is: the layers above windows, the windows, the
    /// layers below (a bar, a dock and the desktop take the pointer too).
    pub fn surface_at(&self, at: smithay::utils::Point<f64, smithay::utils::Logical>) -> Option<(WlSurface, smithay::utils::Point<f64, smithay::utils::Logical>)> {
        if self.locked() {
            return self.lock_surface_at(at);
        }
        use smithay::wayland::shell::wlr_layer::Layer as L;
        // the layers of the screen under the pointer; their boxes are relative to it
        let output = self.output_at(at).or_else(|| self.output.clone())?;
        let off = self.space.output_geometry(&output).map(|g| g.loc).unwrap_or_default();
        let map = layer_map_for_output(&output);
        let local = at - off.to_f64();
        let layer_hit = |which: &[L]| layer_input_at(&map, which, local).map(|(_, s, loc)| (s, (loc + off).to_f64()));
        layer_hit(&[L::Overlay, L::Top])
            .or_else(|| {
                self.space.element_under(at).and_then(|(w, loc)| w.surface_under(at - loc.to_f64(), WindowSurfaceType::ALL).map(|(s, sloc)| (s, (sloc + loc).to_f64())))
            })
            .or_else(|| layer_hit(&[L::Bottom, L::Background]))
    }

    /// A window's animation, if it has one.
    pub fn anim_of(&self, w: &Window) -> Option<&crate::anim::WindowAnim> {
        self.managed_of(w).and_then(|m| self.anims.get(&m.addr))
    }

    /// Any animation still running (the next frame must be drawn).
    pub fn animating(&self) -> bool {
        let now = std::time::Instant::now();
        self.curtain_animating(now) || self.anims.values().any(|a| !a.done(now)) || !self.ghosts.is_empty() || self.fx.as_ref().is_some_and(|f| f.animating(now)) || self.layers_animating()
    }

    /// This window has the keyboard and shows as active (xdg activated); the others not.
    pub fn focus_window(&mut self, window: &Window) {
        // a window already gone (its program quit between the event and now): not it
        if !smithay::utils::IsAlive::alive(window) {
            return;
        }
        // not one of loom's windows (an X menu or tooltip, which places itself): never the focus,
        // as in sway and Hyprland. A click on a Bitwig menu made it the focused window; when it
        // closed, nothing had the focus and the keys went to a gone surface.
        if self.managed_of(window).is_none() {
            return;
        }
        self.focus_history.retain(|w| w != window && smithay::utils::IsAlive::alive(w));
        self.focus_history.push(window.clone());
        if self.locked() {
            self.focused = Some(window.clone());
            return;
        }
        if self.focused.as_ref() != Some(window) {
            self.focused = Some(window.clone());
            self.center_master_on_top();
            let addr = self.managed_of(window).map(|m| m.addr).unwrap_or(0);
            self.hypr_event(&format!("activewindow>>{},{}", self.class_of(window), self.title_of(window)));
            self.hypr_event(&format!("activewindowv2>>{addr:x}"));
            self.fx_event("windowActivated", window, &[]);
            self.foreign_all_changed();
        }
        self.space.raise_element(window, true);
        self.keep_fullscreen_on_top();
        // floating ones stay over the tiled one just raised, and X is told the same order
        self.x_sync_stacking();
        for w in self.space.elements() {
            let on = w == window;
            if let Some(x) = w.x11_surface() {
                let _ = x.set_activated(on);
                continue;
            }
            let Some(t) = w.toplevel() else { continue };
            t.with_pending_state(|s| {
                if on {
                    s.states.set(xdg_toplevel::State::Activated);
                } else {
                    s.states.unset(xdg_toplevel::State::Activated);
                }
            });
            // sent only when it differs from the last configure
            if t.is_initial_configure_sent() {
                t.send_pending_configure();
            }
        }
        let target = match (window.toplevel(), window.x11_surface()) {
            (Some(t), _) => Some(crate::focus::KbFocus::Surface(t.wl_surface().clone())),
            // not raised in X here: x_sync_stacking above told X loom's order, and raising the focused
            // one put a fullscreen Bitwig over its floating plugin (black in X, its clicks to Bitwig)
            (_, Some(x)) => Some(crate::focus::KbFocus::X11(x.clone())),
            _ => None,
        };
        let kb = self.seat.get_keyboard().expect("keyboard");
        // a panel that holds the keyboard exclusively keeps it (a window opening behind the
        // launcher or a dialog of the shell); this window has it when the panel lets go
        if self.exclusive_layer_has_keyboard() {
            return;
        }
        // not mapped yet (no buffer): the keyboard follows on its first frame, as niri and sway do
        if let Some(crate::focus::KbFocus::Surface(s)) = &target {
            let mapped = smithay::backend::renderer::utils::with_renderer_surface_state(s, |st| st.buffer().is_some()).unwrap_or(false);
            if !mapped {
                self.focus_on_map = Some(window.clone());
                return;
            }
        }
        if self.focus_on_map.as_ref() == Some(window) {
            self.focus_on_map = None;
        }
        if let Some(t) = target {
            if kb.current_focus().as_ref() != Some(&t) {
                kb.set_focus(self, Some(t), smithay::utils::SERIAL_COUNTER.next_serial());
            }
        }
    }

    /// The keyboard is on a layer surface that asked for it exclusively and still shows.
    pub fn exclusive_layer_has_keyboard(&self) -> bool {
        let Some(f) = self.seat.get_keyboard().and_then(|k| k.current_focus()).and_then(|f| f.surface()) else { return false };
        smithay::reexports::wayland_server::Resource::is_alive(&f)
            && self.layer_kb_wanted.contains(&smithay::reexports::wayland_server::Resource::id(&f))
            && self.layer_keyboard(&f) == smithay::wayland::shell::wlr_layer::KeyboardInteractivity::Exclusive
    }

    /// The milliseconds since the start, for frame callbacks.
    pub fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }
}

impl CompositorHandler for Loom {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(x) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &x.compositor_state;
        }
        &client.get_data::<ClientState>().expect("client state").compositor_state
    }
    fn commit(&mut self, surface: &WlSurface) {
        // a new picture on the cursor surface: Xwayland changes an X program's cursor (Bitwig) this
        // way, on the surface it already set, so no new cursor request comes; drawn only on the next
        // redraw, it changed only on clicks
        if matches!(&self.cursor_status, smithay::input::pointer::CursorImageStatus::Surface(c) if c == surface) {
            if self.debug_cursor {
                eprintln!("loom: cursor new picture");
            }
            self.dirty = true;
        }
        // what kind of buffer it brought (GPU or CPU memory): the stats line says how programs draw
        if self.stats_on {
            let kind = smithay::wayland::compositor::with_states(surface, |s| {
                s.cached_state.get::<smithay::wayland::compositor::SurfaceAttributes>().current().buffer.as_ref().and_then(|b| match b {
                    smithay::wayland::compositor::BufferAssignment::NewBuffer(buf) => smithay::backend::renderer::buffer_type(buf),
                    _ => None,
                })
            });
            // who: the program, by its process name (read once a second per program)
            let who = |s: &mut Loom| {
                let c = smithay::reexports::wayland_server::Resource::client(surface)?;
                if let Some(n) = s.stats.names.get(&c.id()) {
                    return Some(n.clone());
                }
                let n = c.get_credentials(&s.dh).ok().map(|c| std::fs::read_to_string(format!("/proc/{}/comm", c.pid)).unwrap_or_default().trim().to_string())?;
                s.stats.names.insert(c.id(), n.clone());
                Some(n)
            };
            match kind {
                Some(smithay::backend::renderer::BufferType::Dma) => {
                    self.stats.dma += 1;
                    let w = who(self).unwrap_or_default();
                    *self.stats.dma_by.entry(w).or_default() += 1;
                }
                Some(smithay::backend::renderer::BufferType::Shm) => {
                    self.stats.shm += 1;
                    let w = who(self).unwrap_or_default();
                    *self.stats.shm_by.entry(w).or_default() += 1;
                }
                _ => {}
            }
        }
        // a layer taking its picture away: its last frame kept (before the buffer is let go)
        let removing = with_states(surface, |s| matches!(s.cached_state.get::<smithay::wayland::compositor::SurfaceAttributes>().current().buffer, Some(smithay::wayland::compositor::BufferAssignment::Removed)));
        if removing {
            self.layer_committed(surface, true);
        }
        // an X surface's scale set as it was associated (this commit): its buffer read at it already
        let own = smithay::wayland::compositor::surface_scale(surface, 0.0);
        if own > 0.0 {
            with_states(surface, |s| s.cached_state.get::<smithay::wayland::compositor::SurfaceAttributes>().current().client_scale = own);
        }
        on_commit_buffer_handler::<Self>(surface);
        self.dirty = true;
        self.popups.commit(surface);
        // a layer surface (bar, dock, panel): arranged on its output, its first configure sent
        if let Some((output, _)) = self.layer_output_of(surface) {
            let mut map = layer_map_for_output(&output);
            if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned() {
                let sent = with_states(surface, |s| s.data_map.get::<LayerSurfaceData>().map(|d| d.lock().unwrap().initial_configure_sent).unwrap_or(true));
                let moved = map.arrange();
                if !sent {
                    layer.layer_surface().send_configure();
                }
                drop(map);
                if !removing {
                    self.layer_committed(surface, false);
                    self.curtain_layer(&layer.namespace().to_string());
                }
                if moved {
                    self.relayout();
                }
                return;
            }
        }
        // a popup's first commit: its place (from its positioner)
        if let Some(PopupKind::Xdg(p)) = self.popups.find_popup(surface) {
            if !p.is_initial_configure_sent() {
                let _ = p.send_configure();
            }
        }
        if is_sync_subsurface(surface) {
            return;
        }
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let found = self.space.elements().find(|w| smithay::wayland::seat::WaylandFocus::wl_surface(*w).is_some_and(|s| *s == root)).cloned();
        if let Some(window) = found {
            window.on_commit();
            // an X window (a menu just mapped too): drawn at its screen's scale from its next frame
            if window.x11_surface().is_some() {
                self.x_scales();
            }
            if let Some(a) = self.managed_of(&window).map(|m| m.addr) {
                let n = self.commit_count.entry(a).or_default();
                *n += 1;
                // an X window's first frame: its place told again. Java (Bitwig) drops the configure
                // sent while it maps, keeps its startup position and opens menus there (other screen)
                if *n == 1 {
                    if let Some(x) = window.x11_surface() {
                        let _ = x.configure(None);
                    }
                }
            }
            // its first frame with something to show: the opening animation starts now
            let has_buffer = smithay::backend::renderer::utils::with_renderer_surface_state(&root, |s| s.buffer().is_some()).unwrap_or(false);
            if has_buffer {
                // its first picture: one that waited past DRAW_WAIT has no tile yet, it gets one now
                let late = self.managed.iter_mut().find(|m| m.window == window && !m.drawn).map(|m| {
                    m.drawn = true;
                    m.since.elapsed() >= crate::wm::DRAW_WAIT
                });
                if late == Some(true) {
                    self.relayout();
                }
                self.center_if_pending(&window);
                // it was focused before it could take the keyboard: now it gets it
                if self.focus_on_map.as_ref() == Some(&window) {
                    self.focus_on_map = None;
                    if self.focused.as_ref() == Some(&window) {
                        self.focus_window(&window);
                    }
                }
                // at the size it was given (an X window or one with no size asked: always)
                let fits = window.toplevel().is_none_or(|t| {
                    let (want, got) = (t.with_pending_state(|s| s.size), window.geometry().size);
                    want.is_none_or(|w| (w.w == 0 || (w.w - got.w).abs() <= 2) && (w.h == 0 || (w.h - got.h).abs() <= 2))
                });
                if std::env::var_os("LOOM_DEBUG_ANIM").is_some() {
                    eprintln!("loom: anim commit {:?} fits {fits} (asked {:?})", window.geometry(), window.toplevel().map(|t| t.with_pending_state(|s| s.size)));
                }
                if let Some(a) = self.managed_of(&window).map(|m| m.addr).and_then(|addr| self.anims.get_mut(&addr)) {
                    a.shown(std::time::Instant::now(), fits);
                }
                if self.fx.is_some() {
                    self.fx_keep(&window, &root);
                }
                if let Some(addr) = self.managed_of(&window).map(|m| m.addr).filter(|a| self.fx.is_some() && !self.fx_seen.contains(a)) {
                    self.fx_seen.insert(addr);
                    self.fx_event("windowAdded", &window, &[]);
                }
            }
            // the first commit: its class and title are known, the rules decide (floating, size …),
            // then the window learns its size
            if let Some(t) = window.toplevel() {
                if !t.is_initial_configure_sent() {
                    self.apply_window_rules(&window);
                    // laid out again now its class is known (made before it, it got the default
                    // title bar's room; an app that draws its own then sat 28 px too low)
                    self.relayout();
                    t.send_configure();
                }
            }
        }
    }
}

impl XdgShellHandler for Loom {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    fn title_changed(&mut self, surface: ToplevelSurface) {
        let Some(w) = self.managed.iter().find(|m| m.window.toplevel() == Some(&surface)).map(|m| m.window.clone()) else { return };
        self.title_changed_of(&w);
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.manage(Window::new_wayland_window(surface));
    }
    // the app id came later than the first frame (Zen does): its rules and title bar again now.
    // Laid out for the default title bar until then, Zen sat 28 px low and jumped up at the
    // next workspace switch, so its buttons were not where they had been.
    // the program asks to be moved (its own title bar, Zen's drag areas): loom moves it while the
    // button is held, as a Super+drag would; ignored, GTK and Firefox waited for a move that never came
    fn move_request(&mut self, surface: ToplevelSurface, _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat, _serial: Serial) {
        let Some(w) = self.managed.iter().find(|m| m.window.toplevel() == Some(&surface)).map(|m| m.window.clone()) else { return };
        let Some(pointer) = self.seat.get_pointer() else { return };
        // only while the press that asked is still down
        if !pointer.is_grabbed() || self.drag.is_some() || self.is_full(&w) {
            return;
        }
        self.drag_start_on(&w, false, 0x110);
        if let Some(d) = self.drag.as_mut() {
            d.from_client = true;
        }
    }
    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        let Some(w) = self.managed.iter().find(|m| m.window.toplevel() == Some(&surface)).map(|m| m.window.clone()) else { return };
        if !surface.is_initial_configure_sent() {
            return;
        }
        self.apply_window_rules(&w);
        self.relayout();
    }
    // the program asks (F11, a video's fullscreen button): as the bind does
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>) {
        match self.managed.iter().find(|m| m.window.toplevel() == Some(&surface)).map(|m| m.window.clone()) {
            Some(w) => self.set_fullscreen(&w, true),
            // not shown yet: the configure it waits for says no (the protocol wants an answer)
            None => {
                surface.send_configure();
            }
        };
    }
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(w) = self.managed.iter().find(|m| m.window.toplevel() == Some(&surface)).map(|m| m.window.clone()) {
            self.set_fullscreen(&w, false);
        }
    }
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let gone = self.managed.iter().find(|m| m.window.toplevel() == Some(&surface)).map(|m| m.window.clone());
        if let Some(w) = gone {
            self.unmanage(&w);
        }
    }
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        if std::env::var_os("LOOM_DEBUG_POPUP").is_some() {
            eprintln!("loom: popup new {:?} geometry {:?}", smithay::reexports::wayland_server::Resource::id(surface.wl_surface()), positioner.get_geometry());
        }
        surface.with_pending_state(|s| s.geometry = positioner.get_geometry());
        // on its screen (a window's menu; a panel's is placed once its parent is known)
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }
    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        if std::env::var_os("LOOM_DEBUG_POPUP").is_some() {
            eprintln!("loom: popup grab {:?}", smithay::reexports::wayland_server::Resource::id(surface.wl_surface()));
        }
        self.popup_grab_start(surface);
    }
    fn popup_destroyed(&mut self, surface: PopupSurface) {
        if std::env::var_os("LOOM_DEBUG_POPUP").is_some() {
            eprintln!("loom: popup destroyed {:?}", smithay::reexports::wayland_server::Resource::id(surface.wl_surface()));
        }
        self.popup_grab_gone(&surface);
    }
    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }
}

impl WlrLayerShellHandler for Loom {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }
    fn new_layer_surface(&mut self, surface: WlrLayerSurface, wl_output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>, _layer: Layer, namespace: String) {
        // on the screen it asked for, else the focused one
        let Some(output) = wl_output.as_ref().and_then(Output::from_resource).or_else(|| self.output.clone()) else { return };
        let mut map = layer_map_for_output(&output);
        let _ = map.map_layer(&LayerSurface::new(surface, namespace));
    }
    // a panel's menu (the bar's, the dock's): on the screen, now that its parent is known
    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        self.unconstrain_popup(&popup);
    }
    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let Some((output, _)) = self.layer_output_of(surface.wl_surface()) else { return };
        let mut map = layer_map_for_output(&output);
        let gone = map.layers().find(|l| l.layer_surface() == &surface).cloned();
        if let Some(l) = gone {
            // the keyboard back, if it had it
            drop(map);
            self.layer_keyboard_changed(l.wl_surface(), false);
            map = layer_map_for_output(&output);
            // still shown: its last frame animates out
            let geo = map.layer_geometry(&l).unwrap_or_default();
            if self.layer_shown.contains(&smithay::reexports::wayland_server::Resource::id(l.wl_surface())) {
                drop(map);
                self.layer_closing(l.wl_surface(), l.layer(), &l.namespace().to_string(), geo);
                map = layer_map_for_output(&output);
            }
            map.unmap_layer(&l);
        }
        drop(map);
        self.relayout();
    }
}

impl BufferHandler for Loom {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Loom {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for Loom {
    type KeyboardFocus = crate::focus::KbFocus;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: smithay::input::pointer::CursorImageStatus) {
        // hyprctl dispatch loomdebug cursor: every change, who asked, how big (Bitwig's cursors changed only on clicks)
        if self.debug_cursor {
            let what = match &image {
                smithay::input::pointer::CursorImageStatus::Hidden => "hidden".to_string(),
                smithay::input::pointer::CursorImageStatus::Named(n) => format!("named {}", n.name()),
                smithay::input::pointer::CursorImageStatus::Surface(s) => {
                    let from_x = smithay::reexports::wayland_server::Resource::client(s).is_some_and(|c| self.x.client.as_ref().is_some_and(|x| x.id() == c.id()));
                    let size = smithay::backend::renderer::utils::with_renderer_surface_state(s, |st| st.surface_size()).flatten();
                    format!("surface from_x={from_x} size={size:?}")
                }
            };
            // what is under it: Bitwig's cursor reset to default with the pointer still (focus changes?)
            let under = self.surface_at(self.pointer_at).map(|(sf, _)| {
                let who = smithay::reexports::wayland_server::Resource::client(&sf).and_then(|c| c.get_credentials(&self.dh).ok()).map(|c| std::fs::read_to_string(format!("/proc/{}/comm", c.pid)).unwrap_or_default().trim().to_string()).unwrap_or_default();
                let layer = self.layer_output_of(&sf).map(|(_, l)| format!(" layer {}", l.namespace())).unwrap_or_default();
                format!("{who}{layer}")
            }).unwrap_or_else(|| "nothing".into());
            eprintln!("loom: cursor {} at {:.0},{:.0} over {}", what, self.pointer_at.x, self.pointer_at.y, under);
        }
        self.cursor_status = image;
        self.dirty = true;
    }
    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: smithay::input::keyboard::LedState) {
        self.inputs.leds(led_state);
    }
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&crate::focus::KbFocus>) {
        // LOOM_FOCUS_TRACE: every keyboard focus change (debugging)
        if std::env::var_os("LOOM_FOCUS_TRACE").is_some() {
            let what = focused.and_then(|f| f.surface()).map(|s| { let ns = self.layer_ns(&s); if ns != "?" { ns } else { self.space.elements().find(|w| smithay::wayland::seat::WaylandFocus::wl_surface(*w).as_deref() == Some(&s)).map(|w| self.class_of(w)).unwrap_or_else(|| "?".into()) } });
            eprintln!("loom: keyboard focus -> {}", what.unwrap_or_else(|| "none".into()));
        }
        self.clipboard_focus(seat, focused);
    }
}

impl DataDeviceHandler for Loom {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
impl ClientDndGrabHandler for Loom {
    // over an X window the drag is loom's to carry (xdnd.rs)
    fn dnd_foreign_target(&mut self, s: &WlSurface) -> bool {
        self.xdnd_target(s)
    }

    fn dnd_foreign_motion(&mut self, s: &WlSurface, at: smithay::utils::Point<f64, smithay::utils::Logical>, source: &smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource, _time: u32) {
        self.xdnd_motion(s, at, source);
    }

    fn dnd_foreign_leave(&mut self) {
        self.xdnd_leave();
    }

    fn dnd_foreign_drop(&mut self, _source: &smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource) -> bool {
        self.xdnd_drop()
    }
}
// a drag loom carries for an X program (xdnd.rs)
impl ServerDndGrabHandler for Loom {
    fn accept(&mut self, mime_type: Option<String>, _seat: Seat<Self>) {
        self.xdnd_accept(mime_type);
    }

    fn send(&mut self, mime_type: String, fd: OwnedFd, _seat: Seat<Self>) {
        self.xdnd_send(&mime_type, fd);
    }

    fn finished(&mut self, _seat: Seat<Self>) {
        self.xdnd_finished(true);
    }

    fn cancelled(&mut self, _seat: Seat<Self>) {
        self.xdnd_finished(false);
    }
}

impl OutputHandler for Loom {}

/// What loom keeps per client.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

pub fn client_data() -> Arc<ClientState> {
    Arc::new(ClientState::default())
}

delegate_compositor!(Loom);
delegate_xdg_shell!(Loom);
delegate_shm!(Loom);
delegate_seat!(Loom);
delegate_data_device!(Loom);
delegate_output!(Loom);
delegate_layer_shell!(Loom);

/// The time between frames (60 Hz; the real screen's refresh with M4).
pub const FRAME: std::time::Duration = std::time::Duration::from_micros(16_667);

impl Loom {
    /// Something to draw, and the last frame is a frame's time ago.
    pub fn frame_due(&self) -> bool {
        (self.dirty || self.animating()) && self.last_frame.elapsed() >= FRAME
    }

    /// The loop wakes in `d` (it sleeps otherwise until a client, input or a timer wakes it).
    pub fn wake_in(&mut self, d: std::time::Duration) {
        if let Some(h) = &self.loop_handle {
            let _ = h.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(d), |_, _, _| smithay::reexports::calloop::timer::TimeoutAction::Drop);
        }
    }

    /// After each turn of the event loop: a frame waiting for its time gets one wake-up then
    /// (never a ticking timer); what the clients were sent goes out.
    pub fn after_dispatch(&mut self) {
        self.fx_tick();
        self.layer_anims_tick(std::time::Instant::now());
        // something on screen changed: a still pointer may be over something else now
        if self.dirty {
            self.pointer_refresh();
        }
        if self.captures_ready() && !self.frame_wake_armed {
            self.wake_in(std::time::Duration::ZERO);
        }
        // on the real screen the vblank (or the session coming back) wakes the next frame: no timer
        let tty_waits = self.tty.as_ref().is_some_and(|t| t.screens.iter().all(|s| s.pending) || !t.active);
        if (self.dirty || self.animating()) && !self.frame_wake_armed && !tty_waits {
            let left = FRAME.saturating_sub(self.last_frame.elapsed());
            self.frame_wake_armed = true;
            if let Some(h) = &self.loop_handle {
                let _ = h.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(left), |_, _, s: &mut Loom| {
                    s.frame_wake_armed = false;
                    smithay::reexports::calloop::timer::TimeoutAction::Drop
                });
            }
        }
        let _ = self.dh.flush_clients();
    }
}

/// What a second held: frames shown, render time, buffers programs brought (GPU / CPU memory).
#[derive(Default)]
pub struct Stats {
    pub frames: u32,
    pub render_us: u64,
    pub render_max_us: u64,
    pub dma: u32,
    pub shm: u32,
    pub shm_by: std::collections::HashMap<String, u32>,
    pub dma_by: std::collections::HashMap<String, u32>,
    /// program names by client, for the counts
    pub names: std::collections::HashMap<smithay::reexports::wayland_server::backend::ClientId, String>,
}

impl Loom {
    /// Once a second while stats are on: one line to the log, the counts started again.
    pub fn start_stats(&mut self) {
        self.stats_on = true;
        // a line a minute (LOOM_STATS=1: a second, for measuring); 4000 lines an hour were noise
        let every = if std::env::var_os("LOOM_STATS").is_some() { 1 } else { 60 };
        let Some(h) = self.loop_handle.clone() else { return };
        let _ = h.insert_source(smithay::reexports::calloop::timer::Timer::from_duration(std::time::Duration::from_secs(every)), move |_, _, s: &mut Loom| {
            let st = std::mem::take(&mut s.stats);
            // nothing happened: no line (an idle desktop stays quiet in the log too)
            if st.frames + st.dma + st.shm > 0 {
                let avg = if st.frames > 0 { st.render_us / st.frames as u64 } else { 0 };
                // the programs sending the most, GPU and CPU buffers apart
                // every count per second, as the frames are (the buffers were per minute beside
                // frames per second: 10 000 read as a storm that was 170 a second)
                let per = |n: u32| (n as f64 / every as f64).round() as u32;
                let top = |m: &std::collections::HashMap<String, u32>| {
                    let mut by: Vec<_> = m.iter().collect();
                    by.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
                    let by: Vec<String> = by.iter().take(3).map(|(w, n)| format!("{w} {}", per(**n))).collect();
                    if by.is_empty() { String::new() } else { format!(" ({})", by.join(", ")) }
                };
                eprintln!("loom: stats {} frames/s, render {:.2} ms avg {:.2} max, buffers/s {} gpu{} {} cpu{}", per(st.frames), avg as f64 / 1000.0, st.render_max_us as f64 / 1000.0, per(st.dma), top(&st.dma_by), per(st.shm), top(&st.shm_by));
            }
            smithay::reexports::calloop::timer::TimeoutAction::ToDuration(std::time::Duration::from_secs(every))
        });
    }
}

/// The top-most layer on `which` (in that order) taking input at `local`, a point on `map`'s
/// screen: its input region decides, not its box. (The bar's surface is 600 px tall and passes
/// input through below the bar; by its box it took every click in the top of the screen, so a
/// window clicked there never got the keyboard.) The layer, the surface hit (it, a popup or a
/// subsurface) and where that surface is.
pub fn layer_input_at(
    map: &smithay::desktop::LayerMap,
    which: &[smithay::wayland::shell::wlr_layer::Layer],
    local: smithay::utils::Point<f64, smithay::utils::Logical>,
) -> Option<(LayerSurface, WlSurface, smithay::utils::Point<i32, smithay::utils::Logical>)> {
    which.iter().find_map(|l| {
        map.layers_on(*l).rev().find_map(|ly| {
            let geo = map.layer_geometry(ly)?;
            ly.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL).map(|(s, loc)| (ly.clone(), s, loc + geo.loc))
        })
    })
}

impl Loom {
    /// A popup kept on its screen as its positioner allows (flip, slide, resize): a menu opened
    /// at the screen's edge went half off it. The popup's place is relative to its parent.
    pub fn unconstrain_popup(&self, popup: &smithay::wayland::shell::xdg::PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = smithay::desktop::find_popup_root_surface(&kind) else { return };
        // where the root is on the screen (a window: its content's corner; a panel: its box), and that screen
        let place = if let Some(w) = self.space.elements().find(|w| smithay::wayland::seat::WaylandFocus::wl_surface(*w).as_deref() == Some(&root)) {
            // the screen its middle is on: the first one it touched could be the other screen, which
            // its shadow reaches (Zen's), and its menus were kept to that one, half off this one
            let g = self.space.element_geometry(w);
            let mid = g.map(|g| smithay::utils::Point::<f64, smithay::utils::Logical>::from((g.loc.x as f64 + g.size.w as f64 / 2.0, g.loc.y as f64 + g.size.h as f64 / 2.0)));
            let o = mid.and_then(|m| self.output_at(m)).or_else(|| self.space.outputs_for_element(w).into_iter().next()).or_else(|| self.output.clone());
            g.map(|g| g.loc).zip(o)
        } else if let Some((o, l)) = self.layer_output_of(&root) {
            let off = self.space.output_geometry(&o).map(|g| g.loc).unwrap_or_default();
            let geo = layer_map_for_output(&o).layer_geometry(&l);
            geo.map(|g| (g.loc + off, o.clone()))
        } else {
            None
        };
        let Some((root_loc, output)) = place else { return };
        let Some(mut target) = self.space.output_geometry(&output) else { return };
        target.loc -= smithay::desktop::get_popup_toplevel_coords(&kind);
        target.loc -= root_loc;
        popup.with_pending_state(|s| s.geometry = s.positioner.get_unconstrained_geometry(target));
    }
}
