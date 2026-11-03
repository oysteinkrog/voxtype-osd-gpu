# Decision 0001: how vo-scene and vo-render split into crates

- Status: proposed (V-01). V-P102 reviews it with Astra and Fable and amends this record.
- Date: 2026-11-03
- Requirements: R39 (vo stands alone as the framework), R48 (decisions are written down).
- Evidence: [../restructure-survey.md](../restructure-survey.md), surveyed at vo commit `5f6606a`.

## Context

`vo-scene` is 73 source files and about 31,000 code lines (35,000 lines with comments). It
holds the scene core, text, the display list and every effect. `vo-render` (about 11,800 code
lines) depends on it for the display list types. Plan section 2.6 splits vo into `vo-display`,
`vo-text`, `vo-svg`, a smaller `vo-scene` core and three effect crates (`vo-fx-text`,
`vo-fx-field`, `vo-fx-glass`), so a vo user builds only what they use and `vo-render` stops
depending on the scene.

## Decision

1. **vo-display** (about 6,300 code lines) gets the display list, paint, geometry, vector
   paths, materials, the stage camera, GPU items, image decoding and caching, the small SVG
   icon parser, the embedded assets, and the renderer-facing data types of glass, particle
   emitters, frames, displacements and liquid glyphs. Rule: a type that `vo-render` reads goes
   to `vo-display`, whatever module it sits in today. It depends on `vo-core` only.
2. **vo-text** (about 2,700) gets fonts, shaping and the line fitter (`font.rs`, `text.rs`),
   inline objects (`inline.rs`), `TextStyle` from `element.rs`, and the glyph outline
   functions from `path.rs`. It depends on `vo-core` and `vo-display`.
3. **vo-svg** gets the resvg call from `image.rs` (about 25 lines). `vo-display` decodes PNG
   itself and calls an SVG decoder hook that `vo-svg` fills, because `vo-svg` depends on
   `vo-display` (it returns a `Raster`).
4. **vo-scene core** (about 7,000) keeps the keyed view and `Element`, reconcile, layout,
   input, focus, inspection, signal binding, reach, canvas painting, repeat, timing and the
   ambient loops. It also keeps 16 widget modules (about 3,300 lines: bars, meters, frames,
   graticule, spectrum, status icon and others) that fit no crate in plan 3.2.
5. **vo-fx-text** (about 9,000) gets `flow.rs` with all of `motion/`, stroke writing, glyph
   morph, centre lines, text on curves, typography channels, word voice, the final reading
   pass and aged lines.
6. **vo-fx-field** (about 2,200) gets the emitter element and signals, lattice, ribbon, water
   and fluid. The simulation stays in `vo-sim`.
7. **vo-fx-glass** (about 800) gets the glass element half, the depth stack and the scene
   half of displacement.
8. **vo-render** keeps all its files. Its dependency on `vo-scene` becomes `vo-display` plus
   `vo-text`. Its GPU tests may dev-depend on `vo-scene` and `vo-fx-*`; none of those depend
   on `vo-render`, so that builds one copy of each crate.

## Why

- The renderer must not reach the scene. Many effect modules define the data the renderer
  draws (`GlassInstance`, `EmitterSim`, `Material`, `Warp`, `GpuItem`, `LiquidRevision`). That
  data has to sit below the renderer, so it goes to `vo-display` and only the element side of
  each effect moves to its `vo-fx-*` crate. Moving whole files, as the plan's file lists
  suggest, would make `vo-render` depend on the effect crates and through them on `vo-scene`.
- `flow.rs` belongs with text. It is the live text engine and the parent of the ten `motion/`
  modules, which reach its private items through `super::`. Keeping the family in one crate
  keeps that private access legal. With `flow.rs` in core instead, the core would call into
  the moved `motion/` modules in about 55 places (26 found by the path scan plus 29 short
  `liquid::`, `revisions::` style paths in `flow.rs`), against 40 with `flow.rs` in
  `vo-fx-text`. Plan 3.2 and V-08 list `flow` as a field effect; that looks like a name clash
  with flow fields, and this record moves it.
- Widgets stay in `vo-scene` because the plan names no crate for them and moving them buys
  nothing for the renderer split. A later `vo-widgets` crate is easy because widgets depend on
  core only.
- After these moves the target graph has no cycle, provided the core stops naming effect items.

## Consequences

- **V-06 has more work than the 4a count shows.** Seven effect modules use crate-private items
  (under the limit of 10, so V-P105 is not needed by the rule). But the core names 40 effect
  items directly: `Kind::LiveText` holds flow and motion types, `Element` has builder methods
  for each text effect and the emitter, `scene.rs` draws live text, typography, the final pass
  and emitters, and `layout.rs` asks flow for spans and breath. All 40 must go through the
  effect API. V-06 should treat that inversion as its main job and may still want the V-P105
  split for it.
- **Live text becomes an effect-backed element.** `Element::live_text` stays in core, but what
  draws and moves the words lives in `vo-fx-text`. So 26 of the 38 vo-scene integration tests
  move to `vo-fx-text`, and vo-scene's own tests cover only the core and widgets.
- **`ribbon.rs` needs an extension trait** (or free constructor) in V-08: it adds builder
  methods with `impl<M> Element<M>`, which only the crate that defines `Element` may do.
- **`vo-fx-glass` is small.** About 800 code lines. V-P102 may decide to fold it into
  `vo-fx-field` or keep it for the public API shape.
- Bead texts that list files (V-07 to V-09, plan 3.2) should be read against the survey table,
  which is the file list of record.

## Open questions for V-P102

1. Widgets: stay in `vo-scene`, or a `vo-widgets` crate now?
2. `flow.rs`: `vo-fx-text` (this record) or core?
3. `assets`: `vo-display` (this record) or `vo-core`?
4. Keep `vo-fx-glass` as its own crate at about 800 lines?
5. Effect API shape for the 40 inverted items, in particular how `Kind::LiveText` hands its
   motion and drawing to `vo-fx-text` without a cycle.

## Limits of the survey

The edges and the crate-private count come from a text scan of `crate::` and `vo_scene::` paths
(tools in `tools/restructure-survey/`), not from the compiler. Unqualified calls between items
of one split file were checked separately for the split files. Private items that core or
widgets use from modules going to `vo-text` or `vo-display` (for example `font::Fnv`) were not
counted; V-03 and V-04 find those at compile time.
