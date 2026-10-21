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
- [~] workspaces (any number, switched and moved to over IPC); keybinds from a config file next
- [ ] xdg-decoration (server side), borders, gaps
- [ ] fractional scale, viewporter, cursor shape, primary selection, xdg-activation

## 2. Hyprland compatibility

- [x] IPC request socket: monitors, workspaces, activeworkspace, clients, activewindow, devices, version, getoption, [[BATCH]] (`hyprctl -j clients` answers in loom)
- [x] dispatch (workspace, movetoworkspace(silent), exec, killactive, togglefloating, focuswindow, exit) and keyword (`hyprctl dispatch workspace 2`)
- [~] IPC event socket (socket2): openwindow, closewindow, activewindow(v2), workspacev2, create/destroyworkspacev2, movewindowv2; windowtitlev2, monitoraddedv2 next
- [ ] hyprland-global-shortcuts-v1 (the shell's 127 GlobalShortcuts)
- [ ] hyprland-focus-grab-v1 (launcher, sidebars)
- [ ] hyprland-toplevel-export-v1, hyprland-toplevel-mapping-v1 (window previews)
- [ ] hyprland-ctm-control-v1, hyprland-surface-v1, hyprland-lock-notify-v1
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

- [ ] XWayland (Steam, games)
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
