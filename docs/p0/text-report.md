# Phase 0 Text Lane Report (P0-06, P0-07, P0-10, P0-14)

Text lane, 2026-10-26. Answers roadmap questions Q6, Q8 and Q12. P0-12
(particle look) was not done; see the end.

Machine: RTX 3060 Ti, NVIDIA 615.78.08, Vulkan through wgpu 30.0.1, CachyOS.
Font: the Plasma UI font. `~/.config/kdeglobals` says `font=Sans Serif,11`,
which fontconfig resolves to `/usr/share/fonts/noto/NotoSans-Regular.ttf`.
All runs are release builds. Raw output is in `/var/tmp/claude/vo-p0/*.txt`.

## Answers

| Question | Answer |
|---|---|
| Q6: Slug port error and speed | The Rust encoder makes the same blobs as HarfBuzz for all 3884 glyphs of Noto Sans. Against a 64 x 64 sample reference, mean error on edge pixels is 0.019 to 0.030 upright, 0.019 to 0.028 at 7 degrees and 0.030 to 0.036 at 45 degrees, with our footprint patch to the HarfBuzz shader. 300 glyphs on the 2100 x 1350 surface take 0.017 to 0.048 ms of GPU time (p50). Text path confirmed. |
| Q8: shaping and fitting cold cost | A fresh process reads the font, shapes a 40-word line with harfrust and fits it in 0.46 ms (p50), 0.55 ms (p95). A fontique fallback lookup costs 18 ms to build the system collection plus 1 ms for the query. |
| Q12: hinted small text against KDE | skrifa's light autohinter matches KDE's ink within 0.2% at 9, 10, 11 and 12 px, and looks the same on the contact sheet. Use light autohint for TY26. Oystein's eye check is still open. |

## P0-06: design

The design note is `docs/p0/slug-design.md`. Short form: storage buffer of
`vec4<i32>` as HarfBuzz's WGSL expects, encoder ported to Rust, shader files
copied unchanged and patched at load time for naga, `hb_gpu_dilate` with the
full MVP, coverage plus `gamma` and `stem_darkening` uniforms. No fork in the
road needed an oracle.

## P0-07: Slug port

### What was built

Crate `crates/vo-render`, module `text`:

- `slug_encode.rs`: Rust port of `hb_gpu_draw_encode`, its cubic-to-quadratic
  step and HarfBuzz's own quicksort (the sort is not stable, so ties only land
  in the same order with the same algorithm). Reads outlines through skrifa
  with `PathStyle::HarfBuzz`.
- `slug_gpu.rs`: atlas, pipeline, one instance per glyph, shader assembly.
- `wgsl/`: the three HarfBuzz files, unchanged, plus `slug-glyph.wgsl`.
- `examples/p0_slug.rs`: the accuracy and timing harness.
- `tests/encode_vs_harfbuzz.rs` and `tools/hb-gpu-dump.c`: the byte check.

Run `cargo run --release -p vo-render --example p0_slug`.

### Correctness checks

| Check | Result |
|---|---|
| Rust blobs against `hb_gpu_draw_encode` from the system libharfbuzz-gpu 14.6.0, all glyphs of Noto Sans Regular | 3884 of 3884 identical |
| Patched shader parses and validates in naga 30 (unit test) | passes |
| Encode cost | 62 glyphs in 0.36 ms, about 6 us per glyph |
| Pipeline creation | 69 ms on the first run, then 15, 1.9 and 2.1 ms. Probably the NVIDIA shader disk cache; not checked. |

Bug found on the way: the first version passed a corner normal in font-unit
scale to `hb_gpu_dilate`. That function scales its half-pixel push by the
length of the normal, so the quads hardly grew, and pixels just outside the ink
box were never drawn (error up to 0.5 on every stem edge). Fixed in commit
`vo-render: grow each glyph quad by half a pixel`. The normal is now the sum of
the two unit edge normals at the corner.

### Accuracy

