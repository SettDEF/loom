#!/bin/sh
# shot-tty.sh: a screenshot of loom running on a console (try-tty.sh), from anywhere (e.g. Hyprland)
# Finds its Wayland socket in the newest tty log and asks it with grim (loom serves wlr-screencopy).
# Usage: scripts/shot-tty.sh [out.png] [grim args, e.g. -g "0,0 800x600"]
set -eu
OUT=${1:-/tmp/loom-tty-$(date +%H%M%S).png}
[ $# -gt 0 ] && shift
LOG=$(ls -t "${XDG_STATE_HOME:-$HOME/.local/state}"/loom/tty-*.log 2>/dev/null | head -1)
[ -n "$LOG" ] || { echo "no tty log: is loom running from try-tty.sh?" >&2; exit 1; }
SOCK=$(grep -o 'WAYLAND_DISPLAY=[^;[:space:]]*' "$LOG" | tail -1 | cut -d= -f2)
[ -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/$SOCK" ] || { echo "loom's socket $SOCK is gone: not running" >&2; exit 1; }
WAYLAND_DISPLAY=$SOCK grim "$@" "$OUT" && echo "$OUT"
