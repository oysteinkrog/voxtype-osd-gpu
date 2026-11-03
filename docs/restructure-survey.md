# vo restructure survey

Survey for bead V-01 (ytring plan section 2.6 and 3.2). It sizes the vo crate split, maps
every module of `vo-scene` and `vo-render` to a target crate, and checks the new crate graph
for cycles. The decision record is [decisions/0001-vo-crate-split.md](decisions/0001-vo-crate-split.md).

- Surveyed tree: vo commit `5f6606a` (2026-11-03), committed files only.
- Regenerate: `git archive <commit> crates | tar -x -C <dir>`, then
  `tools/restructure-survey/run.sh <dir>/crates <out dir>`. It writes `generated.md`, which is
  everything from "Crate sizes today" to the appendix below. It needs `python3` and `tokei`.
- Check: `scripts/check-restructure-survey.py` compares the file table with `git ls-files`
  and runs in well under 1 s.

## Findings

1. **The split is cycle free once the core stops calling effects directly.** vo-render then
   depends on `vo-display` and `vo-text` only, and `vo-display` depends on no other vo crate
   except `vo-core`. Graph and edge counts are below.
2. **Several "effect" files are mostly renderer data, so most of their lines go to
   `vo-display`.** vo-render draws glass panels, particle emitters, materials, stage warps, GPU
   items, displacements and liquid glyphs from types defined in those modules. So `material.rs`,
   `stage.rs`, `gpu.rs`, `image.rs` and `svg.rs` move to `vo-display` whole, and the display
   halves of `glass.rs` (about 910 of 1248 code lines), `emitter.rs` (about 700 of 934),
   `frame.rs`, `displace.rs` and `motion/liquid.rs` move there too. The plan placed `material`
   in `vo-fx-glass`; this survey moves it to `vo-display`.
3. **`flow.rs` is live text motion, not a field effect.** Plan 3.2 and bead V-08 list `flow`
   under `vo-fx-field`. In vo-scene, `flow.rs` is the live text engine (caret, revisions,
   rewrap) and the parent of the `motion/` modules. This survey puts it in `vo-fx-text` with
   its children. Keeping it in core is the alternative; the decision record compares the two.
4. **16 modules fit no target crate in plan 3.2.** Bars, meters, frames, graticule, spectrum,
   status icon and the like (about 3,300 code lines) are widgets, neither scene core nor an
   effect. They stay in `vo-scene` for now ("vo-scene widgets" below).
5. **The core calls 40 effect items directly**, mostly from `element.rs` (builder methods and the
   `Kind` enum), `scene.rs` (drawing live text, typography, final pass, emitters) and
   `layout.rs`. These are the real work of V-06.
6. **7 effect modules use crate-private items** (limit for the V-06 split rule: 10). By the
   rule, V-P105 is not needed. See the caveat in the decision record.
7. **26 of the 38 vo-scene integration tests use text effects**, most through
   `Element::live_text`. They move to `vo-fx-text/tests/`. No vo-scene test uses a field or
   glass effect. Runner tests and examples stay in the `vo` crate, which may depend on every
   `vo-fx-*` crate.
8. **`vo-fx-glass` is small**, about 800 code lines (glass element half, `depth_stack`, the
   scene half of `displace`), once the glass display types move down.

## Types that move first

V-03 moves these before anything else, because every other target crate uses them (the
count is the number of target crates that use the item):

- `geom::{Color, Size}` (7), `geom::{Rect, Point}` (6), `geom::{RRect, Affine}`.
- `display::{Paint, ShapeInstance}` (6), `display::DisplayList` (5), `display::{Item,
  ItemKind, GlyphInstance, GlyphXform, Layer, NO_WORD, BackdropRes, ShapeKind}`.
- `path::{make, StrokeStyle, Contour, Outline}` (4 or 3).
- `material::{Material, CUSTOM_FIRST}`, `gpu::GpuSource`, `emitter::EmitterSource` (3 each).

Types two or more target crates need that do not live in `vo-display`:

- `element::TextStyle` is used by `vo-text`'s own `text.rs`, by core, widgets and two effect
  families. It moves from `element.rs` to `vo-text` (V-04).
- `text::{TextLayout, ShapedText}` and `font::FontRegistry`: used by core, `vo-fx-text`,
  `vo-fx-field` and vo-render. They live in `vo-text`.
- `element::{Element, Key, Transition}` and `canvas::Painter`: used by widgets and all three
  effect families. They stay in core and are part of the public effect API.
