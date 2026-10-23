# Settings

loom reads `~/.config/loom/init.lua` (or `$XDG_CONFIG_HOME/loom/init.lua`). Saved changes apply
at once. An error keeps the last good settings and is logged.

loom starts from your Hyprland config (`~/.config/hypr/hyprland.conf`), so the look matches
Hyprland until init.lua changes it.

## Options

| Option         | Default  | Meaning                                                          |
| -------------- | -------- | ---------------------------------------------------------------- |
| `rounding`     | 0        | corner radius, logical px (Hyprland `decoration:rounding`)       |
| `rounding_power` | 2      | corner curve: 2 = circle, higher = squarer (1 to 10)             |
| `gaps_in`      | 5        | gap between two windows (Hyprland `general:gaps_in`)             |
| `gaps_out`     | 20       | gap to the screen's edge (Hyprland `general:gaps_out`)           |
| `animations`   | `"loom"` | `"loom"` (own animations) or `"kwin"` (KDE's window effects)     |
| `own_x`        | `{}`     | program names that get loom's own X server (see x-programs.md)   |

The defaults above apply when neither init.lua nor the Hyprland config sets the option.

## Examples

Set one option, like `vim.opt`:

```lua
loom.opt.rounding = 12
```

Several at once:

```lua
loom.setup {
  rounding = 12,
  rounding_power = 2.4,
  gaps_in = 4,
  gaps_out = 8,
}
```

Use the Hyprland value, read only:

```lua
if loom.hyprland.rounding > 10 then
  loom.opt.rounding = 10
end
```

`loom.hyprland` holds the values from the Hyprland config (`rounding`, `rounding_power`,
`gaps_in`, `gaps_out`). Writing to it does nothing useful; set `loom.opt` instead.

KDE's window effects:

```lua
loom.opt.animations = "kwin"
```

X programs with loom's own X server:

```lua
loom.opt.own_x = { "xterm", "xeyes" }
```

## Blur

loom blurs what is behind layers and windows the way Hyprland does, from the same config. Nothing
in `init.lua` is needed.

| Where it comes from                         | What it sets                                         |
| ------------------------------------------- | ---------------------------------------------------- |
| `decoration { blur { … } }`                 | `enabled`, `size`, `passes`, `noise`, `contrast`, `brightness`, `vibrancy`, `xray` |
| `layerrule = match:namespace RE, blur on`   | which layers (the shell's bar, dock, sidebars …)     |
| `layerrule = …, ignore_alpha 0.79`          | blur only where the layer is more opaque than that   |
| `windowrule = match:class RE, no_blur on`   | which windows (Hyprland blurs all unless told not to) |
| `~/.config/hypr/lua/glass_blur.lua`         | the shell's Glass blur values, over the ones above   |
| `~/.config/hypr/lua/glass_rules.lua`        | the shell's Glass apps (`no_blur = false`)           |

The older rule forms (`layerrule = blur, NAME`, `ignorealpha 0.5, NAME`, `ignorezero`) work too.
A changed file is read again at once.

With `xray` on, a blurred surface shows the blurred wallpaper layers, not the windows between.
The blur is made once per set of things behind and redone only when one of them changes. Blur
needs the GPU renderer: headless without `--gpu` draws no blur.

| Variable          | Effect                                                   |
| ----------------- | -------------------------------------------------------- |
| `LOOM_BLUR=0`     | no blur (to compare)                                     |
| `LOOM_BLUR_TRACE` | prints each blur made and how long it took               |
| `LOOM_BLUR_DUMP`  | a folder: each blur's textures written there as raw RGBA |

## Checks

- `rounding` and `gaps_*` below 0 are set to 0.
- `rounding_power` is kept between 1 and 10.
- `animations` must be `"loom"` or `"kwin"`. Any other value is an error and the last good
  setting stays.
- `own_x` must be a list of names. A program is matched by its file name, so `/usr/bin/xterm`
  and `xterm` both match `"xterm"`.

## Log

Every applied change prints one line on stderr:

```text
loom: settings: rounding 12 power 2.4 gaps 4/8
```

A rejected change prints the reason, for example
`loom.opt.animations must be "loom" or "kwin", not string`.

See also: [effects](effects.md), [x-programs](x-programs.md).
