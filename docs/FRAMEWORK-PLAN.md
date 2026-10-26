# Framework plan

Decided 2026-10-26. This replaces the roadmap rule "phase 1 is a thin slice, not a UI framework".
The framework is the product. The voxtype dictation popup is its first app.

Sources: five lane reports (core, hosts, motion, prior art, API) and a Fable review. Copies are kept in
the maintainer's private notes. `ROADMAP.md` and `CAPABILITIES.md` still hold for the renderer
internals (Slug text, SDF shapes, effects, particles); this file overrides them where they differ.

## Scope

- In: desktop overlays (layer-shell style popups, notifications, OSDs, HUDs), offline motion graphics
  (the same scenes rendered to video or frames), Linux, macOS and Windows.
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
   - macOS: non-activating `NSPanel` with a `CAMetalLayer`, accessory activation policy.
   - Windows: HWND with wgpu DX12 on DirectComposition (`DxgiFromVisual`) for alpha.
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

- The `Host` trait lives in its own small crate, `vo-host`, so the macOS and Windows hosts never depend on
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
  Linux only for now (decided 2026-10-26). The two macOS and Windows checks (Slug's fragment-stage
  storage buffer on DX12 and Metal, DirectComposition alpha with click-through) move to the start of M3.
  Until then the display list avoids anything those backends are known to lack.
- **M1, core (Linux only, started 2026-10-26).** Time model, scene model, input model, the offscreen host with golden tests and a
  determinism check, the Wayland host. CI measures own heap (budget 15 MB) and cold start from the first
  commit.
- **M2, three apps on one API** before the API counts as stable: the Swiss popup (design 12), a
  notification toast, and a 10 s title card exported to video. The popup must run in daily voxtype use
  for one week behind a flag.
- **M3, macOS** host spike, then the host. One conformance scene checked on every host.
- **M4,** the other design families, particles, glass and blur, the preview and scrub tool.
- **M5, Windows** host, when a Windows app is wanted.

## Risks

1. Framework scope delays the first shipped popup. The M2 gate is the guard.
2. A `step(dt)` motion core ships in M1 and the event-time model becomes a rewrite.
3. Windows click-through may break DirectComposition alpha.
4. Slug's fragment-stage storage buffer is untested on DX12 and Metal.
5. Cold start and memory on DX12 and Metal are unknown; there is no pipeline cache there.
6. Key collisions or identity bugs reset springs.
