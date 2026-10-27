# Framework plan

Decided 2026-10-26. This replaces the roadmap rule "phase 1 is a thin slice, not a UI framework".
The framework is the product. The voxtype dictation popup is its first app.

Sources: five lane reports (core, hosts, motion, prior art, API) and a Fable review. Copies are kept in
the maintainer's private notes. `ROADMAP.md` and `CAPABILITIES.md` still hold for the renderer
internals (Slug text, SDF shapes, effects, particles); this file overrides them where they differ.

## Scope

- In: desktop overlays (layer-shell style popups, notifications, OSDs, HUDs), offline motion graphics
  (the same scenes rendered to video or frames), on Linux only. macOS and Windows are out of scope
  (decided 2026-10-26). The `Host` trait stays, so a port stays possible, but nothing is planned or
  tested for them.
- Out: full desktop apps (text fields, focus, screen readers). Node ids stay stable so accesskit can be
  added later.

## Decisions

1. **No existing framework as the base.** GPUI is the nearest relative and is read for its element model
   and layer-shell code. Makepad, Xilem, Blitz, Iced, Slint, egui, Bevy UI and Freya each fail a fixed
   decision (own platform layer, Vello, Skia, game engine, or a renderer that cannot take Slug text).
2. **Scene model: keyed view, retained nodes.** `fn view(&State, &Tokens) -> Element` is pure and runs
   only when app state changes, at the event's timestamp. The framework reconciles it by key path into a
   retained node arena. Each node owns its animated properties as channels. A value from the view is a
   target, never a final value. `scene.sample(t) -> DisplayList` is pure.
   - Channel kinds: spring (closed-form segments `{t0, x0, v0, target}`, velocity kept on retarget),
     track (keyframes on a clip's local time), expression (a function of `t`, signals, index and seed).
   - Presence (enter, present, leaving) is a node property. Leaving nodes stay until they settle.
   - Live words do not go through the general reconcile. A `LiveText` node keeps its own word-id map
     (today's `vo-core` word model) and gives per-word bounds for effects and particles.
   - Why not a view that runs every frame: it sets targets at frame boundaries, so 144 Hz live and
     60 fps export would differ; seek needs a replay; and a target could read another spring's live
     value, which has no closed form.
3. **Time is an input.** Only the host reads the clock. Every event carries a timestamp. Particles step
   at a fixed tick with counter-based seeded hashing. One event log format, versioned from the first
   commit, serves live input, the replay corpus and authored scenarios.
4. **Authoring in plain Rust plus TOML tokens.** No DSL and no layout macros for 1.0. The one macro is a
   serde-style `#[derive(Tokens)]`. The public API stays mostly non-generic to protect the about 1 s
   edit-to-test loop. Tokens and WGSL shaders reload live; a Rust change restarts and replays to the
   same `t`.
5. **Layout: taffy through its low-level traits** on our own tree. Flex and block now, grid behind a
   feature flag. Anchors at the root. Layout runs on structural change only and its output is spring
   targets.
6. **Timelines are core.** Tracks from M1. Combinators (`all`, `chain`, `sequence`, named markers) in M2.
7. **Shaders: WGSL through naga on every backend.** Precompiled SPIR-V and the Vulkan pipeline cache stay
   as a Linux-only optimisation behind a feature.
8. **Text: our own line fitter** (harfrust and unicode-linebreak), matching the Slug encoder's shaping.
   No parley for now; revisit at M4 if a design needs rich paragraph styling.
9. **Own thin hosts on raw-window-handle, not winit.** The host owns the main thread; the core runs in
   callbacks. One wgpu device per process, many cheap surfaces.
   - Wayland: sctk layer-shell, plus an undecorated, non-activating `xdg_toplevel` surface as a host
     capability for compositors without layer-shell (GNOME). No X11 host.
   - Offscreen: renders to a texture with a caller-driven clock, for goldens and export.
10. **Export:** readback ring, PNG sequence in-process, video through an `ffmpeg` child process. Export
    code sits behind a feature so the live overlay does not pay for it.
11. **Zero frames when settled.** Ambient loops are declared with a frame-rate cap.

## Input model (to write before M1)

Pointer enter, leave and click against the last sampled node bounds; hit-test regions handed to each
host for click-through; layer-shell keyboard interactivity mode; the toast dismiss action; the preview
tool's scrub controls.

## Time model notes

Rules found while building the M1 time model in `vo-core` (`spring`, `channel`, `track`, `hash`,
`event_log`).

- **Target-mode tracks ignore easing.** A target track retargets its channel at each keyframe, and the
  spring does the motion between targets. A target that moves along a curve has no closed form, so the
  keyframe's Bezier easing only applies in offset mode, where the eased value is added to the channel's
  output.
- **Retarget rule.** `Channel::retarget(t, ..)` with a `t` earlier than the latest segment drops every
  segment after `t` and continues from the state at `t`, so an earlier event rewrites the future. A
  replay editor wants that. A host bug that feeds events out of order would silently drop motion, so the
  scene must `debug_assert` that events arrive in time order.
- **Segments use `f64`.** Times, positions and velocities in a segment are `f64`, so a seek late in a long
  export stays exact. Values become `f32` when they go into the display list.
- **`pop` overshoots 4.6%.** A damping ratio of 0.7 gives 4.6% overshoot, not the 6% that
  `CAPABILITIES.md` states. `Spring::POP` keeps 0.7; a 6% overshoot would need a ratio of about 0.67.
- **Stagger.** `stagger_fit(i, n, gap, cap)` is the default: it shrinks the gap so the last item starts
  by `cap`. `stagger(i, gap, cap)` only clamps, so items past the cap start together.

## Scene and renderer notes (M1, 2026-10-26)

- **Seek.** Nodes that finish leaving are dropped, so sampling an earlier time does not bring them back.
  Export plays forward and is unaffected. The preview scrubber replays from the event log, with
  snapshots for long pieces. The live overlay keeps a short channel history (`with_retention`).
- **Group opacity.** Opacity on a direct layer is folded into each shape and glyph, which is wrong where
  children overlap. Exact group opacity needs an offscreen layer.
- **Scale and rotation** open a direct layer, because shape instances are axis aligned. Direct layers must
  stay cheap.
- **Generic over Msg.** Click and hover handlers make `Element` and `Scene` generic over the app's message
  type. Accepted as the one exception to "mostly non-generic".
- **Layout triggers** are structural, text, style and viewport changes, not structure alone.
- **Blend.** Normal, Add and Screen are exact. Multiply is exact only over an opaque destination.
- **Clips.** Only the nearest clip is rounded; outer clips act through their bounding-box scissor.
- taffy is 0.15.1, not the 0.13 the lane reports cite.

## Host notes (M1, 2026-10-26)

- The `Host` trait lives in its own small crate, `vo-host`, so another host would never depend on
  the Wayland crate. There is no separate offscreen host crate: `render_offline` uses vo-render's
  `Offscreen` directly.
- The host presents. The frame callback and the input region must be set before the commit that present
  makes, so the host calls the app's frame function with the texture and presents afterwards.
- Wayland's u32 millisecond timestamps have no fixed base and wrap. Events are stamped with the host
  clock when they arrive. Frame time is the next vblank, predicted from `wp_presentation` feedback.
- App timers (`Cx::after`) exist because `next_wake` only covers motion. Offline, timers merge with
  scripted input in time order.
- Measured on NVIDIA in a nested KWin: cold first frame about 200 to 220 ms, about 60 % of it creating
  the Vulkan instance. Own heap 1.35 MB. PSS about 70 MB, almost all driver. Zero frames while idle. So
  the own-heap budget is easy, and the CPU-drawn first frame is still worth building.
- **CPU first frame** (`Options::cpu_first_frame`, off by default; `toast --cpu-first-frame`). A thread
  makes the instance, adapter, device and the first swapchain while the main thread draws CPU frames
  (`vo_render::cpu`) into `wl_shm` buffers on the role surface. The GPU draws into a sub-surface. It
  starts synchronized, so the first GPU frame and a clear buffer on the role surface land in one
  commit (checked with `WAYLAND_DEBUG`: the NVIDIA WSI commits inside `vkQueuePresentKHR`, before the
  host's commit). CPU shapes run the shape shader's maths and match the GPU to 2/255; text is
  tiny-skia fills of unhinted outlines, up to 90/255 apart on single edge pixels, 0.85/255 on average.
- Measured in a nested KWin on NVIDIA, 10 cold runs per mode: first pixels on screen 14 to 23 ms since
  exec (mean of two batches) against 174 to 202 ms without; the first GPU frame comes 10 to 30 ms
  later than without. Three causes: the first acquire on the sub-surface's swapchain takes about
  11 ms (about 0 on the role surface), the synchronized hand-over shows one refresh later than a
  desynchronized one would, and the last CPU frame stays on screen meanwhile, so the entrance motion
  holds for 2 to 3 refreshes. The overlay is click-through until the first GPU frame, because the input
  region lives on the GPU's sub-surface.
- Against a nested KWin started fresh for each run (`just budget`), the CPU first frame shows at 63 to
  73 ms, because a new compositor answers the first configure more slowly. The GPU path there is 200
  to 216 ms.

## Timeline and export notes (M2, 2026-10-26)

- **Timeline API** (`vo_core::timeline`). Build a tree with `clip(name, dur)`, `marker(name)`,
  `wait(secs)`, `all([..])`, `chain([..])`, `sequence(gap, [..])`, `stagger(gap, cap, [..])` and
  `delay(by, item)`. `stagger` uses `stagger_fit`, so the last item starts by `cap`. `sequence` ends
  when its longest-running item ends, not when the last one starts.
- **Schedule.** `Timeline::schedule()` lays the tree out once into a flat table of named spans on
  clip-local time. `progress`, `eased`, `spring`, `active` and `local` are closed form in `t`, so
  frames can be sampled in any order. Two clips or two markers with one name panic, like an empty
  `Track`.
- **Bridge to the scene.** The scene has no expression channels yet, so `Schedule::cues(start)`
  lists clip starts, ends and markers in time order, ends before markers before starts at equal
  times. An app turns them into timers in `init` and moves each part between states, and the
  scene's springs do the motion. `Schedule::spring` is the closed form for when expression channels
  land.
- **Export** (`vo::export`, feature `export`). Exactly `round(duration * fps)` frames at `n / fps`,
  whether or not the app exits. The readback ring feeds a writer thread that hashes every frame,
  pipes raw RGBA to an `ffmpeg` child and hands PNGs to up to 8 encoder threads. ffmpeg is checked
  before the first frame, and a missing one gives an error that names the fix. `VO_FFMPEG`
  overrides the program. Premultiplied frames with the alpha dropped are the same as compositing
  over black.
- **Reproducible files.** `Video::h264` passes `-map_metadata -1` and the bitexact flags, so two
  renders give byte-identical mp4 files, not only identical frames.
- **Measured** (title card, 1920x1080, 600 frames, release, NVIDIA desktop): 4.1 to 5.0 s with
  H.264 (about 120 to 150 fps), 0.45 MB file, peak RSS 323 to 360 MB for the render process. Frames
  only, no video: 0.9 s. With PNGs as well: 11.3 s, peak RSS 511 MB, 29 MB of PNGs. One PNG writer
  thread took 85 s, because unpremultiply and deflate cost about 140 ms per 1080p frame.

## Effects notes (M4, 2026-10-26)

The maintainer pulled the effects work forward on 2026-10-26, to run in parallel with M2. It was planned
after M3, the macOS host, which has since been dropped.

- **Two pass lists, chosen per frame.** A frame with no layer effect takes the direct path, as before:
  everything draws into the target. A frame with any bloom layer takes the effect path: the scene and
  every offscreen layer draw into `Rgba16Float` textures, each bloom layer runs its pyramid, and a last
  pass tone maps, encodes and dithers into the target. The toast and the Swiss popup never take the
  effect path, so their goldens and the budget are unchanged.
- **Offscreen layers are sized to what they draw**, plus the bloom's reach (3 times 2 to the power of
  the level count, in pixels), rounded up to 64 pixels, at a whole-pixel offset, from a texture pool
  that frees a texture after 120 unused frames. Exact group opacity (`.offscreen()`) now costs a texture
  the size of the group, not of the target.
- **Scene API.** `Element::glow(colour, radius, strength)` and `Element::shadow(colour, dx, dy, blur)` on
  anything that draws a shape; `Element::bloom(strength, radius)` on a layer, which makes it offscreen.
  All are spring channels (`Prop::Glow`, `Prop::Shadow`, `Prop::Bloom`). Text glows through a bloom
  layer around it, because glyphs have coverage but no distance.
- **Display list.** Glow and shadow are fill paints (`PaintKind::Glow`, `PaintKind::Shadow`) on their own
  shape instances, drawn under the shape, so plain shapes pay nothing. Bloom is
  `LayerTarget::Offscreen { bloom: Some(Bloom) }`.
- **Glow** is `strength * exp(-d / radius)` of the shape's distance, in the shape shader, with no blur
  pass. Strength above 1 is light above 1 on the effect path; the direct path caps it at 1 so the hue
  holds. Around a stroke it falls off on both sides of the stroke.
- **Drop shadow** is the closed-form Gaussian of a rounded box (exact in x, four samples in y). Lines and
  bands use the error function of their distance instead.
- **Bloom** is the Call of Duty: Advanced Warfare pyramid: 13-tap downsample, Karis average on the first
  level, 3x3 tent upsample, each level mixed 0.65 with the one below. The level count follows the radius
  (level 1 spreads about 2 pixels, each level doubles), and the deepest level fades in, so a radius on a
  spring changes smoothly. The bloom is added on top of the layer, premultiplied, with alpha saturated,
  so the halo shows over a transparent desktop. Nothing is unpremultiplied in the pyramid.
- **Tone map.** A shoulder above 0.95 on the brightest straight channel brings light above 1 back under
  1 with the hue kept. A pure white becomes 0.982. Then sRGB encode and a dither of one code value from
  the pixel position only, so frames do not change between runs.
- **Subpixel motion.** The pyramid's grid is fixed to the texture, so it is not exactly translation
  invariant: a dot moved across a pixel in eighths changes its halo's light by up to 2.3% and moves its
  centroid within 0.06 px of the dot.
- **Determinism.** Two renderers give the same bytes for every effect golden, and two exports of
  `glow_demo` give the same 360 frame hashes.
- **Measured** (`glow_demo -- --bench`, NVIDIA desktop, timestamp queries, median of 200 frames): at
  600x300 the effect path takes 0.047 ms of GPU time against 0.015 ms for the same scene drawn
  without effects; at 1920x1080, 0.212 ms against 0.081 ms. Pooled textures: 1.8 MB at 600x300 and
  20.2 MB at 1920x1080, most of it the scene texture. Cold first frame of a 600x300 effect scene, from
  process start to readback, 124 to 132 ms against 123 to 129 ms for the direct scene, so the extra
  pipelines cost a few ms.
- **Quality tiers and reduced motion (MO09).** `Quality::Full`, `NoBloom` and `Minimal`, from
  `VO_QUALITY` or `Renderer::set_quality`. `NoBloom` leaves bloom and trails out, so every frame takes
  the direct path; `Minimal` also leaves out glows and shadows. When an effect texture cannot be
  allocated (an out-of-memory error scope), the renderer drops one tier by itself and draws the frame
  again. Reduced motion (`VO_REDUCED_MOTION`, `Scene::with_reduced_motion`) makes position, size, offset,
  scale and rotation jump, moves glow, bloom and shadow on a slow spring so nothing flashes, keeps only
  the opacity of enter and exit transitions, and stops orbiting rims and sheens.
- **Light mode (FX14).** `Renderer::set_light(true)` (or `VO_LIGHT`) turns a layer's bloom into a soft
  dark shadow, since added light does not show on a light background. Alpha-blended particles in light
  mode belong to the particle lane; dark-on-light text tuning is not done.
- **Motion blur (FX12).** `Element::motion_blur(shutter)` blurs a node along the distance its spring moved
  during the shutter time. The node gets a layer of its own, and the shape and glyph shaders average
  one sample per pixel of smear, 4 to 32, so thin text does not show as copies. At rest there is no blur
  and no extra layer.
- **Feedback trails (FX11).** `Element::trail(half_life)` keeps what a layer drew in earlier frames and
  fades it to half every `half_life` seconds, independent of frame rate. The trail texture covers the
  target and stays in the pool under the node's key. Each frame a blend-constant pass fades it and the
  layer draws on top. A seek backward, a gap over 0.5 s or a new size starts it fresh, so exports stay
  deterministic. The scene stays awake 12 half lives after the last change, until the tail is under one
  8-bit level. Trails need `Full` quality.
- **Rim light and sheen (FX08).** Two shape paints: `Conic`, a soft band of colour around one direction,
  and `Sheen`, a soft straight band. `Element::rim(colour, width, span, angle, period)` draws a conic
  stroke over the outline: a still specular edge, or a segment that orbits the border once per
  `period`, stepped at 30 fps so the scene wakes 30 times a second. `Element::sheen(colour, secs)`
  sweeps a slanted band across the shape once as it enters. The glass material has its own sheen and
  orbit inside the glass shader.
- **Edge fades and digit roll (FX04).** `Element::fade_edges(left, top, right, bottom)` clips a layer and
  fades its content out over those widths, in the same clip test the shaders already run, at no extra
  pass. `roll::digit_roll` builds a number whose changed digits roll: each digit is a clipped, faded
  window over a column of the ten digits on a spring. The column is keyed by the text to its left, so
  a carry rolls a new column in from below while the old one leaves upward, with no history in the
  view. R8 mask textures for arbitrary shapes are not done.
- **Text effects (TY17).** Blur, dissolve, wipe, chromatic offset and halo come from the glyph's own
  Slug coverage in the glyph shader, with no offscreen pass. Blur and halo average 12 to 24 coverage
  taps on a disc, each with a wider antialiasing window; taps outside the ink box are dropped, since
  Slug coverage is not valid there. Dissolve thresholds a value noise fixed to the glyph's em space;
  wipe hides the glyph from its right edge; chroma moves the red and blue coverage apart. The display
  list carries the effects sparsely (`glyph_fx`, by glyph index), so text without effects costs nothing
  and keeps Slug's own path. Labels and live text take `text_blur`, `text_dissolve`, `text_wipe`,
  `text_chroma` and `text_halo` on `Prop::TextFx`; a label's wipe is one front along each line.
  `Transition` gains `blur`, `dissolve` and `wipe`, so live-text words blur in, form out of noise or
  wipe in as they arrive. The CPU first frame draws text without these effects.
- **Demos.** `glow_demo` (glow, bloom, shadow), `comet_demo` (trail, motion blur, rim, sheen),
  `ticker_demo` (edge fades, digit roll) and `textfx_demo` (text effects) each write an mp4 and a
  contact sheet, and `--check` renders twice and compares all 360 frame hashes.
- **Not done.** The CPU first frame skips glows, shadows, bloom and text effects; it draws conic, sheen
  and edge-fade paints. The noise library (FX10), R8 masks and the style pack (FX13) are next.
## Crates

| Crate | Contents |
|---|---|
| `vo-core` | maths, clock, channels, tracks, signals, seeded noise, tokens, `LiveText`, event log. No GPU |
| `vo-scene` | elements, reconcile, nodes, taffy, shaping, line fitting, display list |
| `vo-render` | wgpu, passes, Slug, hinted text, SDF shapes, effects, particles. No windowing |
| `vo-host` | the Host trait and its types |
| `vo-host-wayland` (later `vo-host-appkit`, `vo-host-win32`) | one per platform, behind features |
| `vo` | facade: `run_overlay`, `render_offline`, and the preview, bench and contact-sheet tools |

The popup app and `osd-protocol` live in voxtype.

## Milestones

- **M0, measure.** Finish phase 0 (wgpu trial, cold start, alpha, pacing, blur, CPU first frame).
  Linux only (decided 2026-10-26).
  Until then the display list avoids anything those backends are known to lack.
- **M1, core (Linux only, started 2026-10-26).** Time model, scene model, input model, the offscreen host with golden tests and a
  determinism check, the Wayland host. CI measures own heap (budget 15 MB) and cold start from the first
  commit.
- **M2, three apps on one API** before the API counts as stable: the Swiss popup (design 12), a
  notification toast, and a 10 s title card exported to video. The popup must run in daily voxtype use
  for one week behind a flag.
- **M3, removed.** It was the macOS host; macOS and Windows are out of scope.
- **M4,** started 2026-10-26 in parallel with M2: the other design families, particles, glass and blur, the preview and scrub tool.
- **M5, removed.** It was the Windows host.

## Risks

1. Framework scope delays the first shipped popup. The M2 gate is the guard.
2. A `step(dt)` motion core ships in M1 and the event-time model becomes a rewrite.
3. Key collisions or identity bugs reset springs.