- `assets` (embedded fonts and the material tile) is used by `font.rs` and `material.rs`. It
  goes to `vo-display`, the lowest crate both reach. `vo-core` is the other option.

## Split files

Items that leave with a different crate than the rest of their file. Each `impl` block
follows its type.

| File | Rest of file | Items that go elsewhere |
|---|---|---|
| `path.rs` | vo-display | to vo-text: `glyph_path`, `glyph_path_styled` (glyph outlines need `font::Face`) |
| `image.rs` | vo-display | to vo-svg: `svg` (the resvg call); to core: the `impl Element` builder block |
| `element.rs` | vo-scene core | to vo-text: `TextStyle` |
| `reach.rs` | vo-scene core | to vo-display: `particle_reach` (reads only emitter display types) |
| `glass.rs` | vo-fx-glass | to vo-display: `Glass`, `GlassKind`, `GlassShape`, `GlassInstance`, `LensProfile`, `Refract`, `BlurBehind`, `BlurCache` and its keys, `push_outline`, the blur-rect functions, the shape outline helpers, the colour helpers they call, the `impl DisplayList` block, and the timing constants |
| `emitter.rs` | vo-fx-field | to vo-display: `Shape`, `Shock`, `EmitterSim`, `EmitterLook`, `SpriteStyle`, `GlyphChars`, `ColorBy`, `EmitterSource`, `SourceRef`, `EmitterInstance`, `WordParticles`, `WordSource`, `ParticleWord(s)`, `Target`, `ShapeTarget`, `TimedMask`, `MaskField`, `MAX_SHAPE_POINTS`, `tilt_x`, `sample_path` and the mask helpers |
| `motion/liquid.rs` | vo-fx-text | to vo-display: `LiquidRevision`, `LiquidPart`, `LiquidGlyph`, `LIQUID_SECS` |
| `frame.rs` | vo-scene widgets | to vo-display: `Corner`, `Edge`, `Notch`, `Frame`, `Stripes`, `Grid` and the polygon helpers (glass shapes use the frame outline) |
| `displace.rs` | vo-fx-glass | to vo-display: `Displace`, `Onset`, `Ripples`, `PageCurl` |

The exact lists are `SPLIT` in `tools/restructure-survey/survey.py`.

## Crate sizes today

Rust lines per crate from `tokei`, `src/` only, then `tests/` and `examples/` where present. Code lines leave out comments and blank lines.

| Crate | src files | src code | src lines | tests code | examples code |
|---|---:|---:|---:|---:|---:|
| vo | 21 | 9290 | 10351 | 536 | 10829 |
| vo-3d | 23 | 6915 | 7657 | 2534 | 2474 |
| vo-audio | 5 | 1519 | 1665 | 171 | 414 |
| vo-core | 16 | 4803 | 5429 | - | 274 |
| vo-dots | 8 | 1979 | 2184 | 332 | 332 |
| vo-host | 1 | 268 | 317 | - | - |
| vo-host-wayland | 5 | 2295 | 2582 | - | 1212 |
| vo-render | 26 | 11802 | 12839 | 10167 | 1342 |
| vo-scene | 73 | 31251 | 35168 | 5069 | 49 |
| vo-sim | 7 | 1837 | 2051 | 737 | 107 |

## Totals per target crate

Lines of `vo-scene/src` and `vo-render/src` that each target crate receives. A split file counts toward each target by the share of its lines the items take (see the split column below), so these are estimates within a few percent. `vo-scene widgets` stays in the `vo-scene` crate.

| Target | code lines | lines | Bead that moves it |
|---|---:|---:|---|
| vo-display | 6295 | 7137 | V-03 |
| vo-text | 2673 | 3014 | V-04 |
| vo-svg | 23 | 26 | V-05 |
| vo-scene core | 7031 | 7911 | stays (V-06 adds the effect API) |
| vo-scene widgets | 3309 | 3744 | stays (open question 1) |
| vo-fx-text | 8972 | 10033 | V-07 |
| vo-fx-field | 2157 | 2413 | V-08 |
| vo-fx-glass | 792 | 890 | V-09 |
| vo-render | 11802 | 12839 | stays (imports change in V-03, V-04) |

## Every file and its target

One row per `.rs` file under `vo-scene/src` and `vo-render/src`. `scripts/check-restructure-survey.py` checks that this table and the files in git match. Split: the file's items go to more than one crate; the item lists are in the split table below.

