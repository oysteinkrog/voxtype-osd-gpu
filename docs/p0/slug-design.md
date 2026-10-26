# Slug Text Path Design (P0-06)

Bead P0-06, 2026-10-26. This note says how the overlay draws large and
animated glyphs with HarfBuzz `hb-gpu`, per decision 2 in `docs/ROADMAP.md`
section 8. Measured numbers are in `docs/p0/text-report.md`.

## Summary

| Question | Choice |
|---|---|
| Where the glyph data lives | One storage buffer of `array<vec4<i32>>`, exactly the layout `hb-gpu`'s WGSL reads. The 16-bit blob is widened to 32 bits on upload. |
| Encoder | Rust port of `hb_gpu_draw_encode` (14.6.0), fed by skrifa outlines. Output equals HarfBuzz's byte for byte, checked by a test. No C library at run time. |
| Shader | `hb-gpu-vertex.wgsl`, `hb-gpu-fragment.wgsl` and `hb-gpu-draw-fragment.wgsl` copied unchanged, plus our own entry points in `slug-glyph.wgsl`. Two text patches at load time: one removes the storage pointer parameters, because naga rejects them; one replaces the `fwidth` footprint so rotated text stays sharp. |
| Dilation | `hb_gpu_dilate` as shipped, with the full MVP and viewport. Each glyph instance carries an em-to-pixel 2x2 matrix and an offset; the vertex shader builds the inverse for the texture coordinate. Rotation and perspective work with no change. |
| Contrast | Raw coverage from the shader, then two uniforms: `gamma` and `stem_darkening` (HarfBuzz's `hb_gpu_stem_darken`). Both are off for the accuracy test. Tuning waits for the phase 1 on-screen check. |
| Small static text | Hinted CPU raster below about 14 px (P0-14), not this path. |

## Files

| File | What it is |
|---|---|
| `crates/vo-render/src/text/wgsl/hb-gpu-*.wgsl` | Copied from HarfBuzz 14.6.0 (commit c7a7457b), unchanged. `COPYING-harfbuzz` beside them holds the licence. |
| `crates/vo-render/src/text/wgsl/slug-glyph.wgsl` | Our vertex and fragment entry points and the instance layout. |
| `crates/vo-render/src/text/slug_encode.rs` | The encoder port, with HarfBuzz's copyright notice. |
| `crates/vo-render/src/text/slug_gpu.rs` | Atlas, pipeline, instance buffer, shader assembly and the naga patch. |
| `tools/hb-gpu-dump.c` | Test-only C tool that writes HarfBuzz's own blobs for every glyph of a font. |

Licence: HarfBuzz uses the "Old MIT" licence. It asks that the copyright notice
and the two disclaimer paragraphs appear in all copies. The copied WGSL files
keep their headers, and `slug_encode.rs` repeats the notice in its module
docs. I read `COPYING` in the 14.6.0 tag on 2026-10-26.

## The blob format

What `hb-gpu` encodes per glyph, in RGBA16I texels (8 bytes each):

1. Header texel 0: ink box (min x, min y, max x, max y) in quarter font units.
2. Header texel 1: number of horizontal bands, number of vertical bands, and
   the font scale (units per em) twice.
3. One texel per band: curve count, offset of the list sorted by descending
   max, offset of the list sorted by ascending min, and the split value. The
   shader casts its ray towards whichever side of the split the pixel is on.
4. Curve index lists, one texel per entry (only `.r` is used).
5. Curve control points. Curves in one contour share end points, so a contour
   of n quadratic curves takes n + 1 texels.

Offsets are relative to the glyph's first texel, so a blob can sit anywhere in
the buffer and the shader only needs the glyph's start (`glyphLoc`). Bands are
at most 16 per axis. Coordinates are quantised to a quarter font unit, which
limits glyphs to about plus or minus 8191 font units. That fits every font
with units per em of 4096 or less.

Two differences from the Slug reference that the roadmap described: straight
segments are stored as `{p1, p1, p3}` (control point equal to the start, not
the end), and every band holds two sorted lists so the ray can go left or
right. Both come from HarfBuzz; we keep them.

## Storage buffer or texture

The WGSL HarfBuzz ships reads `ptr<storage, array<vec4<i32>>, read>`. Keeping
that means no change to the copied fetch code. The options:

| Option | For | Against |
|---|---|---|
| Storage buffer, `vec4<i32>` (chosen) | Matches the copied shader. Linear offsets. No size limit that matters (wgpu's default `max_storage_buffer_binding_size` is 128 MiB). | Twice the memory of the 16-bit blob: 16 bytes per texel. |
| Storage buffer, packed `vec2<u32>` | Same memory as the blob. Only `hb_gpu_fetch` changes: two shifts and a sign extend per channel. | A patch to a copied function. Not needed at our sizes. |
| `Rgba16Sint` 2D texture | Same memory as the blob. | Needs 2D address maths in every fetch. A 1D texture is limited to 8192 texels, and one glyph may use up to 65536. No speed gain expected on NVIDIA, where storage reads and texel fetches both go through L1. |

Memory at our scale, measured: the four test strings use 62 distinct glyphs
(61 with ink), 147 KiB as 16-bit blobs and 294 KiB in the buffer, about 4.8
KiB per glyph. The whole of Noto Sans Regular (3884 glyphs) is 9.7 MiB as
16-bit blobs. A Latin plus Norwegian set of 200
glyphs would be about 1 MiB in the buffer, far under the 32 MB GPU budget in
roadmap section 3.3. If a later design needs thousands of glyphs, switch to
the packed `vec2<u32>` form; that is one function.

This was not a close call, so I did not consult an oracle.

## Atlas management

Append only. A glyph is encoded the first time a frame needs it and its blob
goes at the end of the CPU copy. The GPU buffer grows to the next power of two
and is re-uploaded when it is too small; the bind group is rebuilt then. No
eviction in phase 0 or 1: a dictation session uses a small, stable glyph set.
Variable fonts get one blob per quantised instance (roadmap 4.4), keyed by
glyph id and instance.

## Vertex stage and dilation

One instance per glyph, drawn as a 4-vertex triangle strip:

| Field | Type | Meaning |
|---|---|---|
| `ext` | `vec4f` | Ink box in font units, from the encoder. |
| `lin` | `vec4f` | Em-to-pixel 2x2 matrix, by columns. Holds size, Y flip and rotation. |
| `offset` | `vec2f` | Pixel position of the glyph origin. |
| `loc` | `u32` | Start of the glyph's blob in the buffer. |

The vertex shader picks the corner, maps it to pixels, and calls
`hb_gpu_dilate` with the corner direction mapped through `lin` as the normal
and the inverse of `lin` as the Jacobian. `hb_gpu_dilate` pushes each corner
out by half a pixel on screen through the full MVP and corrects the texture
coordinate to match. That keeps edge pixels from being clipped at any size,
rotation or perspective. For 3D later, the per-layer 4x4 transform goes into
`u.mvp` and nothing else changes.

## Fragment stage and contrast

`hb_gpu_draw` returns coverage in 0 to 1, computed by casting one horizontal
and one vertical ray and blending the two by how close each crossing is.
The width of each coverage ramp comes from the pixel footprint in em space.
HarfBuzz measures it with `fwidth`, which grows by up to 1.41 when a glyph is
rotated. We replace that at load time with the length of each em axis
gradient (`Footprint::AxisLength`, the default), which halves the error on
rotated text and costs nothing measurable. Details and numbers:
`docs/p0/hb-gpu-footprint.md`.
Below 16 pixels per em it fades in a 4-sample supersample (5 evaluations per
pixel at 8 ppem and below). That makes small text slower, which is one more
reason static text under 14 px goes to the hinted path.

After coverage, `slug-glyph.wgsl` applies:

- `stem_darkening`: HarfBuzz's `hb_gpu_stem_darken`, a power curve that
  thickens light-on-dark text at small sizes and fades out by 48 ppem.
- `gamma`: a plain power on coverage.

The output is premultiplied: `foreground * coverage`. On the direct path the
swapchain blends in sRGB-encoded space, which is what FreeType and Qt do and
what the KDE comparison in P0-14 shows. On the effect path the scene target is
linear `Rgba16Float`, where the same coverage looks thinner, so that path will
need its own `gamma` value. Roadmap 4.4 asks for a contrast curve keyed by
foreground luminance; that is a uniform on top of this, tuned on the real
displays in phase 1. Phase 0 measures raw coverage only.

## Naga patch

HarfBuzz passes the atlas to every helper as a `ptr<storage, ...>` argument.
WGSL allows that only with the language extension
`unrestricted_pointer_parameters`, which naga 30 lists as not implemented, and
its validator rejects storage pointer arguments
(`FunctionError::InvalidArgumentPointerSpace`). `slug_gpu::shader_source`
therefore rewrites the source as it loads: it drops that parameter and each
`hb_gpu_atlas` argument, and points `hb_gpu_fetch` at the global
`hbgpu_atlas_buf`. A unit test parses and validates the result with naga, so
a HarfBuzz update that breaks the patch fails the build, not the first frame.
When naga implements the extension, the patch can go.

## Not taken from hb-gpu now

- `hb-gpu-paint` (COLRv1 colour glyphs, HarfBuzz 14.2). Same blob idea plus a
  paint op stream. Not needed until emoji or colour fonts are in scope.
- HarfBuzz's MSAA fade below 16 ppem is kept as is. If the hinted path covers
  all static small text, animated text under 16 ppem is rare enough that its
  cost does not matter.
