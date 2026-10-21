# zeal

A Wayland compositor for this desktop, in Rust on Smithay. It speaks Hyprland's protocols and
IPC (the `ii` shell runs on it unchanged) and KDE's (System Settings › Display, kscreen-doctor).

Early: see [PLAN.md](PLAN.md) for the milestones and where it stands.

```sh
cargo run --release -- --headless --size 1280x720 --snapshot out.png -- alacritty
```
