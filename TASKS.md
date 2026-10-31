# loom — tasks

The milestones of [PLAN.md](PLAN.md), each step a line; ticked when it works with a real client
(the test named after it). Dotos shows this list (Projects › loom).

## 0. Skeleton

- [x] headless backend: pixman, frames at 60 Hz, `--snapshot` PNG (alacritty shows in it)
- [x] wl_compositor, wl_subcompositor, wl_shm, xdg-shell, wl_seat, data device, wl_output, xdg-output (wayland-info lists them)
- [x] keyboard and pointer input in the headless backend (scripted: `--keys`, `--click`), focus follows clicks
- [x] several windows (two alacritty side by side in one picture)
- [x] popups (xdg_popup: placed from their positioner; a Quickshell PopupWindow shows where anchored)
- [x] nested backend (winit, GL, dmabuf) for looking at it live: zash runs in it with the real keyboard and mouse

## 1. Windows

- [x] wlr-layer-shell first (docks, the shell's bar, panels; exclusive zones keep windows clear: a Quickshell bar and dock with a window between)
- [~] animations: windows open (fade + grow, from their first frame) and move (glide) with Hyprland's bezier curves; close, resize, workspaces next
- [x] frames only on damage (no 60 Hz loop while nothing changes: 35 frames in 6 s, all of them animation or content)
- [x] dwindle tiling and floating, as Hyprland's defaults (gaps 5 / 10)
- [x] the user's AwesomeWM layouts (tile, tileleft, tilebottom, tiletop, fair, fairh, spiral, dwindle, max, corner nw/ne/sw/se; mwfact, nmaster, ncol per workspace) by `hyprctl dispatch layoutmsg` as the shell sends them; state read from Hyprland's file, written to loom's own
- [x] workspaces (any number, switched and moved to over IPC)
- [x] keybinds: the user's own Hyprland config read as it is (sources, $vars, submaps, bind flags r e n i d, catchall, code:N, Super alone); 345 binds; one hash lookup per key, no allocation unless one matches; `keyword bind/unbind`, `hyprctl binds`, `submap`
- [x] dispatchers: workspace, movetoworkspace(silent), exec, killactive, togglefloating, focuswindow, global, layoutmsg, submap, movefocus, cyclenext, bringactivetotop, fullscreen
- [x] sendshortcut (vim mode: h → Left, tested), movewindow, togglespecialworkspace + special workspace, pin, resizeactive; repeating binds (binde, held keys repeat at 600 ms / 25 Hz)
- [x] mouse binds: bindm Super+drag moves (a tiled window floats while moved, is tiled again in the slot it is dropped on), Super+right-drag resizes (a tiled one moves the layout's master share), mouse:N buttons, Super+scroll (300 ms apart, as Hyprland); one pointer path for nested and headless (--sdrag, --sscroll)
- [x] Hyprland 0.56's Lua dispatch (`hl.dsp.focus({ workspace = 3 })` …, the 25 the shell sends) read and carried out; focuswindow by address:/class:/title:/pid:; movewindowpixel, movecursor, exec [workspace N silent]; focus moves on when a window leaves
- [x] xdg-decoration + KDE server-decoration: programs draw no title bar or borders (as on Hyprland); zash's CPU-drawn frame (≈190 CPU buffers/s, the TTY slowness) gone
- [x] rounded corners (a GL shader masks each window's own surfaces; popups untouched; fullscreen square; Hyprland's rounding and rounding_power)
- [x] settings Neovim style: Hyprland's values, then ~/.config/loom/init.lua (loom.opt.x = …, loom.setup{…}, loom.hyprland.*), applied live on save (inotify), errors keep the last good ones
- [x] the user's ii-decor title bars, native: ii-decor.json + ii-decor.d read as the plugin does (exclude/include, per class, overlay pills for integrated apps), bar with icon + centred title + dots (close outermost, hover darker), buttons act on release over the same dot (close, minimize to special, maximize), drag the bar to move, double click maximizes; drawn once with cosmic-text + tiny-skia and cached; live reload of its files
- [x] ii-decor glass: `blur: true` blurs what is behind the bar, in the window's rounded box (blur.rs Over::Bar)
- [ ] ii-decor: custom buttons, button image themes, layout lists in the config (unused in the user's ii-decor.json today)
- [x] borders and shadows drawn by loom (src/frames.rs): border_size, col.active/inactive_border, decoration:shadow (range, offset, render_power, color); none fullscreen, no border on Glass apps
- [x] stats line (LOOM_STATS=1, always on a console): frames/s, render time, GPU vs CPU buffers and which program sends CPU ones
- [x] fractional scale, viewporter, primary selection; cursor-shape (named cursors from the theme on the TTY), xdg-activation, relative pointer + pointer constraints (lock/confine), keyboard-shortcuts-inhibit, single-pixel-buffer, xdg-foreign, content-type, xdg-dialog (src/protocols.rs)

## 2. Hyprland compatibility

- [x] closewindow / killwindow (address:, class:, pid:, activewindow), setfloating / settiled, resizewindowpixel; Lua window.close/kill on a named window closes it in place (no focus, no workspace switch), window.float action, window.resize; a 40-round stress (open, float, move, fullscreen, close) holds: fds level, memory flat, no zombies left
- [x] Hyprland's dispatchers as a probe of the user's binds found them missing or wrong: workspace `e±N` / `m±N` (round the existing ones, on this screen for m), `previous`, `empty`; named special workspaces (special:minimized keeps its name and id, -99 on; togglespecialworkspace NAME; monitors' specialWorkspace); clients' fullscreen 1 for maximized, workspaces' hasfullscreen, pinned, focusHistoryID from the focus history (minimize.sh restore picks the last one); fullscreenstate INTERNAL CLIENT (0 3: the program told fullscreen, the layout kept, kept over relayouts; the same again turns it off); splitratio, togglesplit (tile ↔ tilebottom, tileleft ↔ tiletop, fair ↔ fairh), swapwindow, swapnext, centerwindow, moveactive; movewindowpixel relative and with `, WINDOW`
- [x] keyboard layouts: input:kb_model / kb_layout / kb_variant / kb_options from the config and by `keyword` (a layout that does not load is refused, the old one kept); `switchxkblayout DEV next|prev|N`; `devices` names the real layouts, the active keymap and Caps/Num Lock; `activelayout` event on every change
- [x] input devices: input:, input:touchpad: and device { name = … } (and `hl.device`, `keyword device:NAME:key`) put on libinput devices: sensitivity, accel_profile, natural_scroll, scroll_factor, tap-to-click, tap-and-drag, drag_lock, disable_while_typing, clickfinger_behavior, middle_button_emulation, left_handed, enabled; `devices` lists the mice
- [x] a new window opens where it stays: laid out again at its first commit (its class known, so an app drawing its own title gets no title-bar room, and no default bar is drawn over it); a correction before it shows is a jump, not a glide; the opening waits for a picture at the size it was given (at most 250 ms). LOOM_DEBUG_ANIM=1 logs openings and moves
- [x] hyprland.lua read (sandboxed: no io/os/processes; `hl.config`, `hl.device` kept, other calls ignored), its settings over hyprland.conf, as Hyprland 0.56 uses it
- [x] live reload: any .conf / .lua / .json of the Hyprland config saved (hypr/, hyprland/, custom/, every lua/ folder) or `hyprctl reload`: binds, looks, rules, animations, title bars, input devices, keyboard and key repeat read again (saves within 150 ms are one reload); `configreloaded` to the shell; `keyword` values stay
- [x] tiled windows told so (xdg TiledLeft/Right/Top/Bottom), as Hyprland: GTK (Zen) drew its floating frame (shadow margin, rounded corners, a pale outline) inside the tile and sized itself 1574x977 instead of the tile's 1590x962
- [x] a focus grab letting go of a panel (closed) gives the keyboard back to the window (else the one under the pointer): the keys went into the closed panel when the shell had nothing to restore (70 times in one session)
- [x] touchpad gestures: an open hold/swipe/pinch ends (cancelled) on its surface before the pointer leaves it, logged; suspected cause of apps that stop scrolling until restarted. Confirm in a session log Confirmed: 12 holds ended so in the session logs (2026-10-23..29)
- [x] screens described as Hyprland does ("Make Model": the maker from the EDID's PNP code via hwdata's pnp.ids, the model from it), `desc:` matched at the start: the 180 Hz panel ran at 60 because a rule the shell saved for loom's old "TL134ADXP03 eDP-1" matched too
- [x] the Lua config's binds replace the conf's (Hyprland 0.56 binds only from hyprland.lua): hl.bind, hl.define_submap, flags (mouse/transparent/repeating/…), window.drag, window.resize() as the mouse resize, send_shortcut; 549 binds, Super+Return once (the conf had a stale one)
- [x] the shell's frequent rewrites read alone: ii-decor json → title bars, glass_*.lua → blur and rules; anything else → the whole config
- [x] Xwayland waits 5 min idle (was 15 s: 42 starts an hour from the clipboard bridge's xclip); stats a line a minute (LOOM_STATS=1: a second)
- [x] window rules (src/winrules.rs): float, tile, center, size, move, pin, no_initial_focus, workspace; floating windows kept above tiled ones; monitor dispatchers (focusmonitor, moveworkspacetomonitor, movecurrentworkspacetomonitor, swapactiveworkspaces, Lua focus{monitor} / workspace.move)

- [x] IPC request socket: monitors, workspaces, activeworkspace, clients, activewindow, devices, version, getoption, [[BATCH]] (`hyprctl -j clients` answers in loom)
- [x] dispatch (workspace, movetoworkspace(silent), exec, killactive, togglefloating, focuswindow, exit) and keyword (`hyprctl dispatch workspace 2`)
- [x] IPC event socket (socket2): openwindow, closewindow, activewindow(v2), windowtitle(v2), workspacev2, create/destroyworkspacev2, movewindowv2 (monitoradded with real outputs, M4); `layers` and `globalshortcuts` requests
- [x] hyprland-global-shortcuts-v1: Quickshell's GlobalShortcuts register, `dispatch global quickshell:NAME` fires them (binds next, with the keybind config)
- [x] hyprland-focus-grab-v1: the grab gets the keyboard, a click outside clears it (tested with a Quickshell HyprlandFocusGrab)
- [x] wlr-foreign-toplevel-management: the dock's window list (Quickshell ToplevelManager): every window incl. X ones, title, app id, active, fullscreen; activate (switches workspace), close, fullscreen
- [x] hyprland-toplevel-mapping-v1: the dock's windows linked to their Hyprland addresses (Quickshell HyprlandToplevel.wayland)
- [x] hyprland-toplevel-export-v1: window previews (Quickshell ScreencopyView live) drawn offscreen into the client's buffer; a copy waits until the window drew again and never redraws the screen (live preview of an idle window: 29 frames in 7 s, not 315)
- [x] headless --gpu: surfaceless GL on the render node, linux-dmabuf v4+ with default feedback (Quickshell dies on a v3 global), PNGs read back from the GPU
- [~] hyprland-surface-v1 (opacity drawn), hyprland-lock-notify-v1 (until the lock screen), hyprland-ctm-control-v1 (accepted; drawn with M4)
- [x] the `ii` shell runs on loom headless (scripts/run-ii.sh: bubblewrap sandbox, throwaway overlays, own pid space and D-Bus, no audio or system bus): bar, dock, desktop, widgets all up; 12 warnings, 11 from the sandbox, 1 loom's (toplevel mapping)

## 3. KDE compatibility

- [x] kde_output_device_v2 + registry (src/kde_output.rs; `kscreen-doctor -o` lists the outputs: modes, place, scale, priority, VRR capability on the TTY)
- [x] kde_output_management_v2: position, scale, mode (the CRTC on the TTY), VRR policy applied live; rotation and turning a screen off refused with a reason; System Settings › Display opens and shows the screens
- [x] kde_output_order_v1, kde_primary_output_v1; System Settings › Display shows and changes the monitors
- [ ] Night Light (org.kde.KWin.NightLight; the shell's night light and KDE's one thing)
- [ ] Mouse, Touchpad, Touchscreen (org.kde.KWin.InputDevice on loom's libinput; knurl's devices)
- [ ] Tablet (the pen: KDE's page and knurl's pen layer one setting)
- [ ] Keyboard (kxkbrc layouts, repeat; switchxkblayout the same)
- [ ] Virtual Desktops (org_kde_plasma_virtual_desktop + D-Bus = loom's workspaces)
- [ ] Window Behaviour, Window Rules (kwinrc, kwinrulesrc; org.kde.KWin window info)
- [ ] Effects, Animations, KWin Scripts, Screen Edges (what loom has of them; the rest says so)
- [ ] Window Decorations (KDecoration themes, the title bars), XWayland settings, Virtual Keyboard
- [ ] plasma-window-management, org_kde_kwin_blur, server decoration (+ palette), appmenu
- [ ] every one of the 68 pages opened on loom: works, or says plainly why not (a test that opens each)

## 4. Real hardware

- [~] udev / DRM / KMS output (DrmCompositor: vblank-paced, damage-only, planes for direct scanout and the cursor), libseat session (console switching), libinput (tap to click); mode, refresh and scale from the user's `monitor=` lines matched by name or EDID name (`desc:`); theme cursor or the program's; Ctrl+Alt+Backspace always quits, --for MINUTES; scripts/try-tty.sh (shell sandboxed: Hyprland still runs) — built, not yet run on the screen
- [x] multi-monitor (src/outputs.rs): every connected screen, placed and moded by the monitor rules (conf + the Lua hl.monitor lines), a workspace per screen as Hyprland, focus follows the pointer, layers/lock/screencopy per screen, hotplug (udev); headless `--size A,B`
- [x] VRR from the monitor rules (`vrr,0/1/2`: off, on, fullscreen only), per screen on the TTY; hyprctl monitors reports it (needs a check on the panel)
- [ ] screens on a second GPU
- [ ] 10-bit / HDR where the panel has it; the Z13 backlight cap (98%)

## 5. The rest of a day

- [x] Xwayland + window manager: X windows tiled in the layouts, on workspaces, in `hyprctl clients` with xwayland: true; dialogs float centred, menus/tooltips where the program puts them; click focus and typing reach X (xterm, xeyes, xmessage tested)
- [x] Xwayland started on demand: the X display reserved and watched, Xwayland starts with the first X program (no X program = no Xwayland, 54 MB saved)
- [x] Xwayland ends 15 s after its last X window when no X program is left (XRes count; a windowless one like xclip keeps it), starts again with the next
- [x] clipboard + middle-click selection shared between Wayland and X both ways; clipboard tools (wl-paste --watch, cliphist: wlr/ext-data-control); the focused program gets the offer
- [x] X programs sharp at 1.6: Hyprland's xwayland:force_zero_scaling (they draw at the screen's pixels), ~/.Xresources merged into each new Xwayland (Xft.dpi 224, cursor size)
- [x] drag from Wayland programs into X ones (src/xdnd.rs: loom is the XDND source; hook in vendor dnd_grab.rs; examples/dnd_source + xdnd_target; Kate on X opens a dropped file)
- [x] drag from X programs into Wayland ones (xdnd.rs: a stand-in XDND window over the X screen while a held X button is over Wayland; vendor ServerDnDGrab::detached/hover/finish; examples/xdnd_source + dnd_target)
- [ ] per-app sharp/scaled choice for X programs
- [ ] Steam and games (fullscreen, pointer lock, direct scanout) on the real screen (M4)
- [x] loom's own X server (Rust, src/bin/loom-x): a Wayland client of loom, X top-levels = loom windows; core protocol (windows, properties, atoms, selections, GCs, drawing, images, colours incl. name1-4) + core fonts (system monospace via swash, any XLFD) + input (your keymap, pointer, implicit grabs, focus); sharp at 1.6 (viewporter + fractional scale); xeyes, xev, xmessage, xterm (typing) work; started by loom for loom.opt.own_x programs, ends itself when idle
- [x] loom-x: clipboard + primary selection with Wayland (ext-data-control, TARGETS), menus/tooltips (override-redirect) as xdg popups where the program puts them, background pixmaps, core fonts by family (mono/sans/serif via fontconfig, per-character widths, sized by the scale)
- [x] loom-x extensions: BIG-REQUESTS, SHAPE (bounding shapes as transparent pixels: round xeyes), RandR 1.5 (one output, xrandr works), XFIXES (selection notices), MIT-SHM 1.2 (SysV, AttachFd, CreateSegment with fd passing: feh), RENDER 0.11 (Porter-Duff compositing, masks, glyph sets: Xft text incl. Unicode, trapezoids/triangles, gradients)
- [x] loom-x: tiles/stipples (pattern shaders), RENDER picture transforms (nearest/bilinear), ARGB visual (depth-32 windows shown with alpha), XInputExtension 2.2 (devices, XI2 device/crossing/focus/raw events), DRI3 1.2 + SYNC fences (xshmfence) + Present 1.2 (zero-copy: client GPU buffers to loom via linux-dmabuf, idle on release, completions on loom's frames) + GLX 1.4 for direct rendering (configs as Xwayland sends them): eglgears, glxgears 60 fps, vkcube over xcb; fast integer path for Xft text and solid fills; scripts/x-bench.sh compares with Xwayland (similar wall time, ~1/3 the memory), examples/loomx_check.rs checks drawing (6 checks)
- [ ] loom-x next: XKB (Qt/Chromium on X, detectable autorepeat), MIT-SHM shared pixmaps, Present flips
- [ ] own X server grows: Render, Composite, XFixes, Damage, XInput2, then DRI3 + Present + GLX (games)
- [ ] per-app setting: which X server a program gets (own one when it can, Xwayland otherwise)
- [x] wlr-screencopy (grim, the shell's region selector, Snip): whole output or a region, at the output's scale, pixel-identical to the screen
- [x] screencopy paced: copy_with_damage waits for a changed frame (recorders at the screen's rate, a still screen sends one)
- [x] screencopy into dmabufs: on by default (LOOM_SCREENCOPY_DMABUF=0 turns it off): xdg-desktop-portal-hyprland needs it, without it no stream ("Couldn't obtain a format from dma": no screen sharing in Discord or browsers). Checked headless with the whole portal path, scripts/screencast-check/run.sh (2026-10-29). wf-recorder's VA-API path stalled on it (2026-10-22): turn it off for that
- [x] ext-image-copy-capture + ext-output-image-capture-source (src/image_copy.rs, on screencopy's drawing): grim 1.5 takes it first, its shot is pixel-identical to loom's own; GPU buffers too (any modifier the renderer draws, AMD's compressed ones included; examples/copy_check.rs); a session's next copy waits for a change (written, not yet tested: grim takes one); windows (ext-foreign-toplevel sources) later
- [x] ext-session-lock (the lock screen; src/lock.rs, examples/lock_check.rs)
- [x] idle notify / inhibit, dpms on/off, the Hyprland lock notifier (src/idle.rs)
- [x] the pen (tablet-v2, src/tablet.rs; mouse fallback) and touchpad gestures (Hyprland gesture lines, pointer-gestures to programs; src/gestures.rs)
- [x] night light: hyprland-ctm-control (hyprsunset) to the CRTC's CTM property, cleared when its client goes, kept across VT switches
- [ ] input method
- [x] tiled windows resize: resizeactive (and `exact`) on a tiled window moves its workspace's master share by the pixels asked, wider for a master and narrower for one beside it, up and down for tilebottom / tiletop (wm.rs resize_tiled); fair, max and center have nothing to resize. Super+right-drag still moves mwfact sideways only
- [x] YouTube/Twitch video froze in fullscreen (sound on): frame callbacks owed per screen (one flag for all was cleared by the AOC's refresh; zen got 0 callbacks after 2 s of fullscreen). Fixed 230945b, measured on the real screen: 171-179 callbacks/s, 66-117 buffers/s. `hyprctl dispatch loomdebug stats` shows per-second counts

## 6. Looks

- [ ] extensions: KWin scripts and effects from the KDE Store, loom's own (custom animations), one place to get them
- [x] blur: Hyprland's settings and layer/window rules, Glass files, xray, ignore_alpha (src/blur.rs)
- [x] layer animations: layersIn/Out, fadeLayersIn/Out, beziers, per-layer `animation`/`no_anim` rules, closing layers kept as ghosts (src/layer_anim.rs)
- [x] window animations from the config: windowsIn/Out/Move, fadeIn/Out (popin size, speed, curve); closing windows animate out as their last frame

- [x] screen shaders (src/shader.rs): decoration:screen_shader by keyword, config or `hyprctl eval hl.config({...})` (the shell's AppDisplay); Hyprland GLSL ES 3.00 turned into 1.00; all 10 in ~/.config/hypr/shaders compile

## 7. Switch

- [x] a session entry: dist/loom.desktop, scripts/loom-session, `--session` runs exec-once (scripts/install.sh)
- [ ] a week on it, Hyprland one login away
- [x] greeter (a): the user's look snapshotted for the login screen, `loom --greeter DIR` shows it (dist/greeter: wallpaper, clock, name, password), headless (docs/greeter.md)
- [x] greeter (b): loom-auth, the PAM helper (src/bin/loom-auth.rs; mock for tests, real PAM refuses a wrong password); the greeter page asks it directly, shakes on failure; dist/pam.d/loom
- [~] greeter (c): the user's own ii lock screen as the greeter (snapshot mirrors ii; dist/greeter/ii), session_lock_xray, loom-auth starts the session (PAM session as root), --curtain fades the greeter's last picture into the shell (headless: no dark frame); the GPU handover itself needs the real seat
- [ ] greeter (d): on the real seat (the user: docs/greeter.md "For the real seat later")

## X per screen, like Hyprland (2026-10-27)
X programs saw every screen × the laptop's 1.6 (the AOC as 3072×1728 at 144 dpi): Bitwig and plugins
picked wrong scales (Terrain 2.4×), X windows were shrunk on the 1.0 screen. Hyprland shows X each
screen at its real pixels and scales each X window by its own screen. Branch `x-per-screen`.
- [x] 1. X screen layout at real pixels (src/xscreens.rs; vendor smithay: Output::set_x_geometry, X11Wm::set_x_map): xrandr shows 2560x1600+0 and 1920x1080+2560; X↔logical mapped by the screen a rectangle's middle is on
- [x] 2. per-window scale (vendor smithay: set_surface_scale over the client's): a 1.6 tile is X 2544x1539 and drawn exactly in its tile (headless snapshot)
- [x] 3. input per window: pointer, touch, tablet, gestures, relative pointer, cursor hints by the surface's scale (xev: a click at logical 500,400 in the window arrives at 800,640)
- [x] 4. a window moved to the other screen takes its scale (xterm to the AOC: 1910x1042 X pixels, 1:1)
- [ ] 5. headless checks done for positions, sizes, clicks (by hand: --size 2560x1600,1920x1080 --scale 1.6); left: menus, Bitwig plugin sizes, cursor on the real screens, then install
- [ ] Bitwig's cursor (finding 2026-10-27, `hyprctl dispatch loomdebug cursor`, lines "loom: cursor" in the session log): Bitwig's own image (surface from_x, 26×26) does arrive live, but "named default" follows at once, several times at the same pointer position with no motion; check who sends it (Xwayland's cursor-shape for the X core cursor? the per-screen scale mismatch?) and fix with stage 2/3
- [x] Bitwig's right-click menus in the right place with a window on each screen (fixed by stage 1, confirmed on the real screens 2026-10-27)
- [ ] Bitwig on the AOC: its UI drawn at the laptop's scale (one Xft.dpi 224 for all X screens, as on Hyprland; Bitwig's own per-display scale?), and a smaller old picture of the window left in its top-left corner (stale frame after the scale change? check)
- [ ] one cursor size everywhere: an X program's own cursor image (Bitwig's 26×26) is drawn at the size of the theme's arrow (Xcursor.size 48 / the desktop cursor), so switching between Bitwig's cursor and the arrow never changes its size (tty.rs cursor drawing, `cs`)
