# loom — plan

A Wayland compositor for this desktop (the `ii` shell, zash, zeit, zett), in Rust on Smithay. It
speaks **Hyprland's** protocols and IPC, so the shell runs unchanged, and **KDE's**, so KDE's
tools (System Settings › Display, kscreen-doctor, the window list, blur hints) work too.

Why its own: Hyprland only speaks Hyprland's, KWin only KDE's; a plugin would have to follow
Hyprland's internals on every release. loom is built for both from the start.

## What the desktop needs today (2026-10-21 survey)

- 116 shell files use Quickshell's Hyprland module: `GlobalShortcut` (127 uses), the focused
  monitor and workspace, `HyprlandToplevel`, `HyprlandFocusGrab`, `Hyprland.dispatch`.
  That is: Hyprland's IPC sockets (`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`
  `.socket.sock` requests, `.socket2.sock` events) and the hyprland-* protocols.
- 79 `hyprctl` calls in the shell and ~/.scripts: monitors, dispatch, keyword, getoption,
  hyprsunset, eval, reload, switchxkblayout, devices.
- The config: hyprland.conf / hyprland.lua (generated from the .conf source), monitors.conf,
  workspaces.conf, window rules, the AwesomeWM layouts (lua:awesomewm), ii-decor title bars,
  screen shaders (hypr/shaders, the shell's AppDisplay), blur/rounding/shadows/animations.

## Goals the user set (2026-10-21)

- **Faster**: every frame drawn only when something changed (damage), the GPU renderer with direct
  scanout for full-screen windows, measured frame times (a budget, checked by a test), never a
  frame late because of loom.
- **Animations** from the start: windows opening, closing, moving and resizing, workspaces
  sliding, layers fading; curves as Hyprland's config writes them (bezier) and KDE's.
- **Docks**: layer-shell, foreign-toplevel (wlr and ext) and plasma-window-management, so the
  shell's dock, KDE's and any other one work.
- **Extensions later, from the KDE Store**: KWin scripts and effects from store.kde.org run on
  loom as they do on KWin (its scripting API), plus loom's own (custom animations, effects);
  install, update and remove from one place, and they simply work.
- **X windows natively**: X programs are ordinary windows (sharp at any scale, clipboard and
  drag and drop shared, games and Steam fine). Two X servers (the user, 2026-10-21): Xwayland
  started on demand first, so everything runs soon; then loom's own X server in Rust (the
  `xserver` module, drawing X windows itself, no Xwayland), starting with simple programs (core
  protocol, MIT-SHM, XKB, RandR) and growing; a per-app setting says which server a program
  gets, and games stay on Xwayland until loom's covers DRI3, GLX, Present and XInput2.
- **Both desktops perfectly**: everything the shell expects of Hyprland, everything KDE's
  programs expect of KWin.

## Rules

- **Performance first** (the user, 2026-10-21): the hot paths (every key, pointer motion, every
  frame) do a lookup at most and allocate nothing in the common case; config is parsed once
  into tables; anything new on a hot path is measured, not assumed.
- **Battery**: an idle desktop wakes loom 0 times a second (frames are event driven, paced to the
  refresh, never a ticking timer); `scripts/idle-cost.sh` checks it. Hidden workspaces get no
  frame callbacks, so their programs stop drawing. Only damaged regions go to the screen.

- Never replace the running session while testing: every milestone is tested **headless**
  (loom's own off-screen backend, a picture of what it shows) or nested, never on the TTY until
  the user starts it themselves.
- Each protocol has a test that a real client passes (wayland-info, kscreen-doctor, qs, zash,
  alacritty), not only unit tests.
- Hyprland's behaviour is the reference where the shell depends on it; KDE's where KDE tools do.

## Milestones

0. **Skeleton** — Smithay, a headless backend (pixman, renders to memory, `--snapshot out.png`),
   a nested backend (winit, for looking). wl_compositor, wl_shm, xdg-shell, seat (keyboard,
   pointer), one window shown. Test: alacritty / zash open in it, the picture shows them.
1. **Windows** — tiling (dwindle, as Hyprland's default) and floating, focus, workspaces,
   keybinds from a config, xdg-decoration (server side), layer-shell (the shell's bar, dock,
   panels), popups, fractional scale, cursor shape, data device (clipboard), primary selection.
2. **Hyprland compatibility** — the IPC sockets (j/monitors, workspaces, clients, activewindow,
   dispatch, keyword, getoption, devices; socket2 events), hyprland-global-shortcuts,
   hyprland-focus-grab, hyprland-toplevel-export, hyprland-ctm-control, hyprland-surface,
   hyprland-lock-notify. Test: the `ii` shell runs on loom headless, its log clean.
3. **KDE compatibility** — every page of KDE's System Settings works on loom (the user, 2026-10-21).
   The Wayland side: kde_output_device_v2 (registry), kde_output_management_v2,
   kde_output_order_v1, kde_primary_output_v1 (Display), plasma-window-management,
   org_kde_kwin_blur, server decoration (+ palette), appmenu, kde-screen-edge,
   org_kde_plasma_virtual_desktop. The D-Bus side, as KWin answers it (survey of the 68 pages):
   `org.kde.KWin` (reconfigure, window info for Window Rules), `org.kde.KWin.InputDevice(Manager)`
   (Mouse, Touchpad, Touchscreen, Tablet), `org.kde.KWin.NightLight` (Night Light),
   `org.kde.kwin.Effects` and `org.kde.kwin.Scripting` (Effects, KWin Scripts),
   `org.kde.KWin.VirtualDesktopManager` (Virtual Desktops), `org.kde.kwin.VirtualKeyboard`,
   decorations (KDecoration themes drawn by loom), XWayland settings; and KWin's files the
   pages write (kwinrc, kxkbrc keyboard layouts, kcminputrc, kwinrulesrc), read again on
   reconfigure.
   **Hardware goes through knurl**: loom owns the libinput settings (acceleration, scrolling,
   tap, left-handed) because it owns the devices; knurl (the user's hardware app, `knurl serve`)
   stays the place for what devices are (names, batteries, DPI, backlight with its 98% cap,
   gestures, the pen layer). KDE's pages, knurl's window and the shell all read and write the
   same settings through loom, and loom asks knurl for what is knurl's.
4. **Real hardware** — udev/DRM/KMS, libseat session, libinput, multi-monitor, hotplug, VRR,
   10-bit and HDR where the panel has it, DPMS, the Z13 panel's backlight cap (98%).
5. **The rest of a day** — XWayland (games, Steam), screencopy (wlr-screencopy,
   ext-image-copy-capture) for portals, OBS and the shell's Snip, ext-session-lock (the lock
   screen), idle notify and inhibit, input method, tablet v2 (the pen), gestures, gamma.
6. **Looks** — rounding, borders, shadows, blur, animations, screen shaders (the shell's
   AppDisplay effects), the title bars ii-decor draws today.
- **Switching fast** (the user, 2026-10-21): loom and Hyprland side by side on two TTYs (Ctrl+Alt+F-key between them) while trying it; later a live swap: loom takes over the Wayland socket name and Qt programs (the shell, KDE apps, zett) reconnect without restarting (QT_WAYLAND_RECONNECT), the same machinery as restarting loom itself without losing windows.
7. **Switch** — a session entry, the user's config read (the .conf source), a week on it
   with Hyprland one login away.

## Now

Milestone 0.
