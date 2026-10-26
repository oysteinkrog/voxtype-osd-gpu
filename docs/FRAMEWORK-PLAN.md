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

## Crates

| Crate | Contents |
|---|---|
| `vo-core` | maths, clock, channels, tracks, signals, seeded noise, tokens, `LiveText`, event log. No GPU |
| `vo-scene` | elements, reconcile, nodes, taffy, shaping, line fitting, display list |
| `vo-render` | wgpu, passes, Slug, hinted text, SDF shapes, effects, particles. No windowing |
| `vo-host-wayland`, `vo-host-appkit`, `vo-host-win32`, `vo-host-offscreen` | one per platform, behind features |
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
