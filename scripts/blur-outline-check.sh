#!/usr/bin/env bash
# EG-07 screenshot check: compositor blur shaped to an outline.
#
# Runs `blur_probe` in private nested KWins (never the real session) over a sharp black and
# white checkerboard, with an octagon panel and with a rounded panel turned 15 degrees, and
# takes a screenshot of each. Then checks, inside the overlay surface:
#   - every pixel more than 3 px outside the outline equals the checkerboard (no blur spill);
#   - the variance inside the outline (more than 3 px in) is at most half the variance
#     outside (the desktop is blurred there).
# A third run turns KWin's blur effect off and checks the probe still draws and prints
# "no compositor blur" exactly once.
#
#   scripts/blur-outline-check.sh            # builds nothing; run first:
#   heavy-build cargo build -p vo-host-wayland --example blur_probe
#
# Output (screenshots, logs, numbers) goes to $OUT, default /var/tmp/claude/swarm/EG-07.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-/var/tmp/claude/swarm/EG-07}
bin=$root/target/debug/examples/blur_probe
[ -x "$bin" ] || { echo "no $bin: build the blur_probe example first" >&2; exit 2; }
mkdir -p "$out/xdg" "$out/xdg-noblur"
printf '[Plugins]\nblurEnabled=false\n' > "$out/xdg-noblur/kwinrc"
CHECKER=4

cat > "$out/inner.sh" <<'EOF'
#!/usr/bin/env bash
# Inside the nested KWin: run the probe, take one screenshot while it shows.
set -u
out=$1; bin=$2; tag=$3; shift 3
"$bin" "$@" > "$out/$tag.log" 2>&1 &
pid=$!
sleep 2.5
spectacle -b -n -f -o "$out/$tag.png" > "$out/$tag.spectacle.log" 2>&1
wait $pid
echo "exit $?" >> "$out/$tag.log"
EOF
chmod +x "$out/inner.sh"

run() {
  local tag=$1 cfg=$2; shift 2
  rm -f "$out/$tag.png"
  XDG_CONFIG_HOME=$cfg KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 WAYLAND_DISPLAY=no-such-wl DISPLAY= \
    timeout 60 dbus-run-session -- kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
    --socket "eg07-$tag-$$" --width 1280 --height 800 \
    --exit-with-session "$out/inner.sh $out $bin $tag $*" > "$out/$tag.kwin.log" 2>&1 || true
  cat "$out/$tag.log"
}

run octagon "$out/xdg" --secs 5 --checker $CHECKER --octagon 24
run turned "$out/xdg" --secs 5 --checker $CHECKER --turn 15
run noblur "$out/xdg-noblur" --secs 4 --checker $CHECKER --octagon 24

python3 -I - "$out" "$CHECKER" <<'EOF'
import math, sys
from PIL import Image, ImageDraw, ImageFilter
import numpy as np

out, n = sys.argv[1], int(sys.argv[2])
W, OW, OH, PW, PH, R = 1280, 720, 320, 380.0, 180.0, 28.0
ox, oy = (W - OW) // 2, 120          # overlay: anchored top, margin 120, centred
px0, py0 = (OW - PW) / 2, (OH - PH) / 2
cx, cy = px0 + PW / 2, py0 + PH / 2

def octagon(c):
    x0, y0, x1, y1 = px0, py0, px0 + PW, py0 + PH
    return [(x0, y0 + c), (x0 + c, y0), (x1 - c, y0), (x1, y0 + c), (x1, y1 - c), (x1 - c, y1), (x0 + c, y1), (x0, y1 - c)]

def rounded(r, steps=32):
    pts = []
    for (ccx, ccy, a0) in [(px0 + r, py0 + r, 180), (px0 + PW - r, py0 + r, 270), (px0 + PW - r, py0 + PH - r, 0), (px0 + r, py0 + PH - r, 90)]:
        for k in range(steps + 1):
            a = math.radians(a0 + 90 * k / steps)
            pts.append((ccx + r * math.cos(a), ccy + r * math.sin(a)))
    return pts

def turned(pts, deg):
    s, c = math.sin(math.radians(deg)), math.cos(math.radians(deg))
    return [(cx + (x - cx) * c - (y - cy) * s, cy + (x - cx) * s + (y - cy) * c) for x, y in pts]

def check(tag, pts, blurred):
    img = np.asarray(Image.open(f"{out}/{tag}.png").convert("L"), dtype=np.float64)
    img = img[oy:oy + OH, ox:ox + OW]
    mask = Image.new("L", (OW, OH), 0)
    ImageDraw.Draw(mask).polygon(pts, fill=255)
    # 3 px margins either side of the edge: the antialiased rim is neither inside nor outside.
    inner = np.asarray(mask.filter(ImageFilter.MinFilter(7))) > 0
    outer = np.asarray(mask.filter(ImageFilter.MaxFilter(7))) == 0
    ys, xs = np.mgrid[oy:oy + OH, ox:ox + OW]
    checker = np.where(((xs // n) + (ys // n)) % 2 == 0, 255.0, 0.0)
    diff = np.abs(img - checker)[outer]
    equal = float((diff <= 2).mean())
    v_in, v_out = float(img[inner].var()), float(img[outer].var())
    ratio = v_in / v_out
    ok = equal == 1.0 and (ratio <= 0.5 if blurred else True)
    print(f"{tag}: outside {outer.sum()} px, {100 * equal:.3f} % equal the checkerboard (max diff {diff.max():.0f}); "
          f"variance inside {v_in:.0f}, outside {v_out:.0f}, ratio {ratio:.4f} -> {'PASS' if ok else 'FAIL'}")
    return ok

ok = check("octagon", octagon(24), True)
ok &= check("turned", turned(rounded(R), 15), True)
log = open(f"{out}/noblur.log").read()
count = log.count("no compositor blur")
drew = check("noblur", octagon(24), False)
print(f"noblur: 'no compositor blur' printed {count} time(s), panel drawn: {drew} -> {'PASS' if count == 1 and drew else 'FAIL'}")
ok &= count == 1 and drew
sys.exit(0 if ok else 1)
EOF
