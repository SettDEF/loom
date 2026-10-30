#!/bin/sh
# install.sh: loom, loom-x and loom-session into ~/.local/bin (the session entry needs root: it prints the command).
# Usage: scripts/install.sh   (LOOM_BIN = the release build; default: where cargo builds, $CARGO_TARGET_DIR/release)
set -eu
cd "$(dirname "$0")/.."
# (/mnt/storage is one machine's data disk: elsewhere cargo's own target/)
D=/mnt/storage/dev/cargo-target/loom
[ -d "$D" ] || D=target
T=${LOOM_BIN:-${CARGO_TARGET_DIR:-$D}/release}
# a build older than the source is not what was just built (a stale target folder): refused
# only what the loom binary is built from (src/bin/* are other programs; dist/greeter is built in)
newest=$(find src dist/greeter Cargo.toml -newer "$T/loom" -type f -not -path 'src/bin/*' 2>/dev/null | head -1)
if [ -n "$newest" ]; then echo "$T/loom is older than $newest: build first (or LOOM_BIN=… for the right folder)" >&2; exit 1; fi
B=$HOME/.local/bin
mkdir -p "$B"
for f in loom loom-x; do
  [ -x "$T/$f" ] || { echo "no $T/$f: build it first (cargo build --release)" >&2; exit 1; }
  # a new file, then renamed: a running loom keeps its old one
  /usr/bin/cp -f "$T/$f" "$B/.$f.new" && /usr/bin/mv -f "$B/.$f.new" "$B/$f"
done
/usr/bin/cp -f scripts/loom-session "$B/loom-session" && chmod +x "$B/loom-session"
echo "installed loom, loom-x, loom-session in $B"
# the session entry with this home in it (dist/loom.desktop has @HOME@): it named one machine's
# /home/caesar, and on any other the login screen started nothing
S=${XDG_DATA_HOME:-$HOME/.local/share}/loom/loom.desktop
mkdir -p "$(dirname "$S")"
sed "s|@HOME@|$HOME|g" dist/loom.desktop > "$S"
# into the login screen's list here (root: sudo asks once, only when it is missing or out of date);
# without a terminal to ask in, the command is printed instead
if ! cmp -s "$S" /usr/share/wayland-sessions/loom.desktop; then
  if [ -t 0 ] && command -v sudo >/dev/null 2>&1 \
      && sudo install -Dm 644 "$S" /usr/share/wayland-sessions/loom.desktop; then
    echo "installed the session entry: loom is in the login screen's list"
  else
    echo "for the login screen's session list (once, needs root):"
    echo "  sudo install -Dm 644 $S /usr/share/wayland-sessions/loom.desktop"
  fi
fi
