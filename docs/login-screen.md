# Login screen

loom can be your display manager. The login screen is your own `ii` lock screen, with its clock,
side panel and password field, and signing in hands the screen to your session without a black
frame.

![The login screen](img/login-screen.png)

Pick the desktop to start from the pill in the corner. Every session in
`/usr/share/wayland-sessions` is listed.

![Choosing the desktop](img/login-desktops.png)

## How it works

| Piece | What it does |
| --- | --- |
| Snapshot | Your loom copies your look into `/var/lib/loom/greeter`: screens, keyboard, ii, its colours and your fonts. Your home stays closed to the login screen. |
| `loom-greeter.service` | Runs `loom --greeter` on tty1 as the `loom-greeter` user and shows your lock screen. |
| `loom-auth.service` | The only part that runs as root. It checks the password through PAM (`/etc/pam.d/loom`) and starts the session you picked. |
| Curtain | The new session starts under the login screen's last picture, which fades once your shell's wallpaper is up. |

The password goes from the login screen straight to `loom-auth`. It never passes through the
compositor. Only the `loom-greeter` user may ask `loom-auth`, and only one sign-in runs at a time.

## Install

```sh
cargo build --release
scripts/install-greeter.sh      # installs, switches nothing
loom restart                    # in your loom session: writes the snapshot
```

Or as an Arch package: `cd packaging && makepkg -si`.

## Try it first

```sh
scripts/try-greeter.sh
```

This opens the login screen in a window on your desktop. Sign in with your real password; a
second loom starts in a window. Your real login is not touched.

## Switch

From a text console (Ctrl+Alt+F3) is safest:

```sh
sudo systemctl disable gdm && sudo systemctl enable loom-greeter && sudo reboot
```

## Switch back

If the login screen does not work, press Ctrl+Alt+F3, log in on the console, then:

```sh
sudo systemctl disable loom-greeter && sudo systemctl enable gdm && sudo reboot
```

To remove it: `scripts/install-greeter.sh --uninstall`. That works after switching back.

## Logs

```sh
journalctl -b -u loom-auth -u loom-greeter
```

## Settings

| Setting | Default | What it does |
| --- | --- | --- |
| `misc:session_lock_xray` | your Hyprland config | ii's lock is see-through; with xray its wallpaper shows under it |
| `LOOM_CURTAIN_UNTIL` | `quickshell:background` | the layer whose first picture lifts the curtain |

The design and its status are in [greeter.md](greeter.md).
