#!/bin/sh
# run-ii.sh: the user's real `ii` shell on loom, headless, in a sandbox that cannot touch the live desktop
#
# bubblewrap: the system read-only; own pid namespace (pkill/killall see only the sandbox); an
# empty /run/user (no PipeWire, no real Hyprland, no qs IPC to the live shell) and no system bus
# (no NetworkManager, logind, asusctl, udisks); its own session D-Bus; writable copies of the
# config and data as throwaway overlays (it reads the real files, its writes vanish), state and cache read the same way.
#
# Usage: scripts/run-ii.sh [seconds=25] [out.png] [loom-binary]   (LOOM_GPU=1: the GPU renderer, for blur and rounding)
#   QS_BIN=path: that quickshell build instead of the installed qs
#   LOOM_EXTRA="kitty …": a program started next to the shell; LOOM_ARGS: more loom options (--keys …); LOOM_SIZE=WxH[,WxH]: the screens
set -eu
SECS=${1:-25}
OUT=${2:-$PWD/ii-on-loom.png}
L=${3:-/mnt/storage/dev/cargo-target/loom/release/loom}
# the CPU renderer by default (no GPU in the sandbox); LOOM_GPU=1 draws with the GPU
# LOOM_BLUR_DUMP=dir: the blur textures written there (debugging)
# LOOM_BIND=dir: a folder of the real disk writable inside (frames from --record, say)
DRI_DUMP=""
for d in ${LOOM_BLUR_DUMP:-} ${LOOM_BIND:-}; do DRI_DUMP="$DRI_DUMP --bind $d $d"; done
if [ -n "${LOOM_GPU:-}" ]; then GPU="--gpu"; SOFT=0; DRI="--dev-bind /dev/dri /dev/dri"; else GPU=""; SOFT=1; DRI=""; fi
T=$(mktemp -d /tmp/claude-1000/ii-loom.XXXXXX)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/out"
# no keyring prompt from the sandbox's own D-Bus (the services hidden)
NOKEYRING=""
for f in /usr/share/dbus-1/services/*[Ss]ecret* /usr/share/dbus-1/services/*keyring* /usr/share/dbus-1/services/*kwallet*; do
  [ -e "$f" ] && NOKEYRING="$NOKEYRING --ro-bind /dev/null $f"
done

bwrap --ro-bind / / --dev /dev --proc /proc --tmpfs /tmp --dir /tmp/.X11-unix \
  --unshare-pid --unshare-ipc --die-with-parent --new-session \
  --tmpfs /run/user/"$(id -u)" --tmpfs /run/dbus \
  --overlay-src "$HOME" --tmp-overlay "$HOME" \
  $NOKEYRING $DRI $DRI_DUMP \
  --bind "$T/out" /tmp/out \
  --ro-bind "$(dirname "$(readlink -f "$0")")/sandbox-bin" /tmp/sandbox-bin --setenv PATH "/tmp/sandbox-bin:$PATH" \
  --setenv XDG_RUNTIME_DIR /run/user/"$(id -u)" \
  --setenv DBUS_SYSTEM_BUS_ADDRESS unix:path=/nonexistent \
  --unsetenv DBUS_SESSION_BUS_ADDRESS --unsetenv WAYLAND_DISPLAY --unsetenv DISPLAY \
  --unsetenv HYPRLAND_INSTANCE_SIGNATURE --unsetenv ZEIT --unsetenv ZEIT_PANE \
  --setenv LIBGL_ALWAYS_SOFTWARE "$SOFT" --setenv LOOM_BLUR "${LOOM_BLUR:-1}" ${LOOM_FOCUS_TRACE:+--setenv LOOM_FOCUS_TRACE 1} \
  "$L" --headless $GPU ${LOOM_ARGS:-} --size ${LOOM_SIZE:-1536x960} --wait "$((SECS * 1000))" --snapshot /tmp/out/shot.png \
  -- dbus-run-session -- sh -c "QT_QPA_PLATFORM=wayland ${QS_BIN:-qs} -p $HOME/.config/quickshell/ii > /tmp/out/qs.log 2>&1 & sleep 5; ${LOOM_EXTRA:-wait}" \
  > "$T/out/loom.log" 2>&1 || true

/usr/bin/cp -f "$T/out/shot.png" "$OUT" 2>/dev/null && echo "shot: $OUT"
/usr/bin/cp -f "$T/out/qs.log" "${OUT%.png}.qs.log" 2>/dev/null && echo "shell log: ${OUT%.png}.qs.log"
/usr/bin/cp -f "$T/out/loom.log" "${OUT%.png}.loom.log" 2>/dev/null && echo "loom log: ${OUT%.png}.loom.log"