Method: the four test strings (Latin, Norwegian letters, digits, punctuation)
laid out with cmap and advances. Each run renders coverage into an
`Rgba16Float` target with premultiplied "over" blending, and composites a CPU
reference the same way. The reference is tiny-skia at 16x, box-filtered down,
with tiny-skia's 4 x 4 anti-aliasing on, so 64 x 64 samples per pixel. Errors
are absolute coverage differences in 0 to 1, with no gamma. "Edge" pixels
are those where the reference is strictly between 0 and 1. A second reference
with 16 x 16 point samples (the 256 samples the roadmap names) gave edge errors
0.002 to 0.004 higher, because it resolves an upright edge only in steps
of 1/16.

Every case is rendered twice: with HarfBuzz's shader as shipped (`fwidth`
footprint) and with our footprint patch (axis length, the default in
vo-render; see "Rotated text" below). All 96 runs (4 sizes, 4 scales, 3
rotations, 2 footprints) are in `/var/tmp/claude/vo-p0/slug-run3.txt`. Scale 1
rows with the `fwidth` footprint:

| Size | Rotation | Max | p99 | Mean, all ink pixels | Mean, edge pixels | Pixels over 0.1 |
|---|---|---|---|---|---|---|
| 14 px | 0 | 0.227 | 0.147 | 0.025 | 0.027 | 3.0% |
| 14 px | 7 | 0.252 | 0.149 | 0.048 | 0.050 | 9.4% |
| 14 px | 45 | 0.393 | 0.344 | 0.135 | 0.149 | 59.6% |
| 24 px | 0 | 0.310 | 0.151 | 0.020 | 0.026 | 2.4% |
| 24 px | 7 | 0.267 | 0.138 | 0.024 | 0.031 | 1.8% |
| 24 px | 45 | 0.261 | 0.140 | 0.054 | 0.067 | 24.9% |
| 56 px | 0 | 0.299 | 0.088 | 0.009 | 0.021 | 0.6% |
| 56 px | 7 | 0.274 | 0.075 | 0.012 | 0.028 | 0.5% |
| 56 px | 45 | 0.404 | 0.127 | 0.030 | 0.065 | 13.9% |
| 120 px | 0 | 0.247 | 0.071 | 0.004 | 0.019 | 0.2% |
| 120 px | 7 | 0.453 | 0.056 | 0.006 | 0.027 | 0.1% |
| 120 px | 45 | 0.384 | 0.125 | 0.016 | 0.065 | 7.5% |

Worst over all 48 `fwidth` runs: max 0.468, mean on edge pixels 0.149 (14 px, scale 1,
45 degrees).

What the numbers show:

- Upright and slightly rotated text is close: about 0.02 to 0.03 mean error
  on edge pixels at every size. The error is unbiased (signed mean +0.0002 at
  120 px).
- The max is always a single pixel at a sharp inner corner, for example where
  the bowl of "u" meets its stem. This is the known weak spot of casting one
  horizontal and one vertical ray. It does not grow with size.
- With the shader as shipped, the edge error at 45 degrees is about 0.065 at
  every size and 0.149 at 14 px. The cause is the footprint, fixed below.

### Rotated text

`hb_gpu_draw` takes pixels per em from `fwidth`, which is `|dpdx| + |dpdy|`.
On a glyph rotated by `a` that overstates the footprint by `|cos a| + |sin a|`,
up to 1.41 at 45 degrees, so every coverage ramp is wider than one pixel. Our
patch uses the length of each em axis gradient instead,
`sqrt(dpdx^2 + dpdy^2)`, which equals `fwidth` upright and does not change
under rotation. It is applied as text when the shader loads, like the naga
patch; the HarfBuzz files stay unchanged. Mean error on edge pixels, before and
after:

| Size (scale 1) | 7 degrees, `fwidth` | 7 degrees, axis length | 45 degrees, `fwidth` | 45 degrees, axis length |
|---|---|---|---|---|
| 14 px | 0.050 | 0.028 | 0.149 | 0.033 |
| 24 px | 0.031 | 0.024 | 0.067 | 0.033 |
| 56 px | 0.028 | 0.021 | 0.065 | 0.031 |
| 120 px | 0.027 | 0.019 | 0.065 | 0.031 |

