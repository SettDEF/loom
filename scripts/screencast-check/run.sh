#!/bin/sh
# screencast-check: screen sharing through the portal (Discord, browsers, OBS) against a headless
# loom: xdg-desktop-portal + xdg-desktop-portal-hyprland + PipeWire in their own runtime dir, home
# and D-Bus session (nothing of the running desktop is touched). The client reads brightness
# numbers only, never keeps a picture, and stops if any camera/video source exists (WirePlumber
# runs with cameras, sound and bluetooth off: a bad stream number once read the webcam).
# Usage: scripts/screencast-check/run.sh   (LOOM_BIN = the loom build to test)
set -u
here=$(cd "$(dirname "$0")" && pwd)
loom=${LOOM_BIN:-${CARGO_TARGET_DIR:-target}/release/loom}
work=$(mktemp -d /tmp/loom-sc.XXXX)   # short: socket paths have a length limit
trap 'rm -rf "${work:?}"' EXIT
mkdir -p "$work/run" "$work/home/.config/hypr" "$work/home/.config/wireplumber/wireplumber.conf.d"
chmod 700 "$work/run"
cp "$here/no-devices.conf" "$work/home/.config/wireplumber/wireplumber.conf.d/"
printf 'screencopy {\n    custom_picker_binary = %s\n}\n' "$here/picker.sh" > "$work/home/.config/hypr/xdph.conf"
cat > "$work/inner.sh" <<IN
export XDG_CURRENT_DESKTOP=Hyprland
pipewire > $work/pw.log 2>&1 &
sleep 0.7; wireplumber > $work/wp.log 2>&1 &
sleep 1.5
/usr/lib/xdg-desktop-portal-hyprland -v > $work/xdph.log 2>&1 &
sleep 0.8
/usr/lib/xdg-desktop-portal -r > $work/xdp.log 2>&1 &
(until xset q >/dev/null 2>&1; do sleep 0.05; done; python3 $here/window.py) &
sleep 2.5
python3 $here/client.py
pkill -P \$\$ 2>/dev/null
IN
env -u DBUS_SESSION_BUS_ADDRESS -u PIPEWIRE_REMOTE XDG_RUNTIME_DIR="$work/run" HOME="$work/home" \
    timeout 60 "$loom" --headless --gpu --size 1600x1000 --wait 25000 -- dbus-run-session -- sh "$work/inner.sh" 2>&1 \
    | grep -E "^(camera|ABORT|stream|node|FAIL|frame|RESULT)"
grep -h "ERR" "$work/xdph.log" 2>/dev/null | head -5
