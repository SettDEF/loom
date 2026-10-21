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
- [ ] mouse binds (bindm move/resize, mouse:N, scroll); focuswindow by class:/title:
- [ ] xdg-decoration (server side), borders, gaps
- [ ] fractional scale, viewporter, cursor shape, primary selection, xdg-activation

## 2. Hyprland compatibility

- [x] IPC request socket: monitors, workspaces, activeworkspace, clients, activewindow, devices, version, getoption, [[BATCH]] (`hyprctl -j clients` answers in loom)
- [x] dispatch (workspace, movetoworkspace(silent), exec, killactive, togglefloating, focuswindow, exit) and keyword (`hyprctl dispatch workspace 2`)
- [x] IPC event socket (socket2): openwindow, closewindow, activewindow(v2), windowtitle(v2), workspacev2, create/destroyworkspacev2, movewindowv2 (monitoradded with real outputs, M4); `layers` and `globalshortcuts` requests
- [x] hyprland-global-shortcuts-v1: Quickshell's GlobalShortcuts register, `dispatch global quickshell:NAME` fires them (binds next, with the keybind config)
- [x] hyprland-focus-grab-v1: the grab gets the keyboard, a click outside clears it (tested with a Quickshell HyprlandFocusGrab)
- [ ] hyprland-toplevel-export-v1, hyprland-toplevel-mapping-v1 (window previews)
- [~] hyprland-surface-v1 (opacity drawn), hyprland-lock-notify-v1 (until the lock screen), hyprland-ctm-control-v1 (accepted; drawn with M4)
- [ ] the `ii` shell runs on loom headless with a clean log

## 3. KDE compatibility

- [ ] kde_output_device_v2 + registry (`kscreen-doctor -o` lists the outputs)
- [ ] kde_output_management_v2 (`kscreen-doctor output.1.scale.1.5` changes it)
- [ ] kde_output_order_v1, kde_primary_output_v1; System Settings › Display shows and changes the monitors
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

- [ ] udev / DRM / KMS output, libseat session, libinput
- [ ] multi-monitor, hotplug, VRR, DPMS
- [ ] 10-bit / HDR where the panel has it; the Z13 backlight cap (98%)

## 5. The rest of a day

- [x] Xwayland + window manager: X windows tiled in the layouts, on workspaces, in `hyprctl clients` with xwayland: true; dialogs float centred, menus/tooltips where the program puts them; click focus and typing reach X (xterm, xeyes, xmessage tested)
- [ ] Xwayland started on demand (on the first X connection), not with loom
- [ ] X clipboard and drag and drop shared with Wayland; HiDPI per app (sharp or scaled)
- [ ] Steam and games (fullscreen, pointer lock, direct scanout) on the real screen (M4)
- [ ] loom's own X server (Rust): core protocol + MIT-SHM + XKB + RandR; xterm, xeyes, xclock run without Xwayland
- [ ] own X server grows: Render, Composite, XFixes, Damage, XInput2, then DRI3 + Present + GLX (games)
- [ ] per-app setting: which X server a program gets (own one when it can, Xwayland otherwise)
- [ ] screencopy (wlr-screencopy, ext-image-copy-capture): portals, OBS, Snip
- [ ] ext-session-lock (the lock screen), idle notify / inhibit
- [ ] input method, tablet v2 (the pen), gestures, gamma

## 6. Looks

- [ ] extensions: KWin scripts and effects from the KDE Store, loom's own (custom animations), one place to get them
- [ ] rounding, shadows, blur, animations
- [ ] screen shaders (the shell's AppDisplay effects), the title bars ii-decor draws today

## 7. Switch

- [ ] a session entry; the user's config (the .conf source) read
- [ ] a week on it, Hyprland one login away