Over all scales the 45 degree range goes from 0.064 to 0.149 down to 0.030 to
0.036, and the worst mean over all ink pixels from 0.135 to 0.034. Upright
results do not change. GPU time does not change either (measured in
alternating order). It is the default now (`Footprint::AxisLength`);
`Footprint::Fwidth` keeps HarfBuzz's behaviour. The write-up for HarfBuzz is
`docs/p0/hb-gpu-footprint.md`.

### Proposed gate

For roadmap 3.4, to confirm in P0-16, with the axis-length footprint: mean edge
error at most 0.035 at 0 and 7 degrees and at most 0.040 at 45 degrees, for
every size from 14 px; max error at most 0.5 (sharp inner corners). A
regression is a rise of more than 0.005 in any mean.

### Contact sheets

Three panels each (GPU, reference, error in red at 4x gain), black on white,
zoomed 4x at 14 px and 2x at 24 px, one sheet per footprint:

- `/var/tmp/claude/vo-p0/slug-14px-rot{0,7,45}-{fwidth,axis}.png`
- `/var/tmp/claude/vo-p0/slug-24px-rot{0,7,45}-{fwidth,axis}.png`
- `/var/tmp/claude/vo-p0/slug-56px-rot{0,7,45}-{fwidth,axis}.png`
- `/var/tmp/claude/vo-p0/slug-120px-rot{0,7,45}-{fwidth,axis}.png`

`slug-14px-rot45-fwidth.png` against `slug-14px-rot45-axis.png` shows the
difference best.

### GPU time for 300 glyphs

300 glyph instances in rows on a 2100 x 1350 `Rgba8Unorm` target (the phase 1
card surface), one render pass with clear, timestamp queries around the pass,
400 frames after 200 warm-up frames. The clear-only pass costs 0.001 to 0.002
ms and is included. Two timing-only processes, each timing both footprints
twice in alternating order (`VO_TIMING_ONLY=1`, output in
`/var/tmp/claude/vo-p0/timing-footprint.txt`). Both footprints gave the same
numbers:

| Size | p50 | p99 |
|---|---|---|
| 14 px | 0.044 to 0.048 ms | 0.048 to 0.054 ms |
| 24 px | 0.017 to 0.018 ms | 0.018 to 0.019 ms |
| 56 px | 0.043 to 0.046 ms | 0.044 to 0.047 ms |

That is under a twentieth of the 1.0 ms phase 1 frame budget. 14 px costs
more than 24 px because HarfBuzz evaluates 5 samples per pixel below 16 ppem.

Timing at the end of a full accuracy run was less stable: up to 0.156 ms p50
at 14 px in one run, even with 200 warm-up frames, while the same binary gave
0.044 ms in a timing-only run. I do not know the cause. One run logged with
`nvidia-smi` stayed in P2 at 1770 MHz the whole time. The phase 1 bench should
time in its own process and log the P-state.

### Memory

62 distinct glyphs (61 with ink) take 147 KiB as 16-bit blobs and 294 KiB in
the `vec4<i32>` buffer, about 4.8 KiB per glyph.

## P0-10: shaping and line fitting

`examples/p0_shape.rs`: 30 fresh processes, each shapes one 40-word line
(184 glyphs) cold, then a second 40-word line warm. The font file was in the
page cache. Times in ms:

| Stage | p50 | p95 | max |
|---|---|---|---|
| `fc-match` subprocess to find the font file (harness only) | 7.15 | 9.88 | 10.11 |
| Read font file (607 KiB) | 0.293 | 0.367 | 0.373 |
| `harfrust::Font` and `ShaperFont` | 0.055 | 0.085 | 0.215 |
| First shape, 40 words | 0.077 | 0.092 | 0.098 |
| `unicode-linebreak`, 40 break opportunities | 0.004 | 0.005 | 0.006 |
| Greedy fit plus balanced fit (bisection, 20 steps) | 0.002 | 0.003 | 0.003 |
| Second shape, 40 words (warm) | 0.022 | 0.028 | 0.029 |
| Text path total, without `fc-match` | 0.461 | 0.548 | 0.655 |
| `fontique::Collection::new` (system font scan) | 17.8 | 19.9 | 21.2 |
| fontique fallback query for Han script | 1.03 | 1.10 | 1.20 |