<!-- survey-table-start -->
| File | code | lines | Target | Split |
|---|---:|---:|---|---|
| `vo-scene/src/ambient.rs` | 288 | 322 | vo-scene core |  |
| `vo-scene/src/arrival.rs` | 50 | 58 | vo-scene core |  |
| `vo-scene/src/assets.rs` | 177 | 204 | vo-display |  |
| `vo-scene/src/bars.rs` | 510 | 561 | vo-scene widgets |  |
| `vo-scene/src/bind.rs` | 205 | 231 | vo-scene core |  |
| `vo-scene/src/canvas.rs` | 121 | 145 | vo-scene core |  |
| `vo-scene/src/centreline.rs` | 1574 | 1702 | vo-fx-text |  |
| `vo-scene/src/container.rs` | 98 | 113 | vo-scene core |  |
| `vo-scene/src/curve.rs` | 290 | 332 | vo-fx-text |  |
| `vo-scene/src/depth_stack.rs` | 434 | 494 | vo-fx-glass |  |
| `vo-scene/src/displace.rs` | 159 | 182 | vo-fx-glass | vo-display ~138; vo-fx-glass ~21 |
| `vo-scene/src/display.rs` | 939 | 1074 | vo-display |  |
| `vo-scene/src/dots.rs` | 63 | 78 | vo-scene widgets |  |
| `vo-scene/src/element.rs` | 1574 | 1816 | vo-scene core | vo-scene core ~1509; vo-text ~65 |
| `vo-scene/src/emitter.rs` | 934 | 1054 | vo-fx-field | vo-display ~704; vo-fx-field ~230 |
| `vo-scene/src/emitter_signals.rs` | 111 | 131 | vo-fx-field |  |
| `vo-scene/src/erase.rs` | 97 | 112 | vo-scene widgets |  |
| `vo-scene/src/final_pass.rs` | 250 | 288 | vo-fx-text |  |
| `vo-scene/src/flow.rs` | 2503 | 2785 | vo-fx-text |  |
| `vo-scene/src/fluid.rs` | 365 | 422 | vo-fx-field |  |
| `vo-scene/src/focus.rs` | 299 | 342 | vo-scene core |  |
| `vo-scene/src/font.rs` | 529 | 614 | vo-text |  |
| `vo-scene/src/frame.rs` | 438 | 506 | vo-scene widgets | vo-display ~317; vo-scene widgets ~121 |
| `vo-scene/src/geom.rs` | 202 | 239 | vo-display |  |
| `vo-scene/src/glass.rs` | 1248 | 1379 | vo-fx-glass | vo-display ~911; vo-fx-glass ~337 |
| `vo-scene/src/glyph_morph.rs` | 301 | 342 | vo-fx-text |  |
| `vo-scene/src/gpu.rs` | 340 | 384 | vo-display |  |
| `vo-scene/src/graticule.rs` | 226 | 256 | vo-scene widgets |  |
| `vo-scene/src/hearing.rs` | 546 | 614 | vo-scene widgets |  |
| `vo-scene/src/image.rs` | 579 | 663 | vo-display | vo-display ~548; vo-svg ~23; vo-scene core ~9 |
| `vo-scene/src/inline.rs` | 263 | 297 | vo-text |  |
| `vo-scene/src/input.rs` | 77 | 87 | vo-scene core |  |
| `vo-scene/src/inspect.rs` | 124 | 134 | vo-scene core |  |
| `vo-scene/src/lattice.rs` | 1091 | 1190 | vo-fx-field |  |
| `vo-scene/src/layout.rs` | 248 | 292 | vo-scene core |  |
| `vo-scene/src/levels.rs` | 140 | 164 | vo-scene widgets |  |
| `vo-scene/src/lib.rs` | 73 | 76 | vo-scene core |  |
| `vo-scene/src/lines.rs` | 104 | 117 | vo-fx-text |  |
| `vo-scene/src/material.rs` | 485 | 562 | vo-display |  |
| `vo-scene/src/meters.rs` | 135 | 154 | vo-scene widgets |  |
| `vo-scene/src/motion/bell.rs` | 124 | 139 | vo-fx-text |  |
| `vo-scene/src/motion/letter_motion.rs` | 77 | 91 | vo-fx-text |  |
| `vo-scene/src/motion/letter_physics.rs` | 231 | 263 | vo-fx-text |  |
| `vo-scene/src/motion/liquid.rs` | 199 | 224 | vo-fx-text | vo-fx-text ~139; vo-display ~60 |
| `vo-scene/src/motion/magnet.rs` | 249 | 284 | vo-fx-text |  |
| `vo-scene/src/motion/margin.rs` | 132 | 144 | vo-fx-text |  |
| `vo-scene/src/motion/overflow.rs` | 266 | 298 | vo-fx-text |  |
| `vo-scene/src/motion/palimpsest.rs` | 178 | 205 | vo-fx-text |  |
| `vo-scene/src/motion/revisions.rs` | 321 | 357 | vo-fx-text |  |
| `vo-scene/src/motion/spine.rs` | 118 | 131 | vo-fx-text |  |
| `vo-scene/src/motion/var_axes.rs` | 217 | 245 | vo-fx-text |  |
| `vo-scene/src/path.rs` | 1022 | 1137 | vo-display | vo-display ~983; vo-text ~39 |
| `vo-scene/src/reach.rs` | 445 | 520 | vo-scene core | vo-scene core ~422; vo-display ~23 |
| `vo-scene/src/repeat.rs` | 468 | 524 | vo-scene core |  |
| `vo-scene/src/reveal.rs` | 137 | 161 | vo-scene widgets |  |
| `vo-scene/src/ribbon.rs` | 168 | 185 | vo-fx-field |  |
| `vo-scene/src/roll.rs` | 81 | 93 | vo-scene widgets |  |
| `vo-scene/src/scene.rs` | 2674 | 2921 | vo-scene core |  |
| `vo-scene/src/segments.rs` | 218 | 240 | vo-scene widgets |  |
| `vo-scene/src/snap.rs` | 88 | 103 | vo-scene widgets |  |
| `vo-scene/src/spectrum.rs` | 328 | 375 | vo-scene widgets |  |
| `vo-scene/src/stage.rs` | 192 | 245 | vo-display |  |
| `vo-scene/src/status_icon.rs` | 349 | 389 | vo-scene widgets |  |
| `vo-scene/src/story.rs` | 165 | 197 | vo-scene core |  |
| `vo-scene/src/stroke_write.rs` | 1167 | 1309 | vo-fx-text |  |
| `vo-scene/src/svg.rs` | 276 | 288 | vo-display |  |
| `vo-scene/src/swap.rs` | 37 | 44 | vo-scene widgets |  |
| `vo-scene/src/text.rs` | 1777 | 1985 | vo-text |  |
| `vo-scene/src/timed.rs` | 201 | 225 | vo-scene core |  |
| `vo-scene/src/trace.rs` | 233 | 260 | vo-scene widgets |  |
| `vo-scene/src/typo.rs` | 476 | 556 | vo-fx-text |  |
| `vo-scene/src/water.rs` | 192 | 225 | vo-fx-field |  |
| `vo-scene/src/word_voice.rs` | 255 | 289 | vo-fx-text |  |
| `vo-render/src/bind_cache.rs` | 46 | 53 | vo-render |  |
| `vo-render/src/blur.rs` | 320 | 344 | vo-render |  |
| `vo-render/src/color.rs` | 123 | 139 | vo-render |  |
| `vo-render/src/cpu.rs` | 1129 | 1220 | vo-render |  |
| `vo-render/src/displace.rs` | 191 | 204 | vo-render |  |
| `vo-render/src/dots.rs` | 101 | 114 | vo-render |  |
| `vo-render/src/glass.rs` | 620 | 668 | vo-render |  |
| `vo-render/src/glyph_points.rs` | 272 | 311 | vo-render |  |
| `vo-render/src/gpu.rs` | 51 | 57 | vo-render |  |
| `vo-render/src/hot.rs` | 8 | 10 | vo-render |  |
| `vo-render/src/image.rs` | 283 | 305 | vo-render |  |
| `vo-render/src/lib.rs` | 26 | 28 | vo-render |  |
| `vo-render/src/material.rs` | 121 | 135 | vo-render |  |
| `vo-render/src/offscreen.rs` | 161 | 178 | vo-render |  |
| `vo-render/src/particle_layer.rs` | 559 | 610 | vo-render |  |
| `vo-render/src/particle_text.rs` | 166 | 186 | vo-render |  |
| `vo-render/src/particles.rs` | 1908 | 2062 | vo-render |  |
| `vo-render/src/profile.rs` | 172 | 190 | vo-render |  |
| `vo-render/src/renderer.rs` | 2844 | 3055 | vo-render |  |
| `vo-render/src/renderer/compose.rs` | 414 | 454 | vo-render |  |
| `vo-render/src/shapes.rs` | 236 | 262 | vo-render |  |
| `vo-render/src/text/mod.rs` | 2 | 3 | vo-render |  |
| `vo-render/src/text/slug_encode.rs` | 603 | 656 | vo-render |  |
| `vo-render/src/text/slug_gpu.rs` | 441 | 486 | vo-render |  |
| `vo-render/src/text_field.rs` | 479 | 530 | vo-render |  |
| `vo-render/src/vector.rs` | 526 | 579 | vo-render |  |
<!-- survey-table-end -->

