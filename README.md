# loom

A Wayland compositor for this desktop, in Rust on Smithay. It speaks Hyprland's protocols and
IPC, so the `ii` shell runs on it unchanged, and KDE's (System Settings › Display,
kscreen-doctor).

Early. See [PLAN.md](PLAN.md) for the milestones and where it stands.

![xterm on loom](docs/img/xterm.png)

## Try it

```sh
cargo run --release -- --headless --size 1280x720 --snapshot out.png -- alacritty
```

## Help

| Page                             | About                                    |
| -------------------------------- | ---------------------------------------- |
| [Settings](docs/settings.md)     | `init.lua` options                       |
| [X programs](docs/x-programs.md) | Xwayland, loom-x, `own_x`                |
| [Effects](docs/effects.md)       | rounded corners, animations              |
| [Running](docs/running.md)       | headless, nested and TTY modes, scripts  |
| [Testing](docs/testing.md)       | screenshots, recordings, cost checks     |

## Quick settings

loom starts from your Hyprland config, then runs `~/.config/loom/init.lua` over it.

```lua
loom.opt.rounding = 12
loom.opt.animations = "kwin"
loom.opt.own_x = { "xterm", "xeyes" }
```

Full list and checks in [docs/settings.md](docs/settings.md).

## Screenshots

xeyes with a shaped window, and eglgears on loom-x:

![xeyes with a shaped window](docs/img/xeyes-shape.png)

![eglgears on loom-x](docs/img/loom-x-eglgears.png)

zash, the user's terminal, running on loom:

![zash on loom](docs/img/zash.png)

A window opening with `animations = "kwin"` (headless recording):

![xterm opening](docs/img/xterm-open-kwin.webp)
