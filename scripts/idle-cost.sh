#!/bin/sh
# idle-cost.sh: what loom costs while nothing happens (wakeups and CPU over 10 s, one idle terminal)
# The battery budget: an idle desktop must stay at 0 wakeups/s. Usage: scripts/idle-cost.sh [loom-binary]
L=${1:-/mnt/storage/dev-cache/targets/release/loom}
H=$(mktemp -d)
env -u ZEIT -u ZEIT_PANE HOME=$H LIBGL_ALWAYS_SOFTWARE=1 "$L" --headless --size 960x540 --socket loom-idle-$$ --wait 16000 -- alacritty -e sh -c 'sleep 30' >/dev/null 2>&1 &
P=$!
sleep 4
t0=$(awk '{print $14+$15}' /proc/$P/stat); c0=$(awk '/^voluntary_ctxt/{print $2}' /proc/$P/status)
sleep 10
t1=$(awk '{print $14+$15}' /proc/$P/stat); c1=$(awk '/^voluntary_ctxt/{print $2}' /proc/$P/status)
kill $P 2>/dev/null; wait $P 2>/dev/null; rm -rf "$H"
echo "idle 10 s: wakeups $((c1-c0)) ($(( (c1-c0)/10 ))/s), cpu $(( (t1-t0)*10 )) ms"
[ $((c1-c0)) -lt 20 ]
