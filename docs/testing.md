# Testing

Tools for checking loom without a screen: screenshots, frame recordings, input scripts, and
cost and speed checks.

## Screenshots

```sh
loom --headless --gpu --scale 1.6 --size 2560x1600 \
  --snapshot docs/img/zash.png --wait 3000 -- zash
```

- `--snapshot FILE.png` writes the output after `--wait` ms and exits.
- `--gpu` renders on the GPU. Without it, loom uses the CPU renderer, which draws square
  corners.
- `--scale` is the output scale. Text and window decorations scale with it.

Keep the run apart from your desktop with its own short `XDG_RUNTIME_DIR` and `--socket NAME`
(see [running](running.md)).

## Recordings

```sh
loom --headless --gpu --size 1280x800 --record out/ --snapshot out/last.png --wait 2500 \
  -- xterm -fa Monospace
```

- `--record DIR` saves every frame drawn as `frame-NNNN-TTTTTTms.png`. The number is the
  frame, the time is ms since start.
- At most 400 frames are kept.
- A frame is drawn only when something changes, so an idle window gives few frames.

To make an animation, take the delays from the timestamps. A WebP of 900 px wide:

```sh
magick $(ls out/frame-*.png) -coalesce -resize 900x -loop 0 -quality 70 out.webp
```

Equal delays are fine for a short clip. For exact timing, use `-delay` per frame from the
timestamps in the file names.

## Input

Scripted input, sent after `--input-at` ms (default 1200):

| Flag                | Does                                                   |
| ------------------- | ------------------------------------------------------ |
| `--keys TEXT`       | type keys: text, <CR> <Esc> <Up> <Down> <C-c> <wait-N> (src/input.rs) |
| `--click X,Y`       | a click at a point                                     |
| `--rclick X,Y`      | a right click there (context menus)                    |
| `--sdrag x,y,x,y`   | a Super drag from one point to another                 |
| `--sscroll X,Y,up`  | a Super scroll (`up` or `down`) at a point             |

```sh
loom --headless --size 1280x720 --click 400,300 --input-at 800 \
  --snapshot out.png -- xterm
```

## Cost

### Idle cost (`scripts/idle-cost.sh`)

```sh
scripts/idle-cost.sh [loom-binary]
```

Runs loom headless with one idle terminal (alacritty, sleeping), counts its wakeups and CPU over
10 s, and fails if there are 20 or more wakeups. The budget is no wakeups while nothing
happens. It prints one line:

```text
idle 10 s: wakeups N (N/s), cpu N ms
```

### Speed (`scripts/x-bench.sh`)

```sh
scripts/x-bench.sh [seconds-for-gl]
```

Runs the same X work on loom-x and on Xwayland, headless on the GPU, and prints wall time, CPU
of the server and of the program, and the server's peak memory. The GL part runs for 8 s by
default. Run it from the loom folder after a release build.

## loom-x checks (`examples/loomx_check.rs`)

A check of loom-x's drawing: core drawing, RENDER compositing, transforms and glyphs, read back
with GetImage. Each check prints PASS or FAIL.

```sh
# with loom-x on :26 (see x-programs.md)
DISPLAY=:26 cargo run --example loomx_check
```

## Lock screen check (`examples/lock_check.rs`)

Locks loom, shows a red lock surface, then unlocks; it prints `locked`, `configured WxH` and
`unlocked`. `LOCK_HOLD_MS` sets how long it stays locked (default 1500).

```sh
loom --headless --gpu --snapshot red.png --wait 1200 -- env LOCK_HOLD_MS=3000 target/release/examples/lock_check
```

## Logs

- `loom: …` lines on stderr: config changes, the sockets, the X servers.
- `LOOM_X_TRACE=1` for loom-x requests (see [x-programs](x-programs.md)).
- Redirect with `2>file` when a run goes wrong, then read the last lines first.

## Showing images in zash

zash shows PNG images with the Kitty graphics protocol. A short shell function sends one, in
4000-byte chunks:

```sh
showimg() {
  b=$(base64 -w0 "$1"); n=${#b}; i=0; first=1
  while [ $i -lt $n ]; do
    chunk=$(printf '%s' "$b" | cut -c$((i+1))-$((i+4000)))
    i=$((i+4000)); more=1; [ $i -ge $n ] && more=0
    if [ $first = 1 ]; then printf '\033_Ga=T,f=100,c=40,r=12,m=%s;%s\033\\' $more "$chunk"; first=0
    else printf '\033_Gm=%s;%s\033\\' $more "$chunk"; fi
  done
  echo
}
showimg docs/img/xeyes-shape.png
```

The image takes 40 columns by 12 rows. Tested in zash under loom headless: the picture shows at
the cursor. Animated WebP does not play in zash (it accepts stills only), so for animations
see the WebP in a browser or on GitHub.

See also: [running](running.md), [settings](settings.md).

## Debug variables

| Variable | What loom then prints |
| --- | --- |
| `LOOM_STATS=1` | the stats line every second instead of every minute (all counts per second) |
| `LOOM_DEBUG_POPUP=1` | each menu (xdg popup): made, grabbed, closed by a press outside, destroyed |
| `LOOM_DEBUG_ANIM=1` | window animations as they start and settle |

`scripts/ipc-probe.sh` starts a headless loom with programs on it and sends Hyprland IPC
requests in order (focus tests: `dispatch workspace 2`, then `j/activewindow`).