## Target crate graph

Each edge counts the distinct items one target uses from another, found from `crate::` and `vo_scene::` paths outside test modules. A `use` at the top of a split file counts for the items whose bodies name the import.

| From | To | Items | Examples |
|---|---|---:|---|
| vo-fx-field | vo-display | 37 | `emitter -> display::DisplayList`, `emitter -> geom::Color`, `emitter -> geom::Point` |
| vo-fx-field | vo-scene core | 12 | `fluid -> element::Element`, `fluid -> element::Key`, `lattice -> ambient::Loop` |
| vo-fx-field | vo-scene widgets | 3 | `ribbon -> levels::Levels`, `ribbon -> levels::Stops`, `ribbon -> levels::smoothstep` |
| vo-fx-field | vo-text | 6 | `emitter -> font::FontRegistry`, `emitter -> path::glyph_path`, `emitter -> text::ShapedText` |
| vo-fx-glass | vo-display | 2 | `depth_stack -> geom::Color`, `depth_stack -> geom::Size` |
| vo-fx-glass | vo-scene core | 5 | `depth_stack -> element::Element`, `depth_stack -> element::Key`, `depth_stack -> element::Motion` |
| vo-fx-text | vo-display | 75 | `centreline -> geom::Point`, `curve -> display::GlyphInstance`, `curve -> display::GlyphXform` |
| vo-fx-text | vo-scene core | 12 | `curve -> canvas::Painter`, `flow -> arrival::Arrival`, `flow -> arrival::INK` |
| vo-fx-text | vo-text | 20 | `centreline -> font::Face`, `centreline -> font::FontRegistry`, `centreline -> path::glyph_path` |
| vo-render | vo-display | 163 | `color.rs -> geom::Color`, `cpu.rs -> display::BlendMode`, `cpu.rs -> display::Bloom` |
| vo-render | vo-text | 9 | `cpu.rs -> font::Face`, `cpu.rs -> font::FontRegistry`, `glyph_points.rs -> font::Face` |
| vo-scene core | vo-display | 131 | `ambient -> display::Paint`, `ambient -> display::ShapeInstance`, `ambient -> geom::Color` |
| vo-scene core | vo-fx-field (upward) | 3 | `element -> emitter::Emitter`, `scene -> emitter::WORD_PARTICLES_BUSY`, `scene -> emitter::push_instance` |
| vo-scene core | vo-fx-text (upward) | 37 | `element -> final_pass::FinalPass`, `element -> flow::Caret`, `element -> flow::CaseChange` |
| vo-scene core | vo-scene widgets (upward) | 8 | `element -> bars::Bars`, `element -> dots::Dots`, `element -> spectrum::Spectrum` |
| vo-scene core | vo-text | 15 | `element -> font::FontId`, `element -> inline::Inline`, `layout -> element::TextStyle` |
| vo-scene widgets | vo-display | 81 | `bars -> display::Paint`, `bars -> display::ShapeInstance`, `bars -> geom::Color` |
| vo-scene widgets | vo-scene core | 34 | `erase -> canvas::Painter`, `frame -> element::Element`, `frame -> element::Key` |
| vo-scene widgets | vo-text | 4 | `graticule -> element::TextStyle`, `graticule -> text::Align`, `roll -> element::TextStyle` |
| vo-text | vo-display | 12 | `font -> assets::AssetKind`, `font -> assets::get`, `font -> assets::names` |

