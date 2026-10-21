#!/bin/sh
# run-ii.sh: the user's real `ii` shell on loom, headless, in a sandbox that cannot touch the live desktop
#
# bubblewrap: the system read-only; own pid namespace (pkill/killall see only the sandbox); an
# empty /run/user (no PipeWire, no real Hyprland, no qs IPC to the live shell) and no system bus
# (no NetworkManager, logind, asusctl, udisks); its own session D-Bus; writable copies of the
# config and data as throwaway overlays (it reads the real files, its writes vanish), state and cache read the same way.
#
# Usage: scripts/run-ii.sh [seconds=25] [out.png] [loom-binary]
set -eu
SECS=${1:-25}
OUT=${2:-$PWD/ii-on-loom.png}
L=${3:-/mnt/storage/dev-cache/targets/release/loom}
T=$(mktemp -d /tmp/claude-1000/ii-loom.XXXXXX)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/out"
# no keyring prompt from the sandbox's own D-Bus (the services hidden)
NOKEYRING=""
for f in /usr/share/dbus-1/services/*[Ss]ecret* /usr/share/dbus-1/services/*keyring* /usr/share/dbus-1/services/*kwallet*; do
  [ -e "$f" ] && NOKEYRING="$NOKEYRING --ro-bind /dev/null $f"
done

bwrap --ro-bind / / --dev /dev --proc /proc --tmpfs /tmp \
  --unshare-pid --unshare-ipc --die-with-parent --new-session \
  --tmpfs /run/user/"$(id -u)" --tmpfs /run/dbus \
  --overlay-src "$HOME/.config" --tmp-overlay "$HOME/.config" \
  --overlay-src "$HOME/.local/share" --tmp-overlay "$HOME/.local/share" \
  --overlay-src "$HOME/.cache" --tmp-overlay "$HOME/.cache" \
  --overlay-src "$HOME/.local/state" --tmp-overlay "$HOME/.local/state" \
  $NOKEYRING \
  --bind "$T/out" /tmp/out \
  --setenv XDG_RUNTIME_DIR /run/user/"$(id -u)" \
  --setenv DBUS_SYSTEM_BUS_ADDRESS unix:path=/nonexistent \
  --unsetenv DBUS_SESSION_BUS_ADDRESS --unsetenv WAYLAND_DISPLAY --unsetenv DISPLAY \
  --unsetenv HYPRLAND_INSTANCE_SIGNATURE --unsetenv ZEIT --unsetenv ZEIT_PANE \
  --setenv LIBGL_ALWAYS_SOFTWARE 1 \
  "$L" --headless --size 1536x960 --wait "$((SECS * 1000))" --snapshot /tmp/out/shot.png \
  -- dbus-run-session -- sh -c "QT_QPA_PLATFORM=wayland qs -p $HOME/.config/quickshell/ii > /tmp/out/qs.log 2>&1" \
  > "$T/out/loom.log" 2>&1 || true

/usr/bin/cp -f "$T/out/shot.png" "$OUT" 2>/dev/null && echo "shot: $OUT"
/usr/bin/cp -f "$T/out/qs.log" "${OUT%.png}.qs.log" 2>/dev/null && echo "shell log: ${OUT%.png}.qs.log"
/usr/bin/cp -f "$T/out/loom.log" "${OUT%.png}.loom.log" 2>/dev/null && echo "loom log: ${OUT%.png}.loom.log"
