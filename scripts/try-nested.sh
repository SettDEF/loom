#!/bin/sh
# try-nested.sh: loom in a window on the desktop, with the user's real `ii` shell inside, sandboxed
#
# Same sandbox as run-ii.sh (system read-only, throwaway overlays over config/data/state/cache,
# own D-Bus, no system bus or PipeWire, no route to the live shell), but loom runs nested: its
# window is on the real desktop (only the host's Wayland socket and the GPU are let in).
# Close the window (or Ctrl+C here) to end it; nothing it changed survives.
#
# Usage: scripts/try-nested.sh [loom-binary]
set -eu
L=${1:-/mnt/storage/dev-cache/targets/release/loom}
[ -x "$L" ] || L=$HOME/.local/bin/loom   # another machine: the installed one
U=/run/user/$(id -u)
HOST="${XDG_RUNTIME_DIR:-$U}/${WAYLAND_DISPLAY:-wayland-1}"
[ -S "$HOST" ] || { echo "no Wayland socket at $HOST" >&2; exit 1; }
NOKEYRING=""
for f in /usr/share/dbus-1/services/*[Ss]ecret* /usr/share/dbus-1/services/*keyring* /usr/share/dbus-1/services/*kwallet*; do
  [ -e "$f" ] && NOKEYRING="$NOKEYRING --ro-bind /dev/null $f"
done

# shellcheck disable=SC2086
exec bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc --tmpfs /tmp --dir /tmp/.X11-unix \
  --unshare-pid --unshare-ipc --die-with-parent \
  --tmpfs "$U" --bind "$HOST" "$U/wayland-host" --tmpfs /run/dbus \
  --overlay-src "$HOME" --tmp-overlay "$HOME" \
  $NOKEYRING \
  --ro-bind "$(dirname "$(readlink -f "$0")")/sandbox-bin" /tmp/sandbox-bin --setenv PATH "/tmp/sandbox-bin:$PATH" \
  --setenv XDG_RUNTIME_DIR "$U" --setenv WAYLAND_DISPLAY wayland-host \
  --setenv DBUS_SYSTEM_BUS_ADDRESS unix:path=/nonexistent \
  --unsetenv DBUS_SESSION_BUS_ADDRESS --unsetenv DISPLAY \
  --unsetenv HYPRLAND_INSTANCE_SIGNATURE --unsetenv ZEIT --unsetenv ZEIT_PANE \
  "$L" --nested -- dbus-run-session -- sh -c "QT_QPA_PLATFORM=wayland qs -p $HOME/.config/quickshell/ii"
