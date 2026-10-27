#!/usr/bin/env bash
# The budget gate, `just budget`. Runs the toast example offline twice and live in a private
# nested KWin (never the real display), and fails when a limit is crossed:
#
#   own heap peak        VO_BUDGET_HEAP_MB          default 15
#   cold first frame     VO_BUDGET_FIRST_FRAME_MS   default 300, median of the live runs
#   frames while idle    VO_BUDGET_IDLE_FRAMES      default 0, every live run
#
# It also runs the frame_chain example once, live: 60 frames, each asked for from inside the
# frame before it. A host that drops a request made during a frame stops after one.
#
# Other settings: VO_BUDGET_RUNS (live runs, default 3), VO_BUDGET_ARGS (extra toast arguments,
# for example --cpu-first-frame), VO_BUDGET_DIR (work folder, default /var/tmp/claude/vo-budget).
#
# Offline, the two runs must write the same frames byte for byte. Live, each run hovers the
# toast (which only stops its timer, so it must cost no frame), then clicks its close button.
# "First frame" is the first frame on screen since exec, from wp_presentation feedback.
#
# Expects release builds of the toast, fake_click and frame_chain examples; the just recipe
# makes them.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
bin=$root/target/release/examples
work=${VO_BUDGET_DIR:-/var/tmp/claude/vo-budget}
heap_mb=${VO_BUDGET_HEAP_MB:-15}
first_ms=${VO_BUDGET_FIRST_FRAME_MS:-300}
idle_max=${VO_BUDGET_IDLE_FRAMES:-0}
runs=${VO_BUDGET_RUNS:-3}
args=${VO_BUDGET_ARGS:-}
width=1280
height=800

for b in toast fake_click frame_chain; do
    [ -x "$bin/$b" ] || { echo "budget: $bin/$b is missing; run 'just budget'" >&2; exit 2; }
done
command -v kwin_wayland >/dev/null || { echo "budget: kwin_wayland is not installed" >&2; exit 2; }

mkdir -p "$work"
rm -rf "$work/offline-a" "$work/offline-b" "$work"/live-* "$work"/chain*
fail=0
over() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a > b) }'; }
bad() { echo "budget: FAIL $*"; fail=1; }

# Offline: deterministic frames, and the heap peak of a render.
for run in a b; do
    "$bin/toast" --offline "$work/offline-$run" --fps 30 > "$work/offline-$run.log" 2>&1 \
        || { cat "$work/offline-$run.log"; bad "offline run $run exited with an error"; }
done
if [ "$fail" = 0 ]; then
    a=$(cd "$work/offline-a" && sha256sum -- * | sha256sum)
    b=$(cd "$work/offline-b" && sha256sum -- * | sha256sum)
    frames=$(find "$work/offline-a" -name '*.png' | wc -l)
    [ "$frames" -gt 0 ] || bad "offline run wrote no frames"
    [ "$a" = "$b" ] || bad "offline runs differ: the same scenario gave different frames"
    heap=$(grep -oP 'heap peak \K[0-9.]+' "$work/offline-a.log")
    echo "budget: offline: $frames frames, two runs identical, heap peak $heap MB (not gated: export buffers)"
fi

# Live: one nested KWin per run, so each toast starts cold against a fresh compositor.
cat > "$work/inner.sh" <<'EOF'
#!/usr/bin/env bash
# Runs inside the nested KWin: start the toast, hover it, click its close button.
set -u
out=$1; shift
bin=$1; shift
w=$1; shift
"$bin/toast" --dismiss-after 3 "$@" > "$out" 2>&1 &
pid=$!
for _ in $(seq 50); do grep -q 'close button centre' "$out" 2>/dev/null && break; sleep 0.02; done
# Surface: 352 wide, anchored top right with a 16 px margin.
read -r tx ty cx cy < <(grep -oP 'toast centre \(\K[0-9.]+, [0-9.]+\), close button centre \([0-9.]+, [0-9.]+' "$out" \
    | tr -d '(),' | awk -v ox=$((w - 16 - 352)) '{ printf "%d %d %d %d\n", ox+$1, 16+$2, ox+$6, 16+$7 }')
