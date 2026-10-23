# Effects

Three things change how windows look: rounded corners, loom's own animations, and KDE's window
effects.

## Rounded corners

`rounding` sets the corner radius in logical pixels. `rounding_power` sets the curve: 2 is a
circle, higher values are squarer.

```lua
loom.setup { rounding = 12, rounding_power = 2.4 }
```

- The corners are anti-aliased.
- Rounding needs the GPU renderer (`--gpu` in headless runs). The CPU renderer (pixman, used
  by default for tests) draws square corners.
- A fullscreen window is not rounded, so it can go straight to the screen.

![xeyes with shaped window on loom](img/xeyes-shape.png)

## Animations

`animations` picks the set of window animations.

| Value    | What runs                                                        |
| -------- | ---------------------------------------------------------------- |
| `"loom"` | loom's own (the default), following Hyprland's bezier curves     |
| `"kwin"` | KDE's KWin scripted effects, run unchanged                       |

### loom's own

- A window opening fades in and grows, for 280 ms.
- A window moving to a new place glides there, for 320 ms.
- A retarget (a window moved again mid-animation) carries on from where it is.

The curves are cubic beziers, written the way Hyprland's config writes them.

### KDE's effects (`"kwin"`)

loom runs KWin's own scripted effects, the JavaScript in `/usr/share/kwin/effects/*` and
`~/.local/share/kwin/effects` (KDE Store ones too). Each effect's animate() and set() calls
become loom's native animations (scale, opacity, translation, size, position), using Qt's
easing curves.

Which effects run: the ones KDE enables by default (their `EnabledByDefault` metadata), changed
by `[Plugins] <name>Enabled` in `~/.config/kwinrc`. loom logs the list at start:

```text
loom: kwin effects: dialogparent, dimscreen, fadingpopups, frozenapp, fullscreen, maximize,
  scale, squash, windowaperture
```

A closing window is drawn from a snapshot of its last frame while its close animation runs.

![a window opening with kwin animations](img/xterm-open-kwin.webp)

The animation above is a headless recording of an xterm opening with `"kwin"`, made with
`--record` (see [testing](testing.md)).

## Choosing

- `"loom"` runs only loom's own animations: the window opens and moves.
- `"kwin"` brings KDE's effects (scale, squash, popup fades, dim behind dialogs). The effects
  run in a JavaScript engine (QuickJS), so check `scripts/idle-cost.sh` after turning it on.

See also: [settings](settings.md).
