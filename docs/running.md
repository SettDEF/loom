# Running loom

loom has three backends. Pick one with a flag.

| Flag         | Where it runs                 | Use it for                         |
| ------------ | ----------------------------- | ---------------------------------- |
| `--headless` | no screen, output in memory   | tests, screenshots, recordings     |
| `--nested`   | a window on your desktop      | trying loom with the real shell    |
| `--tty`      | the real screen, from a console | running loom as the session      |

Headless uses the CPU renderer (pixman) unless `--gpu` is given. Nested and TTY use the GPU.

## Headless

```sh
loom --headless --size 1280x720 -- alacritty
```

- `--size WxH` sets the output size in pixels.
- `--scale F` sets the output scale.
- `--snapshot out.png` saves what is shown after `--wait` ms (default 2000) and exits.
- `-- CMD …` runs a program on loom. Everything after `--` is the program.

For a headless run that keeps your own desktop untouched, give it its own socket name and a
short runtime directory:

```sh
XDG_RUNTIME_DIR=$DIR/rt loom --headless --gpu --socket lm1 --wait 3000 \
  --snapshot out.png -- xeyes -shape
```

The runtime directory must be short. loom puts its own IPC socket under it, and Unix socket
paths are limited to about 107 bytes.

For the full flag list, see [testing](testing.md).

## Nested

```sh
scripts/try-nested.sh
```

loom opens as a window on your running desktop. Your desktop's Wayland socket is the only one
it can reach, and the shell inside it runs sandboxed (see below). Close the window to end it.

## TTY

Run loom on the real screen, from a text console (Ctrl+Alt+F3, log in, then):

```sh
scripts/try-tty.sh 10
```

- The argument is minutes. loom quits by itself after that (default 10), a safety net for the
  first tries.
- Ctrl+Alt+Backspace quits loom, except while the screen is locked.
- Ctrl+Alt+F1 (or F2) goes back to the other session.
- Everything loom says goes to `~/.local/state/loom/tty-<time>.log`, so a black screen still
  leaves its reason on disk.

Inside, the `ii` shell runs sandboxed and zash opens.

## Login session

loom as the whole session, picked on the login screen (GDM), like Hyprland:

```sh
scripts/install.sh      # loom, loom-x, loom-session into ~/.local/bin
# install.sh also puts the session entry (with your home) in the login screen's list: sudo asks once
```

Then log out and pick **loom** under the gear on the login screen. `loom-session` runs
`loom --tty --session`:

