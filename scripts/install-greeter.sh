#!/bin/sh
# install-greeter.sh: loom's login screen onto this system (docs/login-screen.md). Installs and
# enables nothing that changes how you log in: the display manager is switched by hand afterwards
# (the commands are printed at the end), so a broken login screen never locks you out.
# Usage: scripts/install-greeter.sh [--uninstall]   (asks for sudo; LOOM_BIN = the release build)
set -eu
cd "$(dirname "$0")/.."
T=${LOOM_BIN:-${CARGO_TARGET_DIR:-/mnt/storage/dev/cargo-target/loom}/release}
[ -d "$T" ] || T=target/release   # another machine: cargo's own target/
ME=$(id -un)

if [ "${1:-}" = "--uninstall" ]; then
  if systemctl is-enabled -q loom-greeter.service 2>/dev/null; then
    echo "loom-greeter is the display manager now: switch back first (sudo systemctl disable loom-greeter && sudo systemctl enable gdm)" >&2
    exit 1
  fi
  sudo rm -f /etc/systemd/system/loom-auth.service /etc/systemd/system/loom-greeter.service \
    /etc/pam.d/loom /etc/pam.d/loom-greeter /etc/sysusers.d/loom.conf /etc/tmpfiles.d/loom.conf \
    /usr/local/bin/loom /usr/local/bin/loom-x /usr/local/bin/loom-auth /usr/local/bin/loom-session
  sudo systemctl daemon-reload
  echo "removed (the loom-greeter user and /var/lib/loom are kept)"
  exit 0
fi

# the build, and a build older than the source refused (a stale target folder)
for f in loom loom-x loom-auth; do
  [ -x "$T/$f" ] || { echo "no $T/$f: cargo build --release first" >&2; exit 1; }
done
# only what the loom binary is built from (src/bin/* are other programs; dist/greeter is built in)
newest=$(find src dist/greeter Cargo.toml -newer "$T/loom" -type f -not -path 'src/bin/*' 2>/dev/null | head -1)
if [ -n "$newest" ]; then echo "$T/loom is older than $newest: cargo build --release first" >&2; exit 1; fi

echo "loom login screen: installing (needs sudo)"
# binaries where the greeter's user can run them (your home is closed to it)
for f in loom loom-x loom-auth; do sudo install -m 755 "$T/$f" "/usr/local/bin/$f"; done
sudo install -m 755 scripts/loom-session /usr/local/bin/loom-session
sudo install -m 644 dist/pam.d/loom dist/pam.d/loom-greeter -t /etc/pam.d/
sudo install -Dm 644 dist/sysusers.d/loom.conf /etc/sysusers.d/loom.conf
sudo install -Dm 644 dist/tmpfiles.d/loom.conf /etc/tmpfiles.d/loom.conf
sudo install -m 644 dist/systemd/loom-auth.service dist/systemd/loom-greeter.service -t /etc/systemd/system/
sudo systemd-sysusers /etc/sysusers.d/loom.conf
sudo systemd-tmpfiles --create /etc/tmpfiles.d/loom.conf
# the snapshot of your look: written by your loom, read by the greeter
sudo install -d -o "$ME" -m 755 /var/lib/loom/greeter
sudo systemctl daemon-reload

if [ ! -f /var/lib/loom/greeter/greeter.json ]; then
  echo "note: no snapshot yet: run 'loom restart' in your loom session (it writes /var/lib/loom/greeter)"
fi
cat <<EOF

installed. Nothing is switched: you still log in with your current display manager.

try it first, in a window on your desktop (your real password, a second loom session in it):
  scripts/try-greeter.sh

switch the login screen to loom (from a text console is safest: Ctrl+Alt+F3, log in there):
  sudo systemctl disable gdm && sudo systemctl enable loom-greeter && sudo reboot

switch back (Ctrl+Alt+F3 if the login screen does not work):
  sudo systemctl disable loom-greeter && sudo systemctl enable gdm && sudo reboot

logs: journalctl -b -u loom-auth -u loom-greeter
EOF
