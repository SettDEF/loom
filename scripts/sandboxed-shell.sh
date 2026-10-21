#!/bin/sh
# sandboxed-shell.sh: the user's `ii` shell for the compositor in $WAYLAND_DISPLAY, sandboxed
#
# For loom on a TTY while Hyprland (with the real shell) still runs on another one: a second,
# free shell would fight the real one (notifications, tray, audio, the same settings files).
# Same sandbox as run-ii.sh, but only loom's Wayland socket and its Hyprland IPC are let in.
# Run by loom (it sets WAYLAND_DISPLAY, HYPRLAND_INSTANCE_SIGNATURE).
set -eu
U=/run/user/$(id -u)
RT=${XDG_RUNTIME_DIR:-$U}
: "${WAYLAND_DISPLAY:?run me from loom}"
SIG=${HYPRLAND_INSTANCE_SIGNATURE:-}
HERE=$(dirname "$(readlink -f "$0")")
NOKEYRING=""
for f in /usr/share/dbus-1/services/*[Ss]ecret* /usr/share/dbus-1/services/*keyring* /usr/share/dbus-1/services/*kwallet*; do
  [ -e "$f" ] && NOKEYRING="$NOKEYRING --ro-bind /dev/null $f"
done
HYPR=""
[ -n "$SIG" ] && [ -d "$RT/hypr/$SIG" ] && HYPR="--bind $RT/hypr/$SIG $U/hypr/$SIG"

# shellcheck disable=SC2086
exec bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc --tmpfs /tmp --dir /tmp/.X11-unix \
  --unshare-pid --unshare-ipc --die-with-parent \
  --tmpfs "$U" --bind "$RT/$WAYLAND_DISPLAY" "$U/$WAYLAND_DISPLAY" $HYPR --tmpfs /run/dbus \
  --overlay-src "$HOME/.config" --tmp-overlay "$HOME/.config" \
  --overlay-src "$HOME/.local/share" --tmp-overlay "$HOME/.local/share" \
  --overlay-src "$HOME/.cache" --tmp-overlay "$HOME/.cache" \
  --overlay-src "$HOME/.local/state" --tmp-overlay "$HOME/.local/state" \
  $NOKEYRING \
  --ro-bind "$HERE/sandbox-bin" /tmp/sandbox-bin --setenv PATH "/tmp/sandbox-bin:$PATH" \
  --setenv XDG_RUNTIME_DIR "$U" --setenv DBUS_SYSTEM_BUS_ADDRESS unix:path=/nonexistent \
  --unsetenv DBUS_SESSION_BUS_ADDRESS --unsetenv DISPLAY --unsetenv ZEIT --unsetenv ZEIT_PANE \
  dbus-run-session -- sh -c "QT_QPA_PLATFORM=wayland qs -p $HOME/.config/quickshell/ii"
