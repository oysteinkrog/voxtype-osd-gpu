#!/usr/bin/env bash
# Clipboard check (F5) in a private nested KWin: paste into the focus demo, copy out of it,
# and check that selecting text leaves the primary selection alone (R18).
#
#   scripts/clipboard-check.sh        # after: cargo build -p vo --example focus_demo
#                                     #        cargo build -p vo-host-wayland --example fake_keys
#
# Steps inside the nested session (its own socket, never the real desktop):
#   1. wl-copy "plum" and wl-copy --primary "kiwi" on the nested socket.
#   2. focus_demo takes focus through its proxy. Ctrl+V pastes "plum" into the first field;
#      typing "s" makes "plums"; Ctrl+A selects it and Ctrl+C copies it.
#   3. Tab to the second field, Shift+Insert pastes "plums" there. Escape ends the demo.
#   4. wl-paste must print "plums" (copied out), wl-paste --primary still "kiwi".
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
bin=$root/target/debug/examples
for b in focus_demo fake_keys; do
    [ -x "$bin/$b" ] || { echo "clipboard-check: build $bin/$b first" >&2; exit 2; }
done
command -v kwin_wayland >/dev/null || { echo "clipboard-check: kwin_wayland is not installed" >&2; exit 2; }
command -v wl-copy >/dev/null || { echo "clipboard-check: wl-clipboard is not installed" >&2; exit 2; }

work=/var/tmp/claude/vo-clipboard-check/$$
mkdir -p "$work/xdg-config"
log=$work/inner.log

cat > "$work/inner.sh" <<'EOF'
#!/usr/bin/env bash
# Runs inside the nested KWin: WAYLAND_DISPLAY is its socket.
bin=$1; log=$2
exec >> "$log" 2>&1
echo "socket $WAYLAND_DISPLAY"
wl-copy "plum"
wl-copy --primary "kiwi"
"$bin/focus_demo" > "$log.demo" 2>&1 &
pid=$!
for _ in $(seq 100); do grep -q 'keyboard focus true' "$log.demo" 2>/dev/null && break; sleep 0.05; done
grep -q 'keyboard focus true' "$log.demo" || echo "no keyboard focus"
"$bin/fake_keys" +leftctrl v -leftctrl wait:200 s wait:100 +leftctrl a c -leftctrl wait:300
echo "clipboard after copy: $(timeout 2 wl-paste --no-newline)"
"$bin/fake_keys" tab wait:100 +leftshift insert -leftshift wait:300 esc
wait $pid
echo "demo exit $?"
echo "primary after: $(timeout 2 wl-paste --primary --no-newline)"
EOF
chmod +x "$work/inner.sh"

XDG_CONFIG_HOME=$work/xdg-config KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 WAYLAND_DISPLAY=no-such-wl DISPLAY= \
    timeout 40 dbus-run-session -- kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
    --socket "vo-clip-$$" --width 1280 --height 800 \
    --exit-with-session "$work/inner.sh $bin $log" > "$work/kwin.log" 2>&1 || true

cat "$log" "$log.demo" 2>/dev/null | sed 's/^/  /'
fail=0
check() { if grep -qF -- "$2" "$3"; then echo "clipboard-check: ok: $1"; else echo "clipboard-check: FAILED: $1" >&2; fail=1; fi; }
check "copy reaches wl-paste" 'clipboard after copy: plums' "$log"
check "paste from wl-copy and Shift+Insert" 'focus_demo: fields ["plums", "plums", ""]' "$log.demo"
check "primary selection unchanged" 'primary after: kiwi' "$log"
check "demo exited cleanly" 'demo exit 0' "$log"
exit $fail
