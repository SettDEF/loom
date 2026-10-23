#!/bin/sh
# zen-input-probe.sh: does Zen act on clicks under loom? A headless loom, a fresh Zen profile and a
# test page that counts what it gets (presses, releases, link clicks, link navigations) in its
# title; then real input (loomtest over IPC) in the cases that broke on the desktop: a plain click,
# after a workspace switch and back, after a right-click menu, after a drag, after the pointer
# visited another window. Each case: PASS when the link navigated, FAIL with what the page saw.
#
# Usage: scripts/zen-input-probe.sh [-b loom-binary] [-z zen-binary]
#   KEEP=1 keeps the folder (loom's log, Zen's WAYLAND_DEBUG log with DEBUG=1, the last shot).
# Never touches the live session: its own runtime folder, socket and Zen profile.
set -eu
B=${CARGO_TARGET_DIR:-/mnt/storage/dev-cache/targets}/release/loom
Z=$(command -v tela-zen || command -v zen-browser || command -v firefox)
while [ $# -gt 0 ]; do
  case $1 in
    -b) B=$2; shift 2 ;;
    -z) Z=$2; shift 2 ;;
    -h|--help) sed -n 2,11p "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "usage: $0 [-b loom] [-z zen]" >&2; exit 2 ;;
  esac
done
[ -x "$B" ] || { echo "no loom at $B" >&2; exit 2; }
[ -n "$Z" ] || { echo "no Zen/Firefox found" >&2; exit 2; }

# short: loom's socket path must stay under ~107 bytes
R=$(mktemp -d "${TMPDIR:-/tmp}/zp.XXXX")
mkdir -p "$R/rt" "$R/prof"
chmod 700 "$R/rt"
cleanup() {
  [ -n "${ZP:-}" ] && kill "$ZP" 2>/dev/null
  [ -n "${LP:-}" ] && kill "$LP" 2>/dev/null
  wait 2>/dev/null
  if [ -n "${KEEP:-}" ]; then echo "kept: $R"; else rm -rf "$R"; fi
}
trap cleanup EXIT

# the page: a big link low enough to miss Zen's drag-the-window strip at the top, a button, a
# second window's worth of text; the counts in the title (hash resets so every click navigates)
cat > "$R/page.html" <<'EOF'
<!doctype html><title>loading</title>
<body style="margin:0;font:18px sans-serif;background:#222;color:#ddd">
<div style="height:180px;padding:20px">input probe</div>
<a id="l" href="#hit" style="display:block;margin-left:40px;width:500px;height:150px;background:#335;color:#fff">LINK</a>
<button id="b" style="margin:30px 40px;width:500px;height:80px">BUTTON</button>
<p style="margin-left:40px">some text to select by dragging over it, for the drag case</p>
<script>
const c = { md: 0, mu: 0, cl: 0, hc: 0, bc: 0 };
const show = () => { document.title = `md${c.md} mu${c.mu} cl${c.cl} hc${c.hc} bc${c.bc}`; };
addEventListener("mousedown", () => { c.md++; show(); }, true);
addEventListener("mouseup", () => { c.mu++; show(); }, true);
document.getElementById("l").addEventListener("click", () => { c.cl++; show(); });
document.getElementById("b").addEventListener("click", () => { c.bc++; show(); });
addEventListener("hashchange", () => { c.hc++; history.replaceState(null, "", location.pathname); show(); });
show();
</script>
EOF

env -u WAYLAND_DISPLAY -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u ZEIT -u ZEIT_PANE XDG_RUNTIME_DIR="$R/rt" \
  nice -n 10 "$B" --headless --socket zp1 --size 1280x720 --wait 600000 --snapshot "$R/last.png" >"$R/loom.log" 2>&1 &