Cycles after the upward edges are inverted: none. Cycles in today's code if the files were moved as they are: 3 (vo-fx-field -> vo-scene core -> vo-fx-field; vo-scene core -> vo-fx-text -> vo-scene core; vo-scene core -> vo-scene widgets -> vo-scene core).

Edges from `vo-render` or `vo-display` to `vo-scene` (core or widgets) or to any `vo-fx-*` crate: none.

### Upward edges the effect API must invert

Today the core calls into these effect items directly. After the split the core cannot name them, so each one becomes a call through the effect API (V-06): the effect registers, the core calls a trait method.

- **vo-scene core -> vo-fx-field** (3): `element -> emitter::Emitter`, `scene -> emitter::WORD_PARTICLES_BUSY`, `scene -> emitter::push_instance`
- **vo-scene core -> vo-fx-text** (37): `element -> final_pass::FinalPass`, `element -> flow::Caret`, `element -> flow::CaseChange`, `element -> flow::EditDecor`, `element -> flow::LiveMotion`, `element -> flow::Reading`, `element -> flow::Revise`, `element -> flow::Rewrap`, `element -> flow::SettleInk`, `element -> motion/bell::Bell`, `element -> motion/letter_motion::LetterMotions`, `element -> motion/letter_physics::LetterPhysics`, `element -> motion/magnet::Magnet`, `element -> motion/margin::MarginMark`, `element -> motion/overflow::Overflow`, `element -> motion/revisions::RevisionMotion`, `element -> motion/spine::Spine`, `element -> motion/var_axes::Breath`, `element -> typo::Typos`, `layout -> flow::breath_toward`, `layout -> flow::live_spans`, `layout -> flow::word_font_ranges`, `scene -> final_pass::FinalPass`, `scene -> final_pass::self`, `scene -> flow::Flow`, `scene -> flow::FlowInput`, `scene -> flow::STILL_FADE`, `scene -> flow::breaks_envelope`, `scene -> flow::edit_frame`, `scene -> flow::word_ranges`, `scene -> motion/margin::emit`, `scene -> motion/overflow::Place`, `scene -> motion/overflow::emit`, `scene -> motion/overflow::emit_shadows`, `scene -> motion/spine::emit`, `scene -> typo::Envelope`, `scene -> typo::TypoState`
- **vo-display -> vo-svg** (1, found by hand): `image::decode` and `image::decode_bytes` call `svg()`. vo-display gets an SVG decoder hook that vo-svg fills.

