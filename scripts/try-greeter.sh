#!/bin/sh
# try-greeter.sh: loom's login screen in a window on your desktop, with your real password, before it
# becomes the real login (docs/login-screen.md). loom-auth checks the password through PAM as you
# (nothing as root), the greeter runs in a nested loom; after sign-in a second nested loom starts
# under the greeter's last picture (the curtain) with a terminal, not a second shell (it would clash
# with yours). Close the windows to end it.
# Usage: scripts/try-greeter.sh   (after scripts/install-greeter.sh and a 'loom restart')
set -eu
SNAP=/var/lib/loom/greeter
[ -f "$SNAP/greeter.json" ] || { echo "no snapshot in $SNAP: run 'loom restart' in your loom session first" >&2; exit 1; }
[ -f /etc/pam.d/loom ] || { echo "no /etc/pam.d/loom: run scripts/install-greeter.sh first" >&2; exit 1; }
LOOM=$(command -v loom); AUTH=$(command -v loom-auth)

D=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/loom-try.XXXX")
trap 'kill $AUTHPID 2>/dev/null; rm -rf "$D"' EXIT INT TERM
"$AUTH" --socket "$D/auth.sock" --service loom \
  --session "LOOM_CURTAIN_UNTIL=none $LOOM --nested --curtain $D/handover -- alacritty" 2>"$D/auth.log" &
AUTHPID=$!
sleep 0.3
echo "greeter in a window: your password, pick loom, Enter (log: $D/auth.log)"
LOOM_AUTH_SOCKET="$D/auth.sock" LOOM_HANDOVER_DIR="$D/handover" "$LOOM" --nested --greeter "$SNAP" || true
cat "$D/auth.log"