sleep 1.2
"$bin/fake_click" move "$tx" "$ty"
sleep 0.8
"$bin/fake_click" click "$cx" "$cy"
wait $pid
echo "toast exit $?" >> "$out"
EOF
chmod +x "$work/inner.sh"

firsts=()
for i in $(seq "$runs"); do
    log=$work/live-$i.log
    # A private config folder: a nested KWin otherwise reads and writes the user's own
    # kwinoutputconfig.json, so a scale set in one nested session leaks into every later run.
    mkdir -p "$work/xdg-config"
    # shellcheck disable=SC2086
    XDG_CONFIG_HOME=$work/xdg-config KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 WAYLAND_DISPLAY=no-such-wl DISPLAY= \
        timeout 30 dbus-run-session -- kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
        --socket "vo-budget-$$-$i" --width "$width" --height "$height" \
        --exit-with-session "$work/inner.sh $log $bin $width $args" > "$work/kwin-$i.log" 2>&1 || true
    if ! grep -q 'toast exit 0' "$log" 2>/dev/null; then
        bad "live run $i: the toast did not exit cleanly (see $log and $work/kwin-$i.log)"
        continue
    fi
    first=$(grep -oP 'first frame on screen=\K[0-9.]+' "$log" || echo "")
    heap=$(grep -oP 'heap peak \K[0-9.]+' "$log" || echo "")
    idle=$(grep -oP 'idle frames: \K[0-9]+' "$log" || echo "")
    clicked=$(grep -c 'Dismiss at' "$log" || true)
    echo "budget: live run $i: first frame ${first:-?} ms, heap peak ${heap:-?} MB, idle frames ${idle:-?}, closed by click: $([ "$clicked" -gt 0 ] && echo yes || echo no)"
    [ -n "$first" ] || bad "live run $i: no first frame time"
    [ -n "$heap" ] && over "$heap" "$heap_mb" && bad "live run $i: heap peak $heap MB is over $heap_mb MB"
    [ -n "$idle" ] && [ "$idle" -gt "$idle_max" ] && bad "live run $i: $idle frames while idle, limit $idle_max"
    [ "$clicked" -gt 0 ] || bad "live run $i: the click never reached the toast, so hover and click were not tested"
    [ -n "$first" ] && firsts+=("$first")
done

# A frame asked for from inside a frame must be drawn.
cat > "$work/chain.sh" <<'EOF'
#!/usr/bin/env bash
"$1" --frames 60 --secs 5 > "$2" 2>&1
echo "frame_chain exit $?" >> "$2"
EOF
chmod +x "$work/chain.sh"
mkdir -p "$work/xdg-config"
XDG_CONFIG_HOME=$work/xdg-config WAYLAND_DISPLAY=no-such-wl DISPLAY= \
    timeout 30 dbus-run-session -- kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
    --socket "vo-budget-$$-chain" --width "$width" --height "$height" \
    --exit-with-session "$work/chain.sh $bin/frame_chain $work/chain.log" > "$work/kwin-chain.log" 2>&1 || true
echo "budget: $(grep -m1 '^frame_chain: ' "$work/chain.log" 2>/dev/null || echo 'frame_chain: no result')"
grep -q 'frame_chain exit 0' "$work/chain.log" 2>/dev/null \
    || bad "frame_chain: a frame asked for during a frame was not drawn (see $work/chain.log)"

if [ "${#firsts[@]}" -gt 0 ]; then
    median=$(printf '%s\n' "${firsts[@]}" | sort -n | awk '{ v[NR] = $1 } END { print (NR % 2) ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2 }')
    max=$(printf '%s\n' "${firsts[@]}" | sort -n | tail -1)
    echo "budget: cold first frame: median $median ms, max $max ms, limit $first_ms ms on the median"
    over "$median" "$first_ms" && bad "cold first frame median $median ms is over $first_ms ms"
fi

if [ "$fail" = 0 ]; then
    echo "budget: ok (heap <= $heap_mb MB, first frame <= $first_ms ms, idle frames <= $idle_max)"
else
    exit 1
fi