`vo-scene core -> vo-scene widgets` is listed as upward too, but both sides stay in one crate, so it needs no change now.

## Effect modules that use crate-private items

**7 effect modules** use a `pub(crate)` item of a module that stays behind. That is 10 or fewer, so by the V-06 split rule V-06 rewires all of them and V-P105 is not needed. Read the caveat in the decision record: the 40 upward items above are the larger part of V-06's work and this count does not measure them.

| Effect module | Family | Items | Crate-private items used |
|---|---|---:|---|
| `curve` | vo-fx-text | 1 | `canvas::layer (member)` |
| `depth_stack` | vo-fx-glass | 1 | `element::presence (member)` |
| `lines` | vo-fx-text | 3 | `element::children (member)`, `element::motion (member)`, `element::presence (member)` |
| `motion/margin` | vo-fx-text | 1 | `scene::push_item` |
| `motion/overflow` | vo-fx-text | 2 | `scene::close_items`, `scene::push_item` |
| `motion/spine` | vo-fx-text | 1 | `scene::push_item` |
| `word_voice` | vo-fx-text | 1 | `font::Fnv` |

Method: a top-level `pub(crate)` item counts when the effect module names it by path; a `pub(crate)` field or method counts when the effect module names the declaring module and calls `.name` on something, and does not declare a field or method of that name itself. This is a text scan, not the compiler. V-P301's throwaway crate gives the exact list.

## Inherent impls that cannot leave vo-scene

Rust allows `impl Element<M> { .. }` only in the crate that defines `Element`. These files add builder methods that way:

- `bind.rs`: 2 block(s), target vo-scene core
- `frame.rs`: 1 block(s), target vo-scene widgets
- `image.rs`: 1 block(s), target vo-display
- `reach.rs`: 1 block(s), target vo-scene core
- `repeat.rs`: 1 block(s), target vo-scene core
- `ribbon.rs`: 1 block(s), target vo-fx-field
- `segments.rs`: 1 block(s), target vo-scene widgets
- `spectrum.rs`: 1 block(s), target vo-scene widgets
- `trace.rs`: 1 block(s), target vo-scene widgets
- `glass.rs`: one `impl DisplayList` block, which goes to vo-display with the glass display types.

Only `ribbon` leaves vo-scene among these, so it needs an extension trait (or a free constructor) in V-08. `image`'s block stays in core with the split.

## Tests and runner files that use an effect

Family found from paths, public effect type names and Element builder methods that belong to one family. vo-scene must end with no dev-dependency on any `vo-fx-*` crate, so every vo-scene test listed here moves.

