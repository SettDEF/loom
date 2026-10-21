#!/bin/sh
# try-tty.sh: loom on the real screen, from a text console (Ctrl+Alt+F3, log in, run this)
#
# Hyprland keeps running on its own console: Ctrl+Alt+F1 (or F2) goes back to it at any time.
# Ctrl+Alt+Backspace quits loom (no bind can take that key); it also quits by itself after
# --for minutes (default 10: a dead man's switch for the first tries). Everything it says goes to
# ~/.local/state/loom/tty-<time>.log, so a black screen still leaves its reason on disk.
# Inside: the user's ii shell, sandboxed (sandboxed-shell.sh), and a zash terminal.
#
# Usage: scripts/try-tty.sh [minutes=10] [loom-binary]
set -eu
MIN=${1:-10}
L=${2:-/mnt/storage/dev-cache/targets/release/loom}
HERE=$(dirname "$(readlink -f "$0")")
LOG_DIR=${XDG_STATE_HOME:-$HOME/.local/state}/loom
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/tty-$(date +%Y%m%d-%H%M%S).log"
case "$(tty)" in
  /dev/tty*) ;;
  *) echo "run this from a text console (Ctrl+Alt+F3, log in), not inside a desktop" >&2; exit 1 ;;
esac
echo "loom on this screen for $MIN min; Ctrl+Alt+Backspace quits, Ctrl+Alt+F1 back to Hyprland"
echo "log: $LOG"
unset WAYLAND_DISPLAY DISPLAY HYPRLAND_INSTANCE_SIGNATURE
"$L" --tty --for "$MIN" -- "$HERE/sandboxed-shell.sh & sleep 2; $HOME/.local/bin/zash" > "$LOG" 2>&1
echo "loom ended; the log: $LOG"
tail -5 "$LOG"
