# hb-gpu Rotated Glyph Footprint: Problem and Patch

Written 2026-10-26 so it can be sent to HarfBuzz as an issue or pull request.
It applies to `hb-gpu` in HarfBuzz 14.6.0 (commit c7a7457b). The measurements
are ours, from `crates/vo-render/examples/p0_slug.rs`.

## Problem

Glyphs drawn with `hb-gpu` get softer when they are rotated. At 45 degrees the
mean coverage error on edge pixels is about twice the upright error at every
size, and at 14 px it is more than five times as large.

## Cause

`hb_gpu_draw` and `hb_gpu_ppem` take the pixel footprint in em space from
`fwidth`:

```wgsl
let pixelsPerEm = 1.0 / fwidth (renderCoord);   // hb-gpu-draw-fragment.wgsl
let emsPerPixel = fwidth (renderCoord);          // hb_gpu_ppem, hb-gpu-fragment.wgsl
```

`fwidth(p)` is `abs(dpdx(p)) + abs(dpdy(p))`. For a glyph scaled by `k` and
rotated by `a`, the em x coordinate changes by `k cos a` per screen pixel in x
and `k sin a` per pixel in y. So `fwidth` gives `k (|cos a| + |sin a|)`, which
is `k` upright and `1.41 k` at 45 degrees. Every coverage ramp in the shader is
then up to 1.41 times wider than one pixel, and the 4-sample blend below 16 ppem
spreads its samples by the same factor.

## Patch

Use the length of the screen-space gradient of each em axis. It equals
`fwidth` on upright glyphs and does not change under rotation:

```wgsl
fn _hb_gpu_ems_per_pixel (renderCoord: vec2f) -> vec2f
{
  let dx = dpdx (renderCoord);
  let dy = dpdy (renderCoord);
  return sqrt (dx * dx + dy * dy);
}

let pixelsPerEm = 1.0 / _hb_gpu_ems_per_pixel (renderCoord);
let emsPerPixel = _hb_gpu_ems_per_pixel (renderCoord);
```

GLSL: `sqrt (dFdx (p) * dFdx (p) + dFdy (p) * dFdy (p))`. HLSL: `ddx` and
`ddy`. MSL: `dfdx` and `dfdy`. The function has to be called where `fwidth` was
called, at uniform control flow.

In vo-render the patch is applied as text when the shader loads
(`slug_gpu::shader_source_with(Footprint::AxisLength)`), so the HarfBuzz files
stay unchanged on disk. `Footprint::Fwidth` keeps the upstream behaviour.

## Measurements

Setup: Noto Sans Regular, four strings of Latin text, Norwegian letters,
digits and punctuation, 14 to 240 px effective size, rotations 0, 7 and 45
degrees. GPU coverage is compared with a CPU reference at 64 x 64 samples per
pixel (tiny-skia at 16x with its 4 x 4 anti-aliasing, box-filtered). The
numbers are mean absolute coverage error on edge pixels, where the reference is
strictly between 0 and 1.

| Size (scale 1) | Rotation | `fwidth` | Axis length |
|---|---|---|---|
| 14 px | 0 | 0.027 | 0.027 |
| 14 px | 7 | 0.050 | 0.028 |
| 14 px | 45 | 0.149 | 0.033 |
| 24 px | 0 | 0.026 | 0.026 |
| 24 px | 7 | 0.031 | 0.024 |
| 24 px | 45 | 0.067 | 0.033 |
| 56 px | 0 | 0.021 | 0.021 |
| 56 px | 7 | 0.028 | 0.021 |
| 56 px | 45 | 0.065 | 0.031 |
| 120 px | 0 | 0.019 | 0.019 |
| 120 px | 7 | 0.027 | 0.019 |
| 120 px | 45 | 0.065 | 0.031 |

Over all 48 runs (sizes 14, 24, 56 and 120 px times scales 1, 1.25, 1.5 and 2):

| | `fwidth` | Axis length |
|---|---|---|
| Edge error range at 0 degrees | 0.019 to 0.030 | same |
| Edge error range at 7 degrees | 0.026 to 0.050 | 0.019 to 0.028 |
| Edge error range at 45 degrees | 0.064 to 0.149 | 0.030 to 0.036 |
| Worst mean error over all ink pixels | 0.135 | 0.034 |
| Worst single pixel | 0.468 | 0.447 |

Upright results are identical to four digits, as expected. The worst single pixel stays
near 0.45 in both. It sits at sharp inner corners, which is a separate
limitation of the two-ray method.

GPU time for 300 glyphs on a 2100 x 1350 target, RTX 3060 Ti, measured in
alternating order: 0.044 ms at 14 px, 0.017 ms at 24 px and 0.043 ms at 56 px
for both versions. The extra square root does not show.

## Not checked

- Perspective. The gradient length is still the right local footprint, but we
  rendered no perspective text.
- Strong non-uniform scale or shear. Each axis gets its own length, so a
  stretched glyph keeps the right footprint per axis. Not measured.
- Other GPUs and the GLSL, HLSL and MSL versions. We only ran WGSL through
  naga on NVIDIA Vulkan.
