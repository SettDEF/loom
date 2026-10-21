# loom

A Wayland compositor for this desktop, in Rust on Smithay. It speaks Hyprland's protocols and
IPC (the `ii` shell runs on it unchanged) and KDE's (System Settings › Display, kscreen-doctor).

Early: see [PLAN.md](PLAN.md) for the milestones and where it stands.

```sh
cargo run --release -- --headless --size 1280x720 --snapshot out.png -- alacritty
```

## Settings (Neovim style)

loom starts from your Hyprland config (so it looks the same: `decoration { rounding = 18 }` …),
then runs `~/.config/loom/init.lua` over it. Saved changes apply at once; an error keeps the last
good settings and is logged.

```lua
loom.opt.rounding = 12                    -- like vim.opt
loom.setup { rounding_power = 2.4 }       -- several at once
if loom.hyprland.rounding > 10 then       -- what Hyprland's config says
  loom.opt.rounding = 10
end
```

Options so far: `rounding` (corner radius, logical px), `rounding_power` (2 = circle, higher = squarer).
