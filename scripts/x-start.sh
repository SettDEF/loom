#!/bin/sh
# x-start.sh: how fast X programs come up on loom-x and on Xwayland: the server answering, then each program until its window is viewable.
# Usage: scripts/x-start.sh   (headless loom on the GPU; LOOM_BIN = the release build with loom-x)
set -u
cd "$(dirname "$0")/.." || exit 1
T=${LOOM_BIN:-/mnt/storage/dev/cargo-target/loom/release}
[ -d "$T" ] || T=$HOME/.local/bin   # another machine: the installed loom, loom-x
OUT=$(mktemp -d)
trap 'rm -rf "${OUT:?}"' EXIT
unset ZEIT ZEIT_PANE
printf 'wm title . "Tk demo"\nbutton .b -text Hi; pack .b\n' > "$OUT/tk.tcl"

cat > "$OUT/start.sh" <<'EOF'
#!/bin/sh
out=$1; tag=$2; tk=$3
ms() { echo $(( $(date +%s%N) / 1000000 )); }
t0=$(ms)
if [ "$tag" = loom-x ]; then
  $LOOMX :27 1600x1000 2>/dev/null & x=$!
  export DISPLAY=:27
  until xset q >/dev/null 2>&1; do sleep 0.002; done
else
  until xset q >/dev/null 2>&1; do sleep 0.002; done
  # keeps Xwayland up between the programs
  xprop -root -spy >/dev/null 2>&1 & x=$!
fi
printf '%-10s %-26s %5d ms\n' "$tag" "server answering" $(( $(ms) - t0 )) >> $out
for spec in "xeyes:xeyes" "xterm:xterm" "xterm:xterm -fa Monospace" "Tk demo:wish $tk"; do
  name=${spec%%:*}; cmd=${spec#*:}
  t0=$(ms); $cmd & pid=$!
  n=0; until xwininfo -name "$name" 2>/dev/null | grep -q IsViewable || [ $n -gt 1000 ]; do sleep 0.002; n=$((n+1)); done
  printf '%-10s %-26s %5d ms\n' "$tag" "$(echo "$cmd" | sed 's@ /.*/@ @')" $(( $(ms) - t0 )) >> $out
  kill $pid 2>/dev/null; wait $pid 2>/dev/null; sleep 0.2
done
kill $x
EOF
chmod +x "$OUT/start.sh"
for tag in loom-x Xwayland; do
  LOOMX=$T/loom-x "$T/loom" --headless --gpu --size 1600x1000 -- sh -c "$OUT/start.sh $OUT/results $tag $OUT/tk.tcl; kill \$PPID" > /dev/null 2>&1
done
cat "$OUT/results"
