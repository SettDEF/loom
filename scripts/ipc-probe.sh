#!/bin/sh
# ipc-probe.sh: a headless loom in its own runtime folder, programs on it, then Hyprland IPC requests in order
#
# Usage: scripts/ipc-probe.sh [-b loom-binary] [-w settle-seconds] -- 'program …' ['program …'] -- request …
#   each request is sent as hyprctl would ("dispatch workspace 2", "j/activewindow", "sleep 1" waits);
#   replies are printed; LOG=1 prints loom's own lines at the end. Never touches the live session.
set -eu
B=${CARGO_TARGET_DIR:-/mnt/storage/dev/cargo-target/loom}/release/loom
W=3
while [ $# -gt 0 ]; do
  case $1 in
    -b) B=$2; shift 2 ;;
    -w) W=$2; shift 2 ;;
    --) shift; break ;;
    *) echo "usage: $0 [-b loom] [-w secs] -- 'prog' … -- request …" >&2; exit 2 ;;
  esac
done
progs=""
while [ $# -gt 0 ] && [ "$1" != "--" ]; do progs="$progs$1
"; shift; done
[ $# -gt 0 ] && shift
# short: loom's socket path must stay under ~107 bytes
R=$(mktemp -d /tmp/claude-1000/ip.XXXX)
mkdir -p "$R/rt"; chmod 700 "$R/rt"
trap '[ -n "${PID:-}" ] && kill "$PID" 2>/dev/null; rm -rf "$R"' EXIT
env -u WAYLAND_DISPLAY -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u ZEIT -u ZEIT_PANE XDG_RUNTIME_DIR="$R/rt" \
  "$B" --headless --socket ip1 --size 1280x720 >"$R/log" 2>&1 &
PID=$!
for _ in $(seq 50); do [ -S "$R/rt/ip1" ] && break; sleep 0.1; done
echo "$progs" | while IFS= read -r p; do
  if [ -n "$p" ]; then
    env -u DISPLAY -u ZEIT -u ZEIT_PANE XDG_RUNTIME_DIR="$R/rt" WAYLAND_DISPLAY=ip1 sh -c "$p" >/dev/null 2>&1 &
    sleep 1.5
  fi
done
sleep "$W"
SOCK=$(ls -d "$R"/rt/hypr/*/ | head -1).socket.sock
for q in "$@"; do
  case $q in
    sleep\ *) sleep "${q#sleep }"; continue ;;
  esac
  printf '%s\n' "> $q"
  printf '%s' "$q" | timeout 3 socat - "UNIX-CONNECT:$SOCK" 2>/dev/null || echo "(no answer)"
  echo
done
[ -n "${LOG:-}" ] && grep '^loom:' "$R/log" | tail -20
exit 0
