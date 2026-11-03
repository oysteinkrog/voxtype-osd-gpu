#!/usr/bin/env bash
# Cursor shape check in a private nested KWin (never the real display), for F6 and its
# fallback (voxtype-osd-gpu-95h5). Two runs of the cursor_probe example, each asking for the
# I-beam over a 400 x 120 overlay that the pointer is then moved onto with fake_click:
#
#   fallback  VO_NO_CURSOR_SHAPE=1 hides wp_cursor_shape_v1. The host must attach a theme
#             image to a cursor surface of its own, commit it, and set it with
#             wl_pointer.set_cursor; its note must name the theme's "text" or "xterm" cursor.
#   shape     The global is used: wp_cursor_shape_device_v1.set_shape, no cursor surface.
#
# Both runs are read from WAYLAND_DEBUG (wayland-rs format, objects as name#id). Expects dev builds of the cursor_probe and fake_click
# examples (cargo build -p vo-host-wayland --example cursor_probe --example fake_click);
# VO_BIN overrides the folder. VO_CURSOR_DIR is the work folder (default
# /var/tmp/claude/vo-cursor).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
bin=${VO_BIN:-${CARGO_TARGET_DIR:-$root/target}/debug/examples}
work=${VO_CURSOR_DIR:-/var/tmp/claude/vo-cursor}
for b in cursor_probe fake_click; do
    [ -x "$bin/$b" ] || { echo "cursor-fallback: $bin/$b is missing" >&2; exit 2; }
done
command -v kwin_wayland >/dev/null || { echo "cursor-fallback: kwin_wayland is not installed" >&2; exit 2; }
mkdir -p "$work/xdg-config"
fail=0
bad() { echo "cursor-fallback: FAIL $*"; fail=1; }

# Inside the nested KWin: start the probe, then move the pointer onto the middle of the
# overlay (1280 x 800 output, overlay bottom centre, 8 px up).
cat > "$work/inner.sh" <<'EOF'
#!/usr/bin/env bash
set -u
out=$1 dbg=$2 bin=$3 hide=$4
VO_NO_CURSOR_SHAPE=$hide WAYLAND_DEBUG=1 "$bin/cursor_probe" --secs 3 > "$out" 2> "$dbg" &
pid=$!
sleep 1.2
# Keep the fake pointer until the probe is done: the cursor is set on it.
FAKE_CLICK_HOLD_MS=2500 "$bin/fake_click" move 640 730 &
wait $pid
echo "probe exit $?" >> "$out"
EOF
chmod +x "$work/inner.sh"

run() {
    local name=$1 hide=$2
    XDG_CONFIG_HOME=$work/xdg-config KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 WAYLAND_DISPLAY=no-such-wl DISPLAY= \
        timeout 30 dbus-run-session -- kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
        --socket "vo-cursor-$$-$name" --width 1280 --height 800 \
        --exit-with-session "$work/inner.sh $work/$name.log $work/$name.wl $bin $hide" > "$work/kwin-$name.log" 2>&1 || true
    grep -q 'probe exit 0' "$work/$name.log" 2>/dev/null || bad "$name: the probe did not exit cleanly (see $work/$name.log)"
    grep -q 'RESULT entered=[1-9]' "$work/$name.log" 2>/dev/null || bad "$name: the pointer never entered the overlay"
}

run fallback 1
# The cursor surface: the one named in wl_pointer.set_cursor.
surf=$(grep -oP -- '-> wl_pointer#\d+\.set_cursor\(\d+, \Kwl_surface#\d+' "$work/fallback.wl" | head -1 || true)
if [ -z "$surf" ]; then
    bad "fallback: no wl_pointer.set_cursor with a surface"
else
    # Before the set_cursor: a buffer attached to that surface and committed.
    set_line=$(grep -n -m1 -- "-> wl_pointer#[0-9]*\.set_cursor([0-9]*, $surf" "$work/fallback.wl" | cut -d: -f1)
    head -n "$set_line" "$work/fallback.wl" | grep -q -- "-> $surf\.attach(wl_buffer#" || bad "fallback: no buffer attached to $surf"
    head -n "$set_line" "$work/fallback.wl" | grep -q -- "-> $surf\.commit()" || bad "fallback: $surf not committed"
    echo "cursor-fallback: fallback: $(grep -m1 -- "-> wl_pointer#[0-9]*\.set_cursor([0-9]*, $surf" "$work/fallback.wl" | sed 's/^.*-> //')"
fi
if grep -q 'wp_cursor_shape_device_v1' "$work/fallback.wl"; then
    bad "fallback: a shape device was made though the global is hidden"
fi
note=$(grep -m1 '^cursor: no wp_cursor_shape_v1, Text' "$work/fallback.log" || true)
echo "cursor-fallback: fallback: ${note:-no note}"
echo "$note" | grep -qE 'as "(text|xterm)"' || bad "fallback: the I-beam was not drawn from the theme"

run shape 0
grep -qP -- '-> wp_cursor_shape_device_v1#\d+\.set_shape\(' "$work/shape.wl" || bad "shape: no set_shape"
if grep -qP -- '-> wl_pointer#\d+\.set_cursor\(\d+, wl_surface#' "$work/shape.wl"; then
    bad "shape: a cursor surface was set though the global is there"
fi
echo "cursor-fallback: shape: $(grep -m1 -oP -- '-> \Kwp_cursor_shape_device_v1#\d+\.set_shape\(.*' "$work/shape.wl" || echo 'no set_shape')"

[ "$fail" = 0 ] && echo "cursor-fallback: ok"
exit "$fail"
