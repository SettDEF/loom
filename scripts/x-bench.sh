#!/bin/sh
# x-bench.sh: loom-x against Xwayland on the same work, headless in loom on the GPU; medians of N rounds (wall, server CPU, server memory).
# Usage: scripts/x-bench.sh [rounds]   (default 3). Uses the release build in LOOM_BIN (default the cargo-target one with loom-x).
set -u
cd "$(dirname "$0")/.." || exit 1
T=${LOOM_BIN:-/mnt/storage/dev/cargo-target/loom/release}
ROUNDS=${1:-3}
OUT=$(mktemp -d)
trap 'rm -rf "${OUT:?}"' EXIT
unset ZEIT ZEIT_PANE

# measure.sh OUT SERVER_PID NAME CMD...: name, wall ms, server CPU ms, program CPU ms, server peak MB
cat > "$OUT/measure.sh" <<'EOF'
#!/bin/bash
out=$1; spid=$2; name=$3; shift 3
tick=$(getconf CLK_TCK)
cpu() { awk -v t="$tick" '{print int(($14+$15)*1000/t)}' /proc/$1/stat 2>/dev/null || echo 0; }
s0=$(cpu $spid); t0=$(date +%s%N)
TIMEFORMAT='%R %U %S'
{ time "$@" > "$out/$name.out" 2>&1; } 2> "$out/$name.time"
t1=$(date +%s%N); s1=$(cpu $spid)
read -r _ u s < "$out/$name.time"
peak=$(awk '/VmHWM/ {print int($2/1024)}' /proc/$spid/status 2>/dev/null)
printf '%s %d %d %s %s\n' "$name" $(( (t1 - t0) / 1000000 )) $(( s1 - s0 )) "$(echo "($u + $s) * 1000 / 1" | bc)" "${peak:-0}"
EOF

# work.sh OUT SERVER_PID TAG: the same steps for every server (each time-limited)
cat > "$OUT/work.sh" <<'EOF'
#!/bin/sh
out=$1; spid=$2; tag=$3; M="$out/measure.sh $out $spid"
$M "$tag:text-core" timeout 60 xterm -fn fixed -e sh -c 'seq 1 100000'
$M "$tag:text-xft" timeout 60 xterm -fa Monospace -fs 11 -e sh -c 'seq 1 100000'
$M "$tag:gl-eglgears-10s" timeout 10 eglgears_x11
EOF
chmod +x "$OUT/measure.sh" "$OUT/work.sh"

# loom runs until told to stop: each round's command ends it (kill $PPID)
n=0
while [ $n -lt "$ROUNDS" ]; do
  "$T/loom" --headless --gpu --size 1600x1000 -- sh -c "
    $T/loom-x :27 1600x1000 2>/dev/null & x=\$!; sleep 0.6
    DISPLAY=:27 $OUT/work.sh $OUT \$x loom-x >> $OUT/results 2>&1
    kill \$x; kill \$PPID" > /dev/null 2>&1
  # a client watching the root keeps Xwayland up (and its pid the same) through every step
  "$T/loom" --headless --gpu --size 1600x1000 -- sh -c "
    xprop -root -spy > /dev/null 2>&1 & keep=\$!; sleep 1.5
    x=\$(pgrep -x Xwayland -P \$PPID | head -1)
    $OUT/work.sh $OUT \${x:-0} Xwayland >> $OUT/results 2>&1
    kill \$keep; kill \$PPID" > /dev/null 2>&1
  n=$((n + 1))
done

printf '%-26s %10s %16s %10s\n' test "wall ms" "server cpu ms" "server MB"
awk '{w[$1]=w[$1]" "$2; c[$1]=c[$1]" "$3; m[$1]=$5}
  END {for (k in w) {nw=split(w[k],a," "); split(c[k],b," "); asort(a); asort(b); mid=int((nw+1)/2); printf "%-26s %10d %16d %10d\n", k, a[mid], b[mid], m[k]}}' "$OUT/results" | sort -t: -k2,2 -k1,1
