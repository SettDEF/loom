#!/bin/sh
# loom stress: windows opened and closed, workspaces, fullscreen, floating, moves; loom's memory,
# fds and children sampled each round (to $1).
out=$1; rounds=${2:-40}
lpid=$(pgrep -x loom -n)
: > "$out"
sample() { echo "$1 rss_kb=$(awk '/VmRSS/{print $2}' /proc/$lpid/status) fds=$(ls /proc/$lpid/fd | wc -l) kids=$(ps -o pid= --ppid $lpid | wc -l) zombies=$(ps -eo ppid=,stat= | awk -v p=$lpid '$1==p && $2 ~ /Z/' | wc -l) clients=$(hyprctl clients -j 2>/dev/null | jq length)" >> "$out"; }
sample start
i=0
while [ $i -lt $rounds ]; do
  i=$((i+1))
  hyprctl dispatch exec alacritty >/dev/null
  hyprctl dispatch exec "kitty -o background=#203040" >/dev/null
  [ $((i % 3)) -eq 0 ] && hyprctl dispatch exec "env DISPLAY=$DISPLAY xterm" >/dev/null
  sleep 1.2
  hyprctl dispatch workspace $(( (i % 4) + 1 )) >/dev/null
  hyprctl dispatch togglefloating >/dev/null
  hyprctl dispatch movewindowpixel "exact 100 100,activewindow" >/dev/null
  hyprctl dispatch fullscreen >/dev/null
  sleep 0.4
  hyprctl dispatch fullscreen >/dev/null
  hyprctl dispatch movetoworkspace $(( ((i+1) % 4) + 1 )) >/dev/null
  # close everything on the current workspace and one more
  for a in $(hyprctl clients -j | jq -r '.[].address' | head -3); do hyprctl dispatch closewindow "address:$a" >/dev/null; done
  sleep 0.6
  [ $((i % 5)) -eq 0 ] && sample "round$i"
done
for a in $(hyprctl clients -j | jq -r '.[].address'); do hyprctl dispatch closewindow "address:$a" >/dev/null; done
sleep 3
sample end