| File | Families | Evidence | Target home |
|---|---|---|---|
| `vo-scene/tests/caret_centred.rs` | text | text: .live_text(), .magnetic_caret(), Magnet, flow | vo-fx-text/tests/ |
| `vo-scene/tests/caret_gradient.rs` | text | text: .live_text(), flow | vo-fx-text/tests/ |
| `vo-scene/tests/centreline.rs` | text | text: CentreStroke, GlyphCentre, Lookup, centreline | vo-fx-text/tests/ |
| `vo-scene/tests/final_pass.rs` | text | text: .final_pass(), .live_text(), FinalPass, final_pass | vo-fx-text/tests/ |
| `vo-scene/tests/glyph_follow.rs` | text | text: .live_text() | vo-fx-text/tests/ |
| `vo-scene/tests/inline_flow.rs` | text | text: .live_text() | vo-fx-text/tests/ |
| `vo-scene/tests/letter_motion.rs` | text | text: .letter_motion(), .live_text(), LetterMotions, motion/letter_motion | vo-fx-text/tests/ |
| `vo-scene/tests/letter_physics.rs` | text | text: .letter_physics(), .live_text(), LetterPhysics, motion/letter_physics | vo-fx-text/tests/ |
| `vo-scene/tests/lines.rs` | text | text: Ages, lines | vo-fx-text/tests/ |
| `vo-scene/tests/live_fallback.rs` | text | text: .live_text(), .word_revise(), Revise, flow | vo-fx-text/tests/ |
| `vo-scene/tests/live_flow.rs` | text | text: .live_text(), .settling_ink(), .word_case(), .word_revise() | vo-fx-text/tests/ |
| `vo-scene/tests/magnet.rs` | text | text: .live_text(), .magnetic_caret(), Magnet, flow | vo-fx-text/tests/ |
| `vo-scene/tests/palimpsest.rs` | text | text: .live_text(), Revise, flow, motion/palimpsest | vo-fx-text/tests/ |
| `vo-scene/tests/reading.rs` | text | text: .live_text(), .reading(), flow | vo-fx-text/tests/ |
| `vo-scene/tests/revise_looks.rs` | text | text: .live_text(), .word_revise(), Revise, flow | vo-fx-text/tests/ |
| `vo-scene/tests/revision_motion.rs` | text | text: .live_text(), .revision_motion(), .word_revise(), Revise | vo-fx-text/tests/ |
| `vo-scene/tests/rewrap_gap.rs` | text | text: .live_text(), .word_revise(), Revise, flow | vo-fx-text/tests/ |
| `vo-scene/tests/stroke_write.rs` | text | text: PenParams, PenSchedule, StrokeSource, WetInk | vo-fx-text/tests/ |
| `vo-scene/tests/text.rs` | text | text: .live_text(), .word_arrival(), .word_revise(), Revise | vo-fx-text/tests/ |
| `vo-scene/tests/text_fx.rs` | text | text: .live_text(), .word_arrival() | vo-fx-text/tests/ |
| `vo-scene/tests/text_size.rs` | text | text: .live_text() | vo-fx-text/tests/ |
| `vo-scene/tests/typo.rs` | text | text: .live_text(), .word_typo(), Typo, Typos | vo-fx-text/tests/ |
| `vo-scene/tests/variable_font.rs` | text | text: .live_text() | vo-fx-text/tests/ |
| `vo-scene/tests/word_letters_together.rs` | text | text: .live_text() | vo-fx-text/tests/ |
| `vo-scene/tests/word_mark.rs` | text | text: .live_text() | vo-fx-text/tests/ |
| `vo-scene/tests/words_elsewhere.rs` | text | text: .live_text(), .word_revise(), Revise, flow | vo-fx-text/tests/ |
| `vo/examples/blur_demo.rs` | glass, text | glass: DepthStack, depth_stack; text: .live_text() | vo runner crate (stays) |
| `vo/examples/breath_demo.rs` | text | text: .live_text(), .word_breath(), Axes, Breath | vo runner crate (stays) |
| `vo/examples/centreline_demo.rs` | text | text: StrokeSource, WordOutline, WriteStyle, Writing | vo runner crate (stays) |
| `vo/examples/displace_demo.rs` | text | text: .live_text() | vo runner crate (stays) |
| `vo/examples/dots_demo.rs` | text | text: .live_text() | vo runner crate (stays) |
| `vo/examples/effect_cost.rs` | field, glass, text | field: .emitter(), Emitter, FluidLayer, InkStyle; glass: .glass(), DepthStack, depth_stack; text: .live_text(), .word_breath(), Axes, motion/var_axes | vo runner crate (stays) |
| `vo/examples/glass_demo/main.rs` | glass | glass: .glass() | vo runner crate (stays) |
| `vo/examples/glass_demo/refract.rs` | glass | glass: .glass() | vo runner crate (stays) |
| `vo/examples/glass_demo/shapes.rs` | glass | glass: .glass() | vo runner crate (stays) |
| `vo/examples/ink_demo.rs` | field, text | field: FluidLayer, InkStyle, InkWord, fluid; text: .live_text() | vo runner crate (stays) |
| `vo/examples/mark_demo.rs` | text | text: .live_text() | vo runner crate (stays) |
| `vo/examples/material_demo.rs` | text | text: .live_text() | vo runner crate (stays) |
| `vo/examples/particles_v2.rs` | field, text | field: .emitter(), Emitter, LevelTrack, emitter; text: .live_text() | vo runner crate (stays) |
| `vo/examples/sweep_demo.rs` | text | text: .final_pass(), .live_text(), .word_revise(), FinalPass | vo runner crate (stays) |
| `vo/examples/text_flow_demo.rs` | text | text: .live_text(), Arrow, Flight, curve | vo runner crate (stays) |
| `vo/examples/text_modes.rs` | text | text: .live_text(), .reading(), .word_revise(), Revise | vo runner crate (stays) |
| `vo/examples/textfx_demo.rs` | text | text: .live_text() | vo runner crate (stays) |
| `vo/examples/type_demo.rs` | text | text: .live_text(), flow | vo runner crate (stays) |
| `vo/examples/voice_particles.rs` | field, text | field: .emitter(), Emitter, RingEmitter, emitter; text: .live_text() | vo runner crate (stays) |
| `vo/examples/water_pool_demo.rs` | field, text | field: PoolDrop, WaterPool, water; text: .live_text() | vo runner crate (stays) |
| `vo/examples/wave_fx.rs` | field | field: Ribbon, ribbon | vo runner crate (stays) |
| `vo/src/envelope.rs` | text | text: .edit(), .live_source(), .live_text(), .word_revise() | vo runner crate (stays) |
| `vo/tests/envelope.rs` | text | text: .live_text(), .word_arrival() | vo runner crate (stays) |
| `vo/tests/ink_envelope.rs` | field | field: InkStyle, fluid | vo runner crate (stays) |