LP=$!
for _ in $(seq 50); do [ -S "$R/rt/zp1" ] && break; sleep 0.1; done
SOCK=$(ls -d "$R"/rt/hypr/*/ | head -1).socket.sock
ipc() { printf '%s' "$1" | timeout 3 socat - "UNIX-CONNECT:$SOCK" 2>/dev/null; }
zen_title() { ipc "j/clients" | jq -r '.[] | select(.class=="zen" or .class=="firefox") | .title' | head -1; }
count() { zen_title | sed -n "s/.*$1\([0-9]*\).*/\1/p"; }

ZENV="WAYLAND_DISPLAY=zp1"
[ -n "${DEBUG:-}" ] && ZENV="$ZENV WAYLAND_DEBUG=1"
env -u DISPLAY -u ZEIT -u ZEIT_PANE XDG_RUNTIME_DIR="$R/rt" $ZENV nice -n 10 "$Z" --no-remote --profile "$R/prof" "file://$R/page.html" >"$R/zen.log" 2>&1 &
ZP=$!
# the page is up when its title has the counts
for _ in $(seq 120); do zen_title | grep -q '^md' && break; sleep 0.5; done
zen_title | grep -q '^md' || { echo "Zen never showed the page (see KEEP=1 and $R/loom.log)"; exit 1; }
# the title comes before Zen takes input: a click right then was lost once
sleep 2

# where the link and button are on screen: the content area's corner (found from a click below)
# is taken from Zen's layout in a 1280x720 headless screen (sidebar 196 px, top 41 px)
LX=480 LY=$((41 + 200 + 75)) BX=480 BY=$((41 + 200 + 150 + 30 + 40))
pass=0 fail=0
check() {
  # $1 name; hc before in $before; one navigation expected
  sleep 1.2
  after=$(count hc)
  if [ "${after:-0}" -gt "${before:-0}" ]; then
    pass=$((pass + 1)); echo "PASS  $1"
  else
    fail=$((fail + 1)); echo "FAIL  $1  (page saw: $(zen_title))"
  fi
}

before=$(count hc); ipc "dispatch loomtest click $LX,$LY" >/dev/null; check "plain click on a link"

# a second window on workspace 2 for the switches (the pointer lands on it there)
ipc "dispatch exec [workspace 2 silent] env GDK_BACKEND=wayland zenity --info --text other" >/dev/null
sleep 2

before=$(count hc); ipc "dispatch workspace 2" >/dev/null; sleep 0.6; ipc "dispatch workspace 1" >/dev/null; sleep 0.6
ipc "dispatch loomtest click $LX,$LY" >/dev/null; check "click after a workspace switch and back (pointer still)"

before=$(count hc); ipc "dispatch loomtest rclick $LX,$((LY + 200))" >/dev/null; sleep 0.8; ipc "dispatch loomtest keys <Esc>" >/dev/null; sleep 0.4
ipc "dispatch loomtest click $LX,$LY" >/dev/null; check "click after a right-click menu closed with Esc"

before=$(count hc); ipc "dispatch loomtest rclick $LX,$((LY + 200))" >/dev/null; sleep 0.8; ipc "dispatch loomtest click 1200,700" >/dev/null; sleep 0.4
ipc "dispatch loomtest click $LX,$LY" >/dev/null; check "click after a right-click menu closed by clicking elsewhere"

before=$(count hc); ipc "dispatch loomtest drag 240,$((BY + 70)),700,$((BY + 75))" >/dev/null; sleep 0.6
ipc "dispatch loomtest click $LX,$LY" >/dev/null; check "click after dragging over text"

before=$(count hc); ipc "dispatch loomtest drag $LX,$LY,$((LX + 60)),$((LY + 20))" >/dev/null; sleep 1
ipc "dispatch loomtest click $LX,$LY" >/dev/null; check "click after dragging the link itself"

before=$(count bc); ipc "dispatch loomtest click $BX,$BY" >/dev/null; sleep 1.2
if [ "$(count bc)" -gt "${before:-0}" ]; then pass=$((pass + 1)); echo "PASS  click on a button"; else fail=$((fail + 1)); echo "FAIL  click on a button  (page saw: $(zen_title))"; fi

for i in 1 2 3; do
  before=$(count hc); ipc "dispatch workspace 2" >/dev/null; sleep 0.4; ipc "dispatch workspace 1" >/dev/null; sleep 0.3
  ipc "dispatch loomtest click $LX,$LY" >/dev/null; check "quick switch and click, round $i"
done

# Zen's own buttons (not the page): "+ New Tab" in the sidebar must open a tab (its title shows),
# then Ctrl+W closes it again. A fresh profile has the sidebar open at this size.
# (from the window's place now: a button is where the window is, not where it was)
zen_at_y() { ipc "j/clients" | jq -r '.[] | select(.class=="zen" or .class=="firefox") | .at[1]' | head -1; }
NTX=60 NTY=$(( $(zen_at_y) + 165 ))
chrome() {
  # $1 name; what led up to it already done
  # Zen's New Tab opens its address field first; typed into, it makes the tab (the page's title
  # goes). A click that was lost leaves the keys to the page, and its title stays.
  ipc "dispatch loomtest click $NTX,$NTY" >/dev/null; sleep 0.8
  ipc "dispatch loomtest keys about:blank<CR>" >/dev/null; sleep 1.5
  if zen_title | grep -q "^md"; then
    fail=$((fail + 1)); echo "FAIL  $1  (title: $(zen_title))"
  else
    pass=$((pass + 1)); echo "PASS  $1"
    ipc "dispatch loomtest keys <C-w>" >/dev/null
  fi
  sleep 1.2
}
chrome "Zen's New Tab button"
ipc "dispatch workspace 2" >/dev/null; sleep 0.6; ipc "dispatch workspace 1" >/dev/null; sleep 0.6
chrome "Zen's New Tab button after a workspace switch"
ipc "dispatch loomtest rclick 100,238" >/dev/null; sleep 0.8; ipc "dispatch loomtest keys <Esc>" >/dev/null; sleep 0.5
chrome "Zen's New Tab button after the tab's right-click menu (Esc)"
ipc "dispatch loomtest rclick $LX,$((LY + 200))" >/dev/null; sleep 0.8; ipc "dispatch loomtest click 1200,700" >/dev/null; sleep 0.5
chrome "Zen's New Tab button after a page menu closed by a click"
ipc "dispatch loomtest click 640,93" >/dev/null; sleep 0.6; ipc "dispatch loomtest keys <Esc>" >/dev/null; sleep 0.5
chrome "Zen's New Tab button after the URL bar opened and closed"

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