- The Hyprland config's `exec-once` and `exec` lines start, in order, with its `$variables`
  (the shell, hypridle, the keyring, the portals' environment …).
- `XDG_CURRENT_DESKTOP=Hyprland`, so the portals and the shell take it for Hyprland.
- The log goes to `~/.local/state/loom/session-<time>.log`.
- Ctrl+Alt+Backspace ends the session (back to the login screen), except while locked.
- Starting loom on the screen ends any older loom session of yours on another console first,
  so restarting is just starting it again.

If loom fails to start, the login screen comes back; pick Hyprland there. After a rebuild, run
`scripts/install.sh` again; a running session keeps its old binary.

## Several screens

Every connected screen is used, as Hyprland's monitor rules place it: `monitor = NAME, MODE, POS,
SCALE` lines and the `hl.monitor({ … })` lines in `~/.config/hypr/lua/` (the shell's
MonitorAutoMode writes those). The last rule that matches wins; `desc:` matches the screen's EDID
name. A screen without a position goes right of the others.

- Each screen shows its own workspace. A workspace belongs to the screen it was first shown on:
  `workspace N` for one shown elsewhere moves the focus (and the pointer) there.
- The focus follows the pointer from screen to screen.
- Plugging a screen in or out is picked up at once; an unplugged screen's workspaces move to
  the focused one.
- Headless: `--size 1600x1000,1920x1080` makes two screens side by side (only the first is
  shot; `grim -o HEADLESS-2` shows the second).

## KDE Display settings

System Settings › Display and `kscreen-doctor` work on loom (kde_output_device_v2 and
kde_output_management_v2): they list the screens and change their place, scale, mode and
adaptive sync policy (KDE's never/always/automatic are `vrr,0/1/2`). Rotating a screen or
turning it off is refused for now. `kscreen-doctor output.eDP-1.scale.1.5`, say.

## Adaptive sync (VRR)

As Hyprland: `vrr,N` on a `monitor =` line (or `vrr` in an `hl.monitor` table): 0 off, 1 on,
2 only while a window is fullscreen on that screen. loom logs `adaptive sync on/off` when it
changes, and `hyprctl monitors -j` shows `vrr`. TTY only (nested and headless have no screen).

## Screen shader

`decoration:screen_shader = /path/to.frag` (config, `hyprctl keyword`, or `hyprctl eval
'hl.config({ decoration = { screen_shader = "…" } })'` as the shell's display grading sends it):
every screen drawn through that Hyprland shader (GLSL ES 3.00, `v_texcoord`, `tex`, `fragColor`).
An empty value turns it off. A shader that does not compile is logged once and left out.

## Pen and touchpad

- **Pen** (tablet-v2): programs that know tablets get the pen with pressure, tilt, distance,
  rotation and its buttons. It draws on the built-in panel (eDP); the cursor goes with it. A
  program that does not know tablets gets the pen as a mouse: the tip is the left button, the
  side buttons right and middle.
- **Gestures**, from the Hyprland config: `gesture = FINGERS, DIRECTION, ACTION[, ARGS]`
  (directions swipe, horizontal, vertical, up, down, left, right, pinch, pinchin, pinchout;
  actions move, float, workspace, close, fullscreen, dispatcher) and
  `gestures { workspace_swipe_distance, _cancel_ratio, _direction_lock_threshold, _create_new,
  _invert }`. A gesture no line takes goes to the program under the pointer (a browser's pinch
  zoom).

## Window rules

Hyprland's `windowrule = match:class RE, match:title RE, EFFECT` lines (and the older
`windowrulev2 = EFFECT, class:RE, title:RE`) apply when a window first shows:

| Effect | What it does |
| --- | --- |
| `float on` / `tile on` | floating or tiled |
| `center on` | centred on the screen (a floating window is centred anyway) |
| `size W H` | its size: numbers, `60%`, or `(monitor_w*.60)` style expressions |
| `move X Y` | its place, the same forms |
| `pin on` | on every workspace (floating) |
| `no_initial_focus on` | the window that had the keyboard keeps it |
| `workspace N` | opens on workspace N |

`no_blur` is read by the blur (see settings). A rule with a matcher loom does not read (xwayland,
float …) or a regex with lookaround (`(?!…)`) is skipped. Floating windows stay above the tiled
ones.

## Lock screen

The shell's lock (and any `ext-session-lock` locker, such as swaylock) locks loom:

- Only the lock screen is drawn, and it alone gets the keyboard and the pointer.
- No bind runs, and Ctrl+Alt+Backspace does not quit.
- Unlocking gives the keyboard back to the window that had it.
- If the locker crashes, the screen stays locked and dark. Start the lock again (from another
  console: `qs ipc call lock activate` with loom's `WAYLAND_DISPLAY`) and unlock as usual.
- Ctrl+Alt+F1 still switches consoles.

## Idle and screen off

- `ext-idle-notify`: hypridle (and anything like it) learns when you are away. Every key, pointer
  move, click and scroll counts as activity.
- Idle inhibit: a playing video (mpv, browsers) or the shell's caffeine toggle keeps it from going
  idle. loom logs `loom: idle inhibited` / `allowed`.
- `hyprctl dispatch dpms off|on|toggle` (and the Lua form `hl.dsp.dpms("off")`) switches the
  screen off on the TTY: nothing is drawn and the panel goes dark. A key press switches it on
  again, even without hypridle's `on-resume`.
- hypridle's `on_lock_cmd` / `on_unlock_cmd` run when the screen locks and unlocks (Hyprland's
  lock notifier).

## Stress check

Windows opened, floated, moved, fullscreened and closed for 40 rounds against the real shell,
loom's memory, fds, children and zombies sampled every 5:

```sh
LOOM_BIND=$DIR LOOM_GPU=1 LOOM_EXTRA="sleep 10; $PWD/scripts/stress.sh $DIR/out.txt 40; sleep 600" scripts/run-ii.sh 170 $DIR/end.png
```

## Drag and drop with X programs

Files dragged from a Wayland program (Dolphin, the browser) drop into X ones (Bitwig, Wine):
over an X window loom carries the drag itself (XDND, `src/xdnd.rs`). The other way too (a bounce
from Bitwig into Dolphin): while a button held on an X window is over a Wayland one, loom puts a
see-through drop target over the X screen and carries the X program's drag on. Headless checks:

```sh
loom --headless --drag 400,150,400,460 -- sh -c 'dnd_source & sleep 1.5; xdnd_target'
loom --headless --drag 400,150,400,460 -- sh -c 'xdnd_source & sleep 1.5; dnd_target'
```

(`cargo build --release --examples`; it prints `dropped "file:///…"` and `finished`.)

## Scripts

Each script starts with a comment that says what it does and how to call it. Run `head -20
scripts/NAME` to read it.

- `run-ii.sh [seconds] [out.png] [loom]`: the `ii` shell on loom, headless, in a bubblewrap
  sandbox. No real Hyprland, no system bus, throwaway config overlays.
- `try-nested.sh [loom]`: loom in a window on the desktop, with the sandboxed shell.
- `try-tty.sh [minutes] [loom]`: loom on a console, with the sandboxed shell and zash.
- `sandboxed-shell.sh`: the shell for loom's Wayland display. Run by loom, not by hand.
- `shot-tty.sh [out.png]`: a grim screenshot of loom running on a console, from elsewhere.
- `idle-cost.sh [loom]`: wakeups and CPU over 10 s with one idle terminal (see testing).
- `x-bench.sh [gl-seconds]`: loom-x against Xwayland on the same work (see testing).

The scripts default to the loom binary at `/mnt/storage/dev-cache/targets/release/loom`. Pass
another path as the last argument to use a different build.

## Quick reference

```sh
# a release build
cargo build --release

# a headless window, with zash, as a picture
loom --headless --gpu --size 1280x800 --snapshot zash.png --wait 3000 -- zash

# the ii shell, headless, for 25 s
scripts/run-ii.sh 25 ii.png
```

See also: [settings](settings.md), [x-programs](x-programs.md), [testing](testing.md).

## Night light

hyprsunset (the shell's night light) works as on Hyprland: its colour matrix goes to the screen's
CTM (the display hardware applies it, no cost per frame). When hyprsunset quits, the colours go
back. A screen loom takes over starts with no matrix, so a night light the other session left on
is cleared; after switching back to loom's console its matrices are set again.

## Input devices and the Lua config

When `~/.config/hypr/hyprland.lua` exists (Hyprland 0.56+), loom runs it sandboxed: no files
written, no programs started, no `os` but `getenv` and `time`. It keeps what `hl.config` and
`hl.device` set, over what `hyprland.conf` says. On the TTY each libinput device gets its
`input:` / `input:touchpad:` / `device` settings when it appears and again on a `keyword` or
`hl.device` change; `enabled = false` turns a device off. `hyprctl devices -j` lists the mice
with their scroll factor.
