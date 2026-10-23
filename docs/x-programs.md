# X programs

loom runs X programs (xterm, xeyes, eglgears_x11 and the like) on one of two X servers.

| Server   | Written in           | Started                  | Ends                         |
| -------- | -------------------- | ------------------------ | ---------------------------- |
| Xwayland | Xorg (system)        | first X program          | 15 s after the last X window |
| loom-x   | Rust, src/bin/loom-x | first `own_x` program    | 30 s after its last client   |

loom-x's 30 s is `LOOM_X_IDLE`.

## Xwayland

The default. loom starts it when an X program runs.

- Clipboard and middle-click selection are shared with Wayland both ways.
- With Hyprland's `xwayland:force_zero_scaling`, X programs draw at the screen's own pixels, so
  text stays sharp at a fractional scale.
- `~/.Xresources` (`Xft.dpi`) is loaded into it.

## loom-x

loom's own X server. Each X top-level window is a Wayland window of loom: tiled, decorated and
focused like any other window. It draws at the screen's pixels.

Use it for the programs named in `own_x`:

```lua
loom.opt.own_x = { "xterm", "xeyes" }
```

A program is matched by its file name. Anything not listed goes to Xwayland.

Run it alone, in any Wayland session:

```sh
loom-x :20 2560x1600 --scale 1.6
DISPLAY=:20 xterm
```

The arguments are the display number, the size in the output's own pixels, and the scale (a
logical pixel is that many physical pixels). `--scale` must be 0.5 or more.

### What it has

- The core protocol: windows, properties, selections, drawing, images, core fonts from the
  system fonts, input with your keymap.
- The clipboard, shared with Wayland.
- Menus as Wayland popups.
- Extensions (the table `QueryExtension` answers from, src/bin/loom-x/ext.rs):

| Extension        | What it is for                                            |
| ---------------- | --------------------------------------------------------- |
| BIG-REQUESTS     | requests longer than 256 KB                               |
| SHAPE            | non-rectangular windows (shaped pixels are transparent)   |
| RANDR            | the screen as one output                                  |
| XFIXES           | selection change notices                                  |
| MIT-SHM          | shared-memory images                                      |
| RENDER           | compositing and anti-aliased text (Xft)                   |
| XInputExtension  | input devices                                             |
| DRI3             | GPU buffers from clients                                  |
| SYNC             | frame sync                                                |
| Present          | presenting pixmaps to a window (waits for frames)         |
| GLX              | OpenGL with direct rendering (glxgears, older GL apps)    |

GPU programs (EGL, Vulkan, GLX) draw on the client side; a full-window frame goes to loom as
it is (zero-copy, linux-dmabuf), so `glxgears`, `eglgears_x11` and `vkcube --wsi xcb` run at
the screen's 60 fps for a few tens of milliseconds of server CPU in ten seconds.

Performance against Xwayland: `scripts/x-bench.sh` (medians of 3 rounds, headless, same GPU).
loom-x is as fast or faster in wall time (text: 176 against 218 ms core, 84 against 116 ms Xft)
with about a third of the memory (25-30 MB against 80 MB); its server uses more CPU for text,
most of it hinting a core font's 256 glyphs when the font is opened (spread over threads).

Start times: `scripts/x-start.sh`. loom-x answers 10-15 ms after it starts (Xwayland about
100 ms). A program's window is viewable within roughly what Xwayland takes (xeyes, xterm, Tk:
15-65 ms); fonts are looked up through fontconfig on first use, the common ones in the
background at start.

Check which extensions a server reports with `xdpyinfo` (`xdpyinfo -display :20 | grep -A12
extensions`).

### Picture

```text
 +--------------------------------------------+
 | loom (Wayland compositor)                  |
 |   +----------+  +------------------------+ |
 |   | xterm    |  | eglgears               | |
 |   | (loom-x) |  | (loom-x)               | |
 |   +----------+  +------------------------+ |
 |   shell, dock, zash (Wayland)              |
 +--------------------------------------------+
   each loom-x window is one Wayland window
```

![xterm on loom](img/xterm.png)

![eglgears on loom-x](img/loom-x-eglgears.png)

## Environment variables

| Variable         | Default | Effect                                                           |
| ---------------- | ------- | ---------------------------------------------------------------- |
| `LOOM_X_IDLE`    | 30      | seconds loom-x waits, with no client, before it exits            |
| `LOOM_X_TRACE`   | unset   | any value: prints each request and close on stderr (debugging)   |

```sh
LOOM_X_IDLE=5 loom-x :20 1280x720
LOOM_X_TRACE=1 loom-x :20 1280x720 2>trace.log
```

Both are read by loom-x itself, so they apply when it starts.

## Troubleshooting

- A program in `own_x` got Xwayland: loom-x did not start. loom logs `loom-x did not start` with
  the reason, and the program falls back to Xwayland.
- loom-x exits in the middle of a session: check `LOOM_X_TRACE` output for the request that
  failed, then the `Io error` line on stderr.

See also: [settings](settings.md), [testing](testing.md) (`scripts/x-bench.sh` compares the two
servers, and `examples/loomx_check.rs` checks drawing).