## Appendix: other vo crates per file

**vo** (21 files, 9290 code lines): `app.rs` 649, `bin/vo-preview.rs` 297, `diag.rs` 161, `envelope.rs` 1843, `export.rs` 268, `lib.rs` 132, `live.rs` 1574, `offline.rs` 684, `preview/chrome.rs` 358, `preview/composite.rs` 367, `preview/control.rs` 155, `preview/effects.rs` 288, `preview/mod.rs` 395, `preview/player.rs` 226, `preview/reference.rs` 317, `preview/reload.rs` 99, `preview/sheet.rs` 168, `preview/source.rs` 186, `preview/window.rs` 717, `record.rs` 103, `surfaces.rs` 303

**vo-3d** (23 files, 6915 code lines): `camera.rs` 147, `env.rs` 540, `field.rs` 12, `frame.rs` 180, `glyph_fx.rs` 931, `glyph_sdf.rs` 197, `grid.rs` 139, `item.rs` 491, `lib.rs` 43, `math.rs` 203, `mesh.rs` 285, `painter.rs` 84, `quad.rs` 175, `readback.rs` 29, `render.rs` 1168, `ribbon3d.rs` 337, `sculpture.rs` 290, `sdf.rs` 551, `shadow.rs` 124, `stack.rs` 205, `surface_text.rs` 545, `text3d.rs` 174, `timer.rs` 65

**vo-audio** (5 files, 1519 code lines): `lib.rs` 11, `mel.rs` 446, `mic.rs` 206, `rate.rs` 356, `word_axis.rs` 500

**vo-core** (16 files, 4803 code lines): `ambient.rs` 544, `channel.rs` 343, `edit.rs` 1026, `edit_keymap.rs` 191, `event_log.rs` 362, `hash.rs` 74, `key.rs` 130, `lib.rs` 15, `noise.rs` 136, `signals.rs` 783, `spring.rs` 228, `throttle.rs` 77, `time.rs` 23, `timeline.rs` 378, `track.rs` 202, `words.rs` 291

**vo-dots** (8 files, 1979 code lines): `field.rs` 338, `font.rs` 118, `font_data.rs` 399, `lib.rs` 14, `particles.rs` 195, `signals.rs` 277, `text.rs` 237, `wave.rs` 401

**vo-host** (1 files, 268 code lines): `lib.rs` 268

**vo-host-wayland** (5 files, 2295 code lines): `blur.rs` 54, `guard.rs` 102, `handlers.rs` 372, `keyboard.rs` 331, `lib.rs` 1436

**vo-sim** (7 files, 1837 code lines): `fluid.rs` 670, `fluid_render.rs` 70, `lib.rs` 133, `reference.rs` 198, `snapshot.rs` 111, `water.rs` 594, `water_render.rs` 61

