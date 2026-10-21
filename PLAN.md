# zeal — plan

A Wayland compositor for this desktop (the `ii` shell, zash, zeit, zett), in Rust on Smithay. It
speaks **Hyprland's** protocols and IPC, so the shell runs unchanged, and **KDE's**, so KDE's
tools (System Settings › Display, kscreen-doctor, the window list, blur hints) work too.

Why its own: Hyprland only speaks Hyprland's, KWin only KDE's; a plugin would have to follow
Hyprland's internals on every release. zeal is built for both from the start.

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

## Rules

- Never replace the running session while testing: every milestone is tested **headless**
  (zeal's own off-screen backend, a picture of what it shows) or nested, never on the TTY until
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
   hyprland-lock-notify. Test: the `ii` shell runs on zeal headless, its log clean.
3. **KDE compatibility** — kde_output_device_v2 (registry), kde_output_management_v2,
   kde_output_order_v1, kde_primary_output_v1: kscreen-doctor and System Settings › Display
   read and change the monitors. Then plasma-window-management, org_kde_kwin_blur,
   org_kde_kwin_server_decoration(_palette), appmenu, kde-screen-edge.
4. **Real hardware** — udev/DRM/KMS, libseat session, libinput, multi-monitor, hotplug, VRR,
   10-bit and HDR where the panel has it, DPMS, the Z13 panel's backlight cap (98%).
5. **The rest of a day** — XWayland (games, Steam), screencopy (wlr-screencopy,
   ext-image-copy-capture) for portals, OBS and the shell's Snip, ext-session-lock (the lock
   screen), idle notify and inhibit, input method, tablet v2 (the pen), gestures, gamma.
6. **Looks** — rounding, borders, shadows, blur, animations, screen shaders (the shell's
   AppDisplay effects), the title bars ii-decor draws today.
7. **Switch** — a session entry, the user's config read (the .conf source), a week on it
   with Hyprland one login away.

## Now

Milestone 0.