Result: shaping and fitting are not a cold-start concern. Reading the font is
most of the 0.46 ms. Two things to keep off the first frame: spawning
`fc-match` (7 ms; use fontconfig in-process or cache the resolved path) and
building the fontique collection (18 ms; only on the first missing glyph, off
the render thread). The fallback found "Noto Sans CJK KR" for Han.

## P0-14: hinted small text against KDE

### Method

KDE side: a QML page with four `Text` items at `font.pixelSize` 9, 10, 11 and
12, `font.family: "Sans Serif"` and `renderType: Text.NativeRendering` (the
FreeType path Plasma uses for small text), shown by `qml6` in a nested
`kwin_wayland --virtual` session at scale 1 and captured there with
`spectacle`. Nothing was opened on the real display. The files are in
`/var/tmp/claude/vo-p0/kde/`.

The capture is pure grayscale: the largest difference between colour
channels is 0. So in this session Qt did not use subpixel (RGB) rendering,
even though `kdeglobals` sets `XftSubPixel=rgb`. That makes the comparison with
our grayscale path direct.

Our side (`examples/p0_hinted.rs`): the same string shaped with harfrust,
outlines from skrifa, rasterised with zeno, black on white in sRGB space, each
glyph origin rounded to a whole pixel. Three modes: skrifa light autohint
(what fontconfig's `hintslight` means in FreeType), skrifa TrueType
interpreter, unhinted.

### Results

Ink total relative to KDE (1.000 = same amount of ink):

| Size | Light autohint | TrueType interpreter | Unhinted |
|---|---|---|---|
| 9 px | 0.999 | 1.021 | 0.967 |
| 10 px | 0.998 | 0.995 | 0.904 |
| 11 px | 0.998 | 0.994 | 0.996 |
| 12 px | 0.998 | 0.996 | 0.945 |

The per-pixel difference against the KDE row, after the best whole-line shift,
is 0.05 to 0.07 mean for every mode. It mostly measures where each glyph lands
along the line, so it does not separate the modes. By eye on the 4x sheet,
light autohint matches KDE closely. The TrueType interpreter row has uneven
gaps ("Hamb urgefon" at 10 px), because it moves stems sideways while we keep
unhinted advances.

Contact sheets, four rows per size, top to bottom: KDE, light autohint,
TrueType interpreter, unhinted; sizes 9, 10, 11, 12 px:

- `/var/tmp/claude/vo-p0/hinted-vs-kde-1x.png`
- `/var/tmp/claude/vo-p0/hinted-vs-kde-4x.png` (left 260 px, 4x)

Recommendation for TY26: skrifa `Engine::Auto(None)` with
`SmoothMode::Light`, grayscale, origins on whole pixels.

Not covered: scale 1.25 (the real desktop has `Xft.dpi: 120`), QtWidgets
apps, and a dark background. The roadmap gate also asks for scale 1.5 and
Oystein's eye.

## Open problems

1. The footprint patch changes HarfBuzz's shader behaviour. It is applied at
   load time and covered by a unit test, but it should go upstream so we can
   drop it. Draft: `docs/p0/hb-gpu-footprint.md`. Not sent.
2. GPU timing was unstable at the end of long runs (see above). Cause unknown.
3. The KDE check ran at scale 1, light background, Qt Quick only. Scale 1.25
   and 1.5 and a dark panel still need a capture.
4. Contrast tuning (gamma, stem darkening, luminance-keyed contrast) is not
   measured yet. It needs the on-screen check in phase 1.
5. P0-12 (particle formation look) is not done. It waits until the spike in
   `~/work/voxtype-osd-spike` has been built by its owner.
