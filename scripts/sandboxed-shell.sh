#!/bin/sh
# sandboxed-shell.sh: the user's `ii` shell for the compositor in $WAYLAND_DISPLAY, sandboxed
#
# For loom on a TTY while Hyprland (with the real shell) still runs on another one: a second,
# free shell would fight the real one (notifications, tray, audio, the same settings files).
# Same sandbox as run-ii.sh, but only loom's Wayland socket and its Hyprland IPC are let in.
# The shell's Quickshell instance sits in the real runtime dir, so loom's binds (`qs ipc …`) reach
# it: Quickshell keeps displays apart, loom's wayland-N finds only it, Hyprland's only the real shell.
# Run by loom (it sets WAYLAND_DISPLAY, HYPRLAND_INSTANCE_SIGNATURE). Arguments: another command to
# run the same way (the user's apps: zash, dotos-gui, vitals …) instead of the shell.
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
X=""
# loom's X display (its Xwayland starts on the first X program): kept, its socket let in
if [ -n "${DISPLAY:-}" ] && [ -S "/tmp/.X11-unix/X${DISPLAY#:}" ]; then X="--bind /tmp/.X11-unix/X${DISPLAY#:} /tmp/.X11-unix/X${DISPLAY#:}"; else X="--unsetenv DISPLAY"; fi
mkdir -p "$U/quickshell"
HYPR=""
[ -n "$SIG" ] && [ -d "$RT/hypr/$SIG" ] && HYPR="--bind $RT/hypr/$SIG $U/hypr/$SIG"

# shellcheck disable=SC2086
exec bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc --tmpfs /tmp --dir /tmp/.X11-unix \
  --unshare-pid --unshare-ipc --die-with-parent \
  --tmpfs "$U" --bind "$RT/$WAYLAND_DISPLAY" "$U/$WAYLAND_DISPLAY" --bind "$U/quickshell" "$U/quickshell" $HYPR --tmpfs /run/dbus \
  --overlay-src "$HOME" --tmp-overlay "$HOME" \
  $NOKEYRING \
  --ro-bind "$HERE/sandbox-bin" /tmp/sandbox-bin --setenv PATH "/tmp/sandbox-bin:$PATH" \
  --setenv XDG_RUNTIME_DIR "$U" --setenv DBUS_SYSTEM_BUS_ADDRESS unix:path=/nonexistent \
  --unsetenv DBUS_SESSION_BUS_ADDRESS $X --unsetenv ZEIT --unsetenv ZEIT_PANE \
  dbus-run-session -- sh -c "${*:-QT_QPA_PLATFORM=wayland qs -p $HOME/.config/quickshell/ii}"
