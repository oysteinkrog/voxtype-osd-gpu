# Voxtype Overlay Renderer Roadmap

Date: 2026-10-26. Author: the planning swarm, for Oystein. Status: proposal, nothing built. This version includes the Fable engineering review (`review-fable.md`) and the completeness review (`review-completeness.md`). Section 10 lists the review points that were not taken, and why.

Inputs: `CAPABILITIES.md` (capability ids such as TY03 or PA03 are used below), the six research files and the six mockup files in `/var/tmp/claude/renderer-plan/`, the voxtype source in `~/work/voxtype`, and the spike source in `~/work/voxtype-osd-spike`. The spike has no `REPORT.md` yet and its `logs/` folder is empty, so this plan uses no spike numbers. Every target below that depends on a measurement is marked so.

Labels: **[V]** verified, with a source. **[I]** inferred from verified facts or normal practice. **[G]** guessed, needs a measurement or a decision. All effort and calendar figures are [G].

## 1. Summary

1. **Build our own renderer in phases, each one shippable.** Phase 1 ships one design family (Swiss 12) at better quality than the GTK overlay, at the target latency and memory. Every later phase adds designs without rewriting phase 1 code. [I]
2. **Phase 1 is a thin slice, not a UI framework.** Designs are Rust functions that return a display list. Colours, spring presets, sizes and placement come from a token file. Variants are token files or flags, not separate designs. A shared component layer and a design file format wait until phase 2, when five real designs exist to abstract from. [I, `review-fable.md` section 3.1]
3. **Text is the core.** Anything that moves, scales or is over about 14 px is drawn analytically from curves (the Slug method). Small static labels (9 to 12 px) are hinted through skrifa and drawn from a tiny cache texture, because Slug does no hinting. Lengyel put the Slug patent in the public domain on 2026-03-17 and published reference shaders. [V, https://terathon.com/blog/decade-slug.html; V for skrifa's hinters, https://github.com/googlefonts/fontations/blob/main/skrifa/README.md]
4. **Adopt the lower layers, build the visible layers.** Adopt wgpu (Vulkan), smithay-client-toolkit, skrifa, harfrust, kurbo, Tracy. Build the glyph renderer, SDF shapes, the pass list, effects, particles, motion core and word diff. Do not adopt Vello, Rive, Skia or a game engine as the base. [I, reasons in section 6]
5. **One process per recording, and the first frame may not need the GPU.** A resident Vulkan client costs about 90 MB PSS on this driver, and the cold first frame was 160 to 207 ms (both reported in the task brief from earlier session measurements; no log file was found, so phase 0 measures them again). Phase 0 tests a CPU-drawn first frame over `wl_shm` while Vulkan starts behind it. If that works, keep-warm becomes a memory choice, not a latency need. [G until measured]
6. **It plugs in as a fourth overlay frontend.** The launcher `voxtype-osd` already picks between `voxtype-osd-gtk4`, `voxtype-osd-native` and Quickshell at runtime. [V, `~/work/voxtype/Cargo.toml` lines 285 to 306] The new renderer becomes `voxtype-osd-gpu`, opt-in at first, and the GTK overlay stays as the automatic fallback.
7. **The daemon must send more, and more truthful, data.** A versioned socket protocol carries text revisions, the final text as a separate kind, the real daemon states (model loading, streaming, post-process, cancel, no speech, clipboard-only output), progress and paste result. [V for the states, `~/work/voxtype/src/state.rs`]
8. **Accessibility and failure handling are phase 1 work.** Reduced motion, a flash cap, computed contrast ratios, a non-colour cue for every state, a hide-transcript mode, and a tested response for every failure, starting with "never paint an opaque rectangle". [I]
9. **Order of designs:** 12 first (no shader effects, hardest typography), then 21, 22, 17 and 03 in phase 2, particle words (09) in phase 3, glass in phase 4, typography beyond the mockups in phase 5, 3D in phase 6. The particle formation look is prototyped in phase 0, because it is the highest visual risk. [I]
10. **Size.** Phases 0 to 4 add up to 24 to 34 weeks of work. With review gaps, office runs and about 20 % slack that is 7 to 10 months of calendar time. Phase 1 alone is 6 to 9 weeks after phase 0. [G]

## 2. Vision

The overlay should look like a piece of precision instrument design, not like a desktop widget. Concretely:

- **Text as good as print.** Correct shaping, kerning and ligatures, tabular figures, tracked caps, linear-light blending. Moving text never shimmers or changes weight. Small labels are hinted and crisp. Settled text sits on whole pixels. [I]
- **Words have identity.** The recogniser rewrites its hypothesis several times a second. Each word keeps a stable id across revisions, so only the changed word animates, and nothing jumps. When the final text replaces the live text at the end, words that match keep their place and only real changes crossfade. [V for the revision rate: 100 to 200 ms, `mockups-v21-22.md` part 3]
- **Everything moves on interruptible physics.** Springs keep their velocity when the target changes, so a revision in the middle of an animation never causes a kink. [I, `research-animation.md` section 2.1]
- **Effects are layers on top of a correct base.** Bloom, glass, particles and later 3D sit on a linear premultiplied pipeline, so they compose without halos, banding or brightness errors. Designs without effects skip the extra passes. [I]
- **It costs almost nothing.** Zero work when hidden, zero frames when nothing moves, a small fraction of one core while the CPU is busy transcribing. [I]
- **It cannot hurt dictation, and it cannot get in the way.** The overlay is a separate process. If it crashes, hangs or runs out of memory, dictation and paste still work. It never leaves a stuck or opaque rectangle on screen. [I, matches `research-platform.md` section 9]
- **It works for people who cannot rely on motion or colour.** Every state has a cue that is not colour alone, motion can be reduced, and text meets a stated contrast ratio. [I]

## 3. Targets

These are the bars each phase gate checks. "Phase gate" means the phase cannot close unless the target holds on the gate GPU (RTX 3060 Ti at home). The office GPU (RTX 5070 Ti) runs the same scripted bench as a smoke run within a week of each gate, through a script Oystein starts or over ssh, and its log is attached to the gate bead. All numbers are [G] until phase 0 measures the baseline. If phase 0 shows a target is impossible on this driver, the target changes by an explicit decision, not by drift.

Refresh rates in scope: 60, 75, 120 and 144 Hz, plus VRR. The home monitor runs at 75 Hz through an EDID override [V, memory note `desktop-monitor-overclock.md`]. Frame budgets below are given at 120 Hz and scale with the refresh period.

### 3.1 Latency

All "presented" times are the `wp_presentation` presented timestamp. That is the compositor's present time, not light leaving the panel. One camera or photodiode run in phase 0 measures the gap once.

| Metric | Target | Stretch | How it is measured |
|---|---|---|---|
| Hotkey to first visible frame, cold spawn | 150 ms | 100 ms | The `voxtype record start` CLI process stamps `CLOCK_MONOTONIC` at its own start and passes it through the daemon to the overlay. The time from the key press to the CLI start (compositor spawn) is measured once with `evtest` timestamps and reported as a fixed offset. The overlay logs each startup stage (connect, configure, shm frame, device, pipelines, first GPU submit) and the presented time of frame 1. Report p50 and p95 over 50 runs. "Cold" means process start with a warm pipeline cache. A separate run with an empty cache is reported, with a ceiling of 300 ms. |
| Hotkey to first visible frame, warm process | 30 ms | 17 ms (2 frames at 120 Hz) | Same log. Warm means the process is alive with no surface. |
| First frame content | Panel shape and status text visible on frame 1, CPU or GPU drawn | Same frame drawn by the GPU | Golden image of frame 1. Pipelines that frame 1 does not need (bloom, particles, glass) compile after it. |
| Audio level to presented | At most one refresh period after the playout delay the design chose | Same | Level frame `seq` and receive time logged, matched to the presented time of the frame that used it. The playout delay is a design token (`level.playout_ms`, default 50 ms as in the GTK overlay [V, `~/work/voxtype/src/osd/ipc.rs` lines 25 to 30]). Reported as presented time minus sample time minus playout delay. |
| Text revision to presented | 1 frame after receipt | same | Receive time of a `Text` message against the presented time of the first frame that shows it. |

### 3.2 Frame time

Surface size for these numbers: the phase 1 card surface, about 1400 x 900 logical pixels at scale 1.5, so about 2100 x 1350 physical pixels. [V for the size guess, `mockups-v03-09.md` observation 3] A different surface size needs a new number.

| Metric | Target | How it is measured |
|---|---|---|
| GPU time per frame, phase 1 designs | p99 under 1.0 ms | wgpu timestamp queries per pass, `bench` subcommand, 30 s scripted scenario. |
| GPU time per frame, phase 2 designs with bloom | p99 under 2.0 ms | Same. |
| GPU time per frame, heaviest design (09-rich or a glass variant) | p99 under 3.0 ms | Same. |
| CPU time per frame on the render thread | p99 under 1.5 ms | Tracy zones and the bench report. |
| Dropped frames | under 0.5 % over 30 s at every refresh rate in scope | Fixed refresh: intervals over 1.5 refresh periods in the `wp_presentation` log. VRR: intervals over 1.5 times the median interval while animating. |
| CPU use while animating | gate under 5 % of one core, stretch under 3 % | `/proc/self/stat` deltas over the bench run, while a CPU transcription load runs. |
| GPU contention | GPU transcription time grows under 3 % with the overlay animating | Bench with a GPU backend (Vulkan Whisper or CUDA Parakeet) transcribing a fixed file, with and without the overlay. |

### 3.3 Memory

| State | Target | How it is measured |
|---|---|---|
| Visible, phase 1 | PSS under 130 MB, of which our own heap under 15 MB | `/proc/self/smaps_rollup` sampled at fixed points (after first frame, after 30 s, after hide). Heap by a counting allocator in bench builds. |
| Visible, heaviest design | PSS under 160 MB | Same. |
| Hidden and kept warm | Measured and reported, expected 80 to 90 MB [G, `review-fable.md` 5.6] | Same, after hide. The keep-warm default is a decision on that number. |
| Idle in spawn-per-recording mode | 0 (no process) | `pgrep`. |
| GPU memory owned by us | under 32 MB at scale 1.5, reported at scale 2 | wgpu allocator report plus a texture and buffer ledger. At scale 2 a 1400 x 900 surface is about 10 MB per swapchain image [I, arithmetic], so the ledger reports swapchain memory on its own line. |
| Growth over 100 recordings in a warm process | under 2 MB | Bench loop. |

### 3.4 Text quality bar

| Check | Target | How it is measured |
|---|---|---|
| Coverage accuracy | No regression from the error number phase 0 measures for the port, plus a visual check | Phase 0 measures mean and max error of the Slug port against a 256-sample supersampled reference over a fixed glyph set (Latin, Norwegian letters, digits, punctuation, the bundled fonts) at 14, 24, 56 and 120 px, scales 1, 1.25, 1.5, 2, rotations 0, 7 and 45 degrees. That number becomes the gate. No absolute number is set before the port exists, because Slug's two-axis ray cast is not an exact area integral. [I, `review-fable.md` section 6] |
| Small static text | Labels at 9, 10, 11 and 12 px are drawn by the hinted path and pass a side-by-side with the same font in KDE at the same size | Golden plus Oystein's eye on a contact sheet at scale 1 and 1.5. |
| No shimmer in motion | Summed coverage per glyph varies under 0.5 % while moving in 0.1 px steps | Sub-pixel translation sweep test. |
| No weight change in motion | Same stem width in settled and moving frames | Same sweep. |
| Settled text is pixel-snapped | Baselines on whole physical pixels after the spring settles, with no visible jump. Font size chosen so `size * capHeight` is a whole number of pixels at the design's main sizes. | Golden at the settle frame, plus a frame sweep across the snap fade. [V for the cap-height rule, https://github.com/EricLengyel/Slug README] |
| Hairlines | Exactly 1 physical pixel at 1x, 1.1x, 1.25x, 1.5x, 1.75x, 2x, 2.25x | Golden of the 12 card border and rules, checked as a test on the final buffer: one row of full coverage, no partial neighbours. |
| Final text swap | Words unchanged between the last live hypothesis and the final text do not move sideways | Replay corpus with recorded final texts. |
| Side-by-side check | Oystein prefers the new 12 over the GTK overlay on both displays | A fixed set of 12 screenshots (same scenarios, same times) rendered on a private display, so the review can be repeated. |

### 3.5 Accessibility

| Check | Target | How it is measured |
|---|---|---|
| Text contrast | Body text at least 4.5:1 against the panel fill, text 24 px and over at least 3:1, in every variant including `light`, `warm`, `cool`, `green` and `glass`. Tentative words keep 4.5:1, which sets the dim floor per theme. | Computed in the golden suite from the token colours, with glass checked against both a pure white and a pure black backdrop. |
| Flashing | No more than 3 flashes in any 1 s, flashed areas kept small, following WCAG 2.3.1 [V, https://www.w3.org/WAI/WCAG22/Understanding/three-flashes-or-below-threshold.html] | Luminance trace of the replay corpus per design. |
| Reduced motion | Every state change still has a visible cue; travel, scale, blur and particles removed or replaced by short fades | Golden set rendered with reduced motion on. |
| Non-colour cue | Every phase is told apart by an icon or a word, not only by hue | Golden check plus a colour-vision simulation (deuteranopia and protanopia) of the `green` variants. |

### 3.6 Power and idle

| State | Target | How it is measured |
|---|---|---|
| Hidden, overlay process | 0 wakeups other than socket and Wayland events | `strace -c -f` or `perf trace` over 60 s, count of `epoll_wait` returns. The daemon's watchdog timer is counted separately. |
| Visible and static (pasted text on screen) | 0 frames submitted | Frame counter in the bench log. |
| Ambient loops (rim orbit, idle breath) | capped at 30 fps | Frame counter. |
| Listening with active animation | GPU power at most 3 W above the desktop baseline, no forced high P-state after hide | `nvidia-smi --query-gpu=power.draw,pstate` at 10 Hz during the bench, compared with no overlay. |
| Emitters idle | Frame loop stops within 1 s after the last particle emitter goes idle | Frame counter after the last impulse in a scripted scenario. |

### 3.7 Size and robustness

| Target | How it is measured |
|---|---|
| Stripped binary under 15 MB, fewer than 40 pipelines | `size` and a pipeline count from the pipeline cache report, both checked in CI. [G, `research-platform.md` section 13] |
| Overlay crash, hang or device loss never stops dictation or paste. Restart within 1 s, at most 3 restarts per minute, then fall back to the GTK overlay. | Fault injection test: kill the child, send SIGSTOP, force a device loss through a debug flag. Supervisor log counts restarts and fallbacks. |
| Never an opaque rectangle. If the surface cannot be composited with alpha, the overlay exits and the GTK overlay takes over. | Startup check of the offered alpha mode, plus a fault test that forces `Opaque`. |
| No stuck overlay. If the daemon goes away while the overlay is visible, the overlay fades out within 2 s and exits. | Fault test: kill the daemon mid-recording. |
| Version mismatch between daemon and overlay fails cleanly with a log line and a fallback, never a crash. | Test that pairs an overlay built with protocol version N and a daemon with N+1, both ways. |
| Daily use: no fallback caused by a crash in one week | Counter in the supervisor log, printed by `voxtype-osd-gpu --diagnose`. |

## 4. Architecture

### 4.1 Process model

```
voxtype daemon (no GPU code)
  ├─ audio, recogniser, paste, state
  ├─ osd.sock server  ──────────────┐   versioned binary protocol
  ├─ audio.sock (exists, 100 Hz)  ──┤   (section 4.10)
  └─ supervisor (exists)            │
        spawns / watches            ▼
voxtype-osd-gpu (one process)
  main thread: calloop loop: Wayland fd, osd.sock, audio.sock, timers
  first frame: CPU draw into wl_shm on its own surface (if Q9 passes)
  init thread (cold start only): Vulkan instance, device, pipeline cache
  render step on frame callback: signals -> springs -> instances -> passes -> present
```

- **Spawn on recording start, keep warm on request.** The overlay starts when recording starts. With `keep_warm_secs > 0` it drops its surface and swapchain on hide and waits, blocked on its sockets. Default 0. Configurable by config file, CLI flag and `VOXTYPE_*` env var, per project principle 5. [I]
- **CPU first frame (if phase 0 Q9 passes).** The CPU draws the first frame of the panel (shape, hairline, status line) into a `wl_shm` buffer on a second surface. Meanwhile the init thread creates the Vulkan device. When the first GPU frame is presented, the two crossfade over one or two frames and the shm surface is unmapped. Two surfaces, because switching one surface from shm to a Vulkan swapchain is not safe to rely on with this driver. [I, `review-fable.md` section 5.1]
- **Pre-warm after driver updates.** The daemon runs `voxtype-osd-gpu --prewarm` once at daemon start. It creates the device, fills the disk pipeline cache and exits. [I]
- **Parallel cold start.** Wayland connect and layer-surface configure run on the main thread while the init thread creates the Vulkan device. The pre-baked font blob is mmapped, not parsed, and carries a version and checksum. [I, `research-platform.md` section 5]
- **Adapter choice.** Prefer a discrete GPU, then an integrated one. Refuse CPU adapters (lavapipe, llvmpipe) in production builds, because a software overlay would burn the CPU the recogniser needs. An env var picks an adapter by name. [I]
- **Selection through the existing launcher.** `voxtype-osd` gains a `gpu` frontend. If `voxtype-osd-gpu` is missing, exits with the "no usable GPU" code from `--probe`, or fails its handshake, the launcher falls back to GTK. [V for the launcher, `~/work/voxtype/src/bin/voxtype_osd.rs`; I for the plan]
- **Opt-in first.** The GTK overlay stays the default for existing users until Oystein flips the default after the phase 1 gate and one release of opt-in use. A new config key never changes behaviour for an existing config. [V for the rule, voxtype `CLAUDE.md` "Backwards Compatibility"]
- **Reuse.** `voxtype-osd-native` already runs sctk, calloop and wgpu with a layer surface and frame-callback pacing. [V, `~/work/voxtype/src/bin/voxtype_osd_native/main.rs` header] The spike has a startup stage log with `since_exec`, PSS sampling and offscreen PNG capture. [V, spike `src/metrics.rs`, per `review-fable.md`] Both are starting points.

### 4.2 Crate layout

Four new crates under `overlay/` in the voxtype repo (see open question 1 about where it lives), plus the existing `xtask`.

| Crate | Contents | Depends on |
|---|---|---|
| `osd-protocol` | Message types, encoder and decoder, version constant. No GPU, no Wayland. Shared with the daemon. | none |
| `vo-core` | Springs, smoothers, keyframe tracks, statechart, word model and diff, tokens, layout maths, line breaking. Pure CPU, fully unit tested. | `osd-protocol` |
| `vo-render` | Device, pipeline cache, pass list, Slug text, hinted small text, SDF shapes, effects, particles, designs. One module per area. | `vo-core`, wgpu |
| `voxtype-osd-gpu` (bin) | Wayland and sctk, CPU first frame, presenter, IPC client, supervisor handshake, CLI, and the harness subcommands `render`, `bench`, `contact-sheet`, `--dump-state`, `--diagnose`, `--probe`, `--prewarm`. | `vo-render` |
| `xtask` (exists) | WGSL to SPIR-V with naga at build time, font subset and blob bake, golden update command. | none |

Modules give the same file-level separation for file reservations as crates would. `vo-core` builds and tests with no GPU, and that is where most logic bugs live. `vo-render` is split later only if build times hurt. [I, `review-fable.md` section 3.2]

### 4.3 Frame and pass list

Each frame runs these steps. Nothing reads the wall clock except the step that turns the presented time into `t`.

1. **Inputs.** Drain the sockets. Latest level wins, the latest text revision goes through the word diff, phase changes go to the statechart in order.
2. **Signals.** Smoothers produce level envelope, peak, onset, progress and state weights.
3. **Motion.** Springs and tracks step to time `t`. The statechart only sets targets.
4. **Layout.** Only when text or mode changed: shape the target layout once, words get target positions.
5. **Instances.** Write glyph, shape and particle-emitter instances plus one frame uniform block (time, level, peak, onset, progress, state weights, accent colour) into one staging upload.
6. **Passes.** Run the pass list for this design. A pass whose inputs are empty is skipped, and its pipeline is not compiled until first needed.
7. **Present** on the frame callback, `PresentMode::Fifo`, maximum frame latency 1. If every spring is settled and every emitter's CPU activity model says idle, request no further frame callback. KWin here offers `wp_fifo_manager_v1` and `wp_commit_timing_manager_v1`. [V, `wayland-info` on this machine, 2026-10-26, per `review-fable.md` 5.5] The spike picks `Mailbox` when offered, so its pacing numbers are not the production numbers.

Two pass lists [I, `research-effects.md` section 5; `review-fable.md` section 5.2]:

```
Direct path (designs with no whole-frame effect, such as 12):
scene: shapes, small text, glyphs -> swapchain (sRGB encode in the shader, premultiplied)

Effect path (designs that enable bloom, glass, trails or layer effects):
particle sim (compute)          optional
scene: shapes, glyphs, particles -> scene (Rgba16Float, linear, premultiplied)
layer targets                   optional, per layer that needs a whole-layer effect
text SDF (half res, jump flood) optional
bright extract + bloom pyramid  optional, reads the emissive mask only
glass                           optional, panel SDF + pyramid level
composite + tone map + dither   -> swapchain (sRGB, premultiplied)
```

The direct path blends in sRGB-encoded space at the swapchain, so its edge blending differs slightly from the linear path. Text contrast tuning is done per path and frozen into goldens. [I]

Layer model: every layer and every instance carries a 4x4 transform (orthographic camera by default), opacity, blend mode, clip SDF, an emissive flag and an optional offscreen target. 2D is the special case of 3D from day one. [I, `CAPABILITIES.md` section 3 item 8]

**Depth rule for later 3D, decided now.** Text and 2D vector never write depth. They draw after the 3D scene, back to front. 3D objects may sit behind text, never in front of it. If a design ever needs 3D in front of text, that is the moment to add a depth-peeled text layer. The Slug vertex stage keeps the full MVP and viewport in its dilation code, so perspective text stays supported. [I, `review-fable.md` section 5.4]

### 4.4 Text stack

| Layer | Choice | Notes |
|---|---|---|
| Fonts | `skrifa` outlines, metrics, variation axes, hinting, COLRv1 paint graph | One bundled OFL variable font with TrueType outlines (glyf and gvar), so there is no cubic conversion and point counts match across instances. Candidates: Roboto Flex (wght, wdth, opsz) and Inter 4 (wght, opsz) [I, check before R16 is decided]. Its curves and band data are baked into an mmap-able blob at build time with a version and checksum. A bad blob means a slow-path re-bake from the font file, then GTK fallback. [I] |
| Shaping and line breaking, phases 1 to 4 | `harfrust` directly, `unicode-linebreak` for break opportunities, our own line fitter (greedy plus a balanced mode for centred text), behind an adapter trait | About 200 lines. Keeps `fontique` and `parley` off the cold path. [I, `review-fable.md` section 4.5] |
| Shaping and layout, phase 5 | `parley` with `harfrust` behind the same adapter | When full bidi, script itemisation and system fallback are needed. [V for parley and harfrust, https://linebender.org/blog/tmil-25/] |
| Fallback in phases 1 to 4 | `fontique` queried lazily only when a glyph is missing from the bundled font | A glyph no font has draws as the font's missing-glyph box. Text is never dropped. [I] |
| Glyph rendering, animated or 14 px and over | Slug-style analytic coverage in WGSL | A rewrite of the reference shaders' data layout, not a transliteration. The reference keeps curves in an RGBA16F texture (two control points per texel, adjacent curves sharing an endpoint texel) and bands in an RG16U texture. [V, https://github.com/EricLengyel/Slug README] Phase 0 decision P0-06 picks textures or storage buffers. The port must keep: bands sorted by max x or max y for the early exit, straight horizontal lines only in vertical bands and vice versa, straight segments encoded as `{p1, p2, p2}`, dynamic dilation with the full MVP. Grayscale coverage only. |
| Glyph rendering, static and under about 14 px | Hinted CPU raster: skrifa hinting (TrueType interpreter, or the autohinter), skrifa's scan converter, a small R8 cache texture, textured quads | Used for labels, timestamps and chips that never change size. A few dozen glyphs, a few KB, rebuilt only when the scale or theme changes. The design picks the path per text run. [V for skrifa's hinters, https://github.com/googlefonts/fontations/blob/main/skrifa/README.md; I for the rule] |
| Contrast and gamma | Coverage in linear light, contrast curve keyed by foreground luminance, optional stem darkening, cap-height snapping at every size | Uniforms, tuned by eye on both displays and over a dark and a light wallpaper, then frozen into goldens. KWin blends our premultiplied edges in its own working space (`wp_color_manager_v1` v3 is present [V, `wayland-info`]), so the on-screen check is part of the phase 1 gate. [I] |
| Word model | Word-level Myers diff with a character-similarity cost for substitutions, stable word ids, states new, kept, substituted, removed, tentative, committed, final | Pure CPU, `similar` crate as the diff core. Revisions arriving during a transition are merged, not queued. The diff runs on a window of the last 200 words, under 1 ms per revision [G, checked in the bench]. Display keeps at most 1000 words, oldest dropped first. [I] |
| Final text | The final text (from the final pass, sometimes a different engine) is diffed against the whole live paragraph, not only the tail | Words that match by similarity keep their ids and crossfade in place; new words enter as new. Motion `settle-to-final` runs before `paste-confirm`. [V for the separate final pass, memory note `voice-typing-voxtype.md`; I for the rule] |
| Glyph instances | One instance per glyph: transform, opacity, colour, axis instance pair and fraction, blur, effect id and parameters, word id, emissive flag | Written per frame from springs. About 1000 instances at most. [I] |
| Reflow | Shape at the target layout at once, each word springs to its new place, wrap decided on commit | FLIP style, reading-order stagger of 12 ms. [I] |
| Animators | After Effects style range selectors over glyphs, words or lines, with audio inputs | Evaluated per glyph in the shader. Phase 2. [I] |
| Variable axes | Re-instance outlines on the CPU at quantised axis steps (for example wght in steps of 25), build bands per instance, cache them, and crossfade coverage between the two nearest instances in the fragment shader | Interpolating control points on the GPU is not a valid Slug input: it breaks the band sort order and band membership. Two coverage evaluations per pixel only for glyphs mid-transition. [V for the band order, Slug README; I for the fix, `review-fable.md` 4.2] |
| Scripts | Phases 1 to 4: Latin, Greek, Cyrillic and other left-to-right scripts the bundled font and fallback cover. Right-to-left runs are shaped per run but mixed-direction lines are not reordered. | Full bidi, complex scripts and CJK in phase 5. A known limit, written in the user docs. [I] |

What we deliberately do not do: a glyph atlas for animated or large text, LCD subpixel AA (wrong over arbitrary content and under transforms), cosmic-text (it owns its own raster cache). [I, `research-typography.md` sections 3 to 5] The spike uses cosmic-text with swash rasterisation [V, `~/work/voxtype-osd-spike/Cargo.toml`], which is fine for a feasibility test but is not the production text path.

### 4.5 Vector stack

- **SDF primitive uber-shader** (VE01): rounded rect with per-corner radii, squircle, capsule, circle, ring, arc, line, dot. Fill, stroke, hairline, analytic AA, analytic shadow (closed-form Gaussian of a rounded rect) and glow from the same distance. One instanced draw for all primitives. [I]
- **General paths** (VE04): icons and frames go through the Slug curve shader, stored in the same curve store as glyphs. [I]
- **Strokes** (VE03): SDF for lines, arcs and rounded polylines. `kurbo` stroke-to-outline, then Slug fill, for anything else. [I]
- **Trim and draw-on** (VE06): an arc-length parameter per curve and per SDF arc. [I]
- **Gradients** (VE08): in-shader linear, radial, conic, OKLab interpolation, blue-noise dither. [I]
- **Morph** (VE02, VE07): parameter springs for primitives, a `kurbo` contour resampler for the small icon set. [I]
- **CPU first-frame shapes**: the shm first frame needs rect, rounded rect, hairline and one status line. `tiny-skia` or our own fill plus skrifa's scan converter. [I]
- **Arbitrary SVG** (VE13): not planned. `vello_hybrid` behind a feature flag only if a design needs imported art. [I]

### 4.6 Effect stack

- **Linear premultiplied compositing** in an `Rgba16Float` scene target on the effect path, blend One / OneMinusSrcAlpha, glow writes alpha, tone-map shoulder above 1.0 only, sRGB out. [I, `research-effects.md` section 3]
- **Selective bloom** from an emissive mask, using the Call of Duty: Advanced Warfare pyramid (13-tap down with Karis average, 3x3 tent up, lerp blend). The implementer reads the slides for the exact tap weights. [V for the technique, https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/]
- **Analytic glow** from SDF distance wherever the source is a shape, because it costs no extra pass. [I]
- **Per-glyph effects inside the Slug fragment shader** (reveal wipe, dissolve, tint, halo, short directional blur), selected by the effect id. Large blur and chromatic split need the text in its own layer target. [I]
- **Glass** (FX06): own WGSL, panel SDF bevel normal, refraction of our own layers, Fresnel, rim light, tint, grain. A client cannot read desktop pixels, so without compositor blur the "glass" is a tinted panel with a bevel and nothing behind it to refract. The tint must be opaque enough that text meets the contrast bar over a pure white and a pure black backdrop. Real desktop blur through `ext-background-effect-v1` is an upgrade if KWin applies it to layer-shell surfaces. [V for the protocol on this KWin, `research-effects.md` section 2; unverified for layer shell]
- **Blur region**: rectangles that approximate the panel shape, rounded outward to whole logical pixels, updated with the surface commit at most once per frame. The rectangle count per corner is set in phase 4 from a stair-step test. [I, `review-fable.md` section 8]
- **Noise library** (FX10), **feedback buffer** for trails (FX11), **style pack** (FX13) later.
- **Quality tiers** (MO09): each optional pass has an off switch that still gives a correct picture. An allocation failure at show time drops to the next lower tier automatically.

### 4.7 Particle system

- Own compute system, about 600 lines of Rust and WGSL. Storage buffers, fixed-slot pools per emitter and per word id, fixed time step, vertex pulling with `draw_indirect`, no readback, no atomics whose order matters, so goldens are deterministic. [I, `research-particles.md` section 2]
- **Glyph-target formation** (PA03): targets from outline sampling (TY18), assignment by sorted position (optionally sorted by x plus a small hash, to avoid parallel streaks), spring plus curl-noise wobble. A revision retargets only the changed word's pool. Committed words are always solid glyphs. [I]
- **Handover without a pop.** Forming particles render into the same emissive layer the glyph will use. The glyph's opacity follows the word's convergence while particle brightness drops by the same amount, so the word's total light stays flat. The handover is checked with a luminance plot, not only images. [I, `review-fable.md` section 7]
- **CPU activity model** per emitter (spawn time, lifetime, last impulse), so the frame scheduler knows when it may stop drawing without a GPU readback. [I]
- Pipelines compile after frame 1. A particle design shows its solid text path first if the particle pipeline is not ready yet. [I]

### 4.8 Animation and state

- **Signal bus.** Inputs become smoothed signals. Signals set spring targets. Springs write instances and the frame uniform block. Every shader and the particle pass read the same block. [I, `CAPABILITIES.md` section 3 item 5]
- **Springs** with a closed-form solver, velocity kept on retarget, dt clamped at 50 ms, settle detection. **Keyframe tracks** only for one-shot beats, and their output adds onto a spring target. [I]
- **Statechart-lite** with parallel regions (text, level visual, panel, status). States follow the real daemon (section 4.10): loading, listening, streaming, finishing (transcribe, then optional post-process), pasted, empty, cancelled, error, connection-lost, hide. Sets targets only, so every transition is interruptible. [I]
- **Edge cases.** A new recording during the paste hold or during hide cancels it and re-enters listening with `snap`. A show cancelled within 300 ms skips the intro and plays a short fade out. There is never more than one surface. A mode change while visible (card to ticker to tall) keeps word ids and runs `mode-morph`. The recording limit shows `limit-warning` 5 s before `max_duration_secs` [G for 5 s]. [I]
- **Motion vocabulary**: the named motions in `CAPABILITIES.md` section 4 are implemented once as reusable behaviours with token parameters. Phase 1 needs the ones design 12 uses plus the shared state motions. [I]
- **Revision contract**: no revision animation runs longer than about 250 ms, and each one is interruptible. [I]
- **Reduced motion** is in phase 1: a config key, CLI flag and env var, plus Plasma's animation speed setting (`AnimationDurationFactor` in `kdeglobals` [G, key name not verified]). A factor of 0 means reduced motion. Each motion lists what it keeps (CAPABILITIES section 4). [I]

### 4.9 Designs, tokens and authoring

**Phase 1: designs are Rust functions.** `fn design_12(scene: &Scene, t: f64, tokens: &Tokens) -> DisplayList`. Flags on the function cover `min`, `tall` and `ticker`. [I, `review-fable.md` section 3.1]

**Tokens are one format: TOML.** One base token file per family and one small override file per colour or type variant (`12-warm`, `12-cool`, `12-green`, `12-light`). TOML because the voxtype config is TOML and users can write an override in `~/.config/voxtype/overlay/<design>.toml` without learning a new format. Tokens hold colours (OKLab), spring presets, durations, sizes, placement, the level playout delay and the small-text threshold. Dev builds reload token files at a frame boundary without losing spring state. User overrides are validated (size cap, value ranges) and a bad file is logged and ignored. [I]

**Placement tokens** per design: anchor (centre by default, mid-screen per the brief), vertical offset as a fraction of the output height, and grow anchor (centre or top edge) for size changes. The oversized surface is sized from the output size, so a 1366 x 768 laptop output gets a smaller margin. [I]

**Plasma theme mapping.** Parent designs follow the Plasma colour scheme: panel fill from the Window background, text from Window ForegroundNormal, listening accent from the global accent colour, paste from ForegroundPositive, error from ForegroundNegative, finishing from ForegroundNeutral. The `warm`, `cool` and `green` variants override the scheme by design. If the computed contrast of a scheme colour is under the bar (section 3.5), the design's own palette is used. A high-contrast scheme switches to a plain, opaque, no-effects look. Colours are read at show time (each spawn, and each show in a warm process); a change during a recording applies on the next show. sRGB scheme colours convert to OKLab when tokens load and to linear light in the shader. The parser in `src/osd/plasma.rs` (in the working tree, uncommitted, being changed by the GTK stopgap work [V, `git status`]) is reused once it lands, moved to a small shared module, with no KDE library dependency. [I]

**Phase 2: the component layer.** At the start of phase 2, with design 12 shipped and 21 and 03 prototyped, a decision bead chooses whether a shared component tree and a design file format earn their place, using the rule "add it only if three designs need something functions and tokens cannot express". [I]

### 4.10 IPC with the daemon

| Item | Design |
|---|---|
| Transport | Unix stream socket `$XDG_RUNTIME_DIR/voxtype/osd.sock`, mode 0600, length-prefixed frames. The existing 100 Hz `audio.sock` stays as it is. [V for audio.sock, `~/work/voxtype/src/audio/levels.rs` header] |
| Header | Magic, protocol version, message kind. Unknown kinds are skipped, a major version mismatch closes the connection and triggers the GTK fallback. |
| Encoding | `postcard` over serde structs in `osd-protocol`. No JSON. |
| Delivery rules | Snapshot on connect, deltas after. Per kind: `Level` is overwrite-latest. `Text` is overwrite-latest by revision number. `Phase` and `Config` are queued and never dropped (a dozen queued phases is a few hundred bytes). If even that backs up, the watchdog kills the overlay. [I, `review-fable.md` section 5.3] |
| `Phase` | `loading_model`, `listening`, `streaming` (text is typed into the app live), `finishing { stage: transcribe or post_process, fraction?, seconds_left? }`, `pasted { method: typed or pasted or clipboard_only or file, enter_sent, target_app?, elapsed_ms }`, `empty` (no speech), `cancelled { rewound_chars }`, `limit_warning { seconds_left }`, `error { kind, message }`. [V for the daemon states, `~/work/voxtype/src/state.rs` (Idle, Recording, EagerRecording, Transcribing, Outputting, Streaming, cancel rewind comment); config fields `on_demand_loading` and `max_duration_secs`, `src/config/schema.rs`] |
| `Text` | revision number, `source: live or final`, full hypothesis or `{stable_len, tail}`, optional per-word confidence and timing, paragraph break markers (the live preview splits at 0.8 s pauses after 6 s [V, commit 6179f5e]). |
| `Telemetry` | elapsed, word count, words per minute, level in dBFS, model name, engine latency, mic name, optional VRAM. Sent at 1 to 2 Hz. |
| `Config` | design name, variant, theme mode, output name, reduced motion, quality tier, hide transcript. Sent on connect and on change. |
| Heartbeat, both ways | The render loop (not the main thread) sends one every 500 ms while visible, so a hung render thread stops it. The daemon kills and restarts only its own child after 2 s of silence. The daemon also sends one every 500 ms while a recording runs; if the overlay hears nothing for 2 s, or the socket closes, it runs `connection-lost` and exits. |
| Migration | The GTK overlay keeps reading files until it is moved to the socket. Both paths run side by side for one release. The spike's `--mode live` reads `$XDG_RUNTIME_DIR/voxtype/preview`, so the file path exists today. [V, spike `main.rs`, per `review-fable.md` 5.3] |
| Upgrade order | The daemon spawns a fresh overlay per recording, so a package upgrade takes effect on the next recording. A warm overlay checks the protocol version and the driver version (`/proc/driver/nvidia/version`) at each show and exits if either changed. [I] |

Daemon-side work: revisions with numbers, the final text as `source: final`, the real phases above, progress as numbers, the Enter flag and output method, telemetry, paragraph markers, the CLI time stamp, and the output name. Per-word confidence, target app name and VRAM are separate research items (section 8).

### 4.11 Failure matrix

This is a phase 0 deliverable. Each row gets a tested response before phase 1 closes. [I, `review-completeness.md` section 8]

| Failure | Response | Test |
|---|---|---|
| Surface offers only `Opaque` alpha | Exit with a code; launcher uses GTK. Never draw an opaque panel. | Forced by a debug flag |
| No Vulkan, no ICD, VM, remote session | `--probe` exits with "no usable GPU"; launcher uses GTK | Run with `VK_ICD_FILENAMES` pointing nowhere |
| Only a CPU adapter | Refuse in production builds; GTK | Run with lavapipe forced |
| Device lost, surface lost | Fade nothing, exit; daemon respawns on next recording, or at once if mid-recording, within the restart limit | Debug flag |
| Render thread hang | Heartbeat stops; daemon kills it after 2 s | SIGSTOP the render thread |
| Daemon dies or socket closes | `connection-lost`, fade within 2 s, exit | Kill the daemon |
| Compositor restart | Wayland connection lost; exit; daemon respawns on next recording | `kwin_wayland --replace` on a nested session |
| Output unplugged mid-recording | Hide, keep recording, re-create on the daemon's new output choice | Nested session output removal |
| GPU allocation fails at show | Drop to the next lower quality tier, then to the solid-text tier | Debug allocation cap |
| Pipeline cache unreadable or unwritable | Compile, log one line, carry on | Read-only cache dir |
| Font blob stale or corrupt | Version and checksum fail; re-bake from the font file; if that fails, GTK | Corrupted blob fixture |
| Glyph in no font | Missing-glyph box; text never dropped | Fixture with an unassigned code point |
| Oversized or malformed message | Decoder rejects, connection closes, fallback | `cargo-fuzz` on the decoder |
| Burst of revisions | Merged; diff bounded to the last 200 words | Bench with 50 revisions per second |
| Version mismatch | Clean close, log, GTK | Paired old and new builds |
| Layer shell missing (GNOME, X11) | Launcher never picks `gpu` | Launcher unit test on the protocol list |
| Other compositors (Sway, Hyprland) | Probe each protocol; missing fractional scale means integer scale, missing presentation means callback time, missing background effect means fake glass | Headless Sway and Hyprland runs in phase 1 |

### 4.12 Accessibility and privacy

- **Reduced motion and flash cap** in phase 1 (section 4.8, targets in 3.5).
- **Contrast** computed from tokens in the golden suite (3.5).
- **Non-colour cues.** Every phase shows an icon or a word: check and "Pasted", cross and "Error", and so on. Required in every variant.
- **User scale.** A `scale` token multiplies all sizes, with a floor that keeps the smallest label at 10 px or more. Plasma's font size setting is not followed automatically (section 10).
- **Screen readers.** The overlay is click-through and unfocusable, so Orca cannot see it. The non-visual channel is the daemon's existing audio feedback cues (`src/audio/feedback.rs` [V, voxtype `CLAUDE.md` module list]) plus an optional desktop notification with the final text. The overlay does not add AT-SPI in phases 1 to 4.
- **Hide transcript.** A config key that shows level and state only, for screen sharing and recording. Built from the `min` variants.
- **Recorded dictations.** The revision recorder writes to `$XDG_DATA_HOME/voxtype/osd-corpus/`, mode 0700, off unless enabled, never committed. The CI corpus is synthetic or written by Oystein for the purpose.
- **Debug output.** `--dump-state` runs only on harness scenarios, not on a live session.

### 4.13 Testing

| Layer | What | Where |
|---|---|---|
| Unit | Springs, smoothers, statechart, word diff, protocol codec, line fitter | `cargo test`, CI |
| Property and fuzz | Word diff invariants (ids stable for unchanged words, no lost words, final swap keeps matching ids), protocol decoder fuzzing with `cargo-fuzz` | CI nightly |
| Replay corpus | Recorded revision streams with the final text (text, level, phase with timestamps), replayed through the word model and the renderer | CI for the model, local for pixels |
| Golden images | Every design and variant at fixed times in fixed scenarios (loading, listening at 3 levels, long text with tail revisions, final swap, finishing at 0, 50 and 100 %, post-process, pasted, pasted with Enter, clipboard only, empty, cancelled, error, reduced motion, hide transcript), at 1x and 1.5x, rendered offscreen | NVIDIA goldens locally in phase 1; lavapipe goldens in CI from phase 2, when CI exists for the crates |
| Text quality suite | Section 3.4 tests | Local on NVIDIA, CI on lavapipe from phase 2 |
| Accessibility suite | Contrast ratios, flash trace, colour-vision simulation | CI |
| Motion traces | Values of named springs over time for each scenario, stored as small CSV goldens | CI |
| Performance | `bench` on the gate GPU: startup stages, frame times, PSS, CPU, power, dropped frames. Fails on a missed target or a 20 % p99 regression | Local, before merge of any GPU change |
| Fault injection | Every row of the failure matrix | Local |
| Compositor smoke | Layer mapping, click-through, scale change, output change, under `kwin_wayland --virtual`; protocol probe under headless Sway and Hyprland | Local, private display only |

All GPU runs from agents happen offscreen or on a private nested display. No agent opens a window on Oystein's screen.

### 4.14 Tooling

- **Startup stage log** first, because every latency target depends on it.
- **Shader hot reload** of WGSL inside a validation error scope that keeps the old pipeline on error, and **token hot reload**. Dev builds only. [I, `research-effects.md` section 10]
- **Scrubber**: `voxtype-osd-gpu render --design 12 --scenario revise.ron --t 1.25 --out f.png` renders one frame at any time; `contact-sheet` renders a grid of frames for review. This is also how agents show their work.
- **`--dump-state`** prints spring, signal and statechart values at a given `t` in a scenario. It replaces a live inspector. [I, `review-fable.md` section 9]
- **`--diagnose`** prints GPU name, driver version, alpha mode, present mode, protocols found, quality tier chosen, restart and fallback counters. Users attach it to bug reports.
- **Profiler**: Tracy behind a `profile` feature, wgpu timestamp queries per pass, RenderDoc capture on request for single frames. [I]
- **Scenario authoring**: scenarios are RON files of timed inputs, and the recorder in the daemon can dump a real session as a scenario.

## 5. Phases

Overview:

| Phase | Name | Main output | Size [G] |
|---|---|---|---|
| 0 | Measure and decide | Numbers for the go/no-go questions, Slug port, particle look test, failure matrix | 3 to 4 weeks |
| 1 | First shippable popup | Swiss 12 family (10 designs), opt-in, with GTK fallback | 6 to 9 weeks |
| 2 | The picked designs | 21, 22, 17, 03 families plus 12-serif and 12-rich (48 designs); component layer decision | 8 to 10 weeks |
| 3 | Particle words | 09 family plus 03-rich (13 designs) | 4 to 6 weeks |
| 4 | Glass and blur | Six `glass` variants plus 17-rich (7 designs) | 3 to 5 weeks |
| 5 | Typography beyond the mockups | Bidi and complex scripts through parley, CJK, emoji, text on path, outline morph, HDR | 4 to 6 weeks |
| 6 | 3D | Depth scene, SDF hero shapes, extruded text, scenes reacting to words | 6 to 10 weeks |

Phases 0 to 4 sum to 24 to 34 weeks of work. Calendar time is longer: gates wait on Oystein's reviews and office runs, so the estimate is 7 to 10 months with about 20 % slack. Phases 3 and 4 do not depend on each other and can run as parallel lanes. Phase 5 items can be pulled forward one at a time when a design needs them.

Why this order [I]: every picked design needs the core (34 capabilities at must in all six families, listed in `CAPABILITIES.md` section 0). Design 12 needs that core and nothing else, with the hardest typography, so it proves the foundation. Phase 2 adds the designs whose new needs are cheap (dots, widgets, chips, morphing capsule, bloom). Particles and glass carry the highest visual risk, so their look is tested in phase 0 and their full build comes once the base is stable and measured.

### 5.1 Design ownership matrix

Every one of the 78 picked designs has one owning phase. Columns are the parent and its 12 variants.

| Family | parent | min | rich | light | warm | cool | green | serif | ticker | tall | finish | pasted | glass |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 12 Swiss | 1 | 1 | 2 | 1 | 1 | 1 | 1 | 2 | 1 | 1 | 1 | 1 | 4 |
| 21 dot matrix | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 4 |
| 22 mission control | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 4 |
| 17 command palette | 2 | 2 | 4 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 4 |
| 03 dynamic island | 2 | 2 | 3 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 4 |
| 09 particle words | 3 | 3 | 3 | 3 | 3 | 3 | 3 | 3 | 3 | 3 | 3 | 3 | 4 |

Counts: phase 1 has 10, phase 2 has 48, phase 3 has 13, phase 4 has 7, total 78. Reasons for the odd cells: `12-serif` needs a second font, decided in phase 2. `12-rich` needs bloom. `17-rich` needs the glass rim. `03-rich` needs motion blur and streaks (FX12). `21-rich` needs the ribbon (VE11) and bloom, both phase 2. `22-rich` needs bloom and rim only, no streaks, so it stays in phase 2. [I, from `mockups-v12-17.md`, `mockups-v21-22.md`, `mockups-v03-09.md`]

In each phase, variants are built as token files or flags on the parent's function. Only parents and the `ticker` and `tall` layouts are scheduled as design work. [I, `review-fable.md` finding 7]

States with no mockup (loading, streaming, post-process, empty, cancelled, limit warning, connection lost, clipboard only, error) are designed per family in the phase that owns the parent, and Oystein approves them at that gate. [V that error and finishing are invented, `mockups-v21-22.md` line 67 and `mockups-c01-08.md` point 5]

### Phase 0: Measure and decide

**Goal.** Answer the questions that could change the architecture before writing production code. Use the running spike where it already answers a question; add small throwaway tests where it does not.

**Questions and pass criteria:**

| # | Question | Method | Decision it drives |
|---|---|---|---|
| Q1 | Cold first frame with precompiled SPIR-V and a disk pipeline cache, both GPUs | Startup stage log, 50 runs, p50 and p95, in FIFO | Process model |
| Q2 | Warm hidden PSS after dropping the swapchain | smaps_rollup | Keep-warm cost (expected 80 to 90 MB [G]) |
| Q3 | Alpha mode offered by NVIDIA on Wayland and whether KWin composites it right | The spike already requests `PreMultiplied` and prints the result on its `SWAPCHAIN` log line; one run on each GPU, plus a look on a nested display [V, spike `popup.rs` lines 285 to 309, per `review-fable.md`] | FX01 as designed, or the fallback to GTK |
| Q4 | Frame pacing at 60, 75, 120 and 144 Hz with FIFO on KWin and NVIDIA | `wp_presentation` intervals over 60 s, `Fifo` with latency 1 (not the spike's `Mailbox`) | Whether an ash presenter is needed |
| Q5 | Does KWin apply `ext-background-effect-v1` to a layer-shell surface, and can the region change every frame | 20-line test plus a region that morphs at 120 Hz, checking for lag against commit timestamps | Glass variants with real blur, or fake only |
| Q6 | Slug port: error against a supersampled reference at 14, 24, 56, 120 px, and speed on the phase 1 surface with 300 glyphs | Port with the layout rewrite (P0-06, P0-07) | Text path confirmed; the measured error becomes the gate |
| Q7 | Word diff on real revision streams including the final text | Record 20 dictations with final texts, run the diff, count id churn and sideways moves at the final swap | TY06 algorithm and `settle-to-final` |
| Q8 | harfrust plus our line fitter cold cost; fontique lazy load cost on a miss | Timed load | Phase 1 shaping path |
| Q9 | CPU first frame through `wl_shm` on a second surface while Vulkan starts, then crossfade to the GPU surface | 100-line experiment on a nested display, startup log | Whether keep-warm is needed for latency at all |
| Q10 | Particle formation look: one word, a scripted revision, rendered offscreen | Start from the spike's `--particles` mode [V, spike `main.rs`] | Whether 09 is a family or a `rich` variant |
| Q11 | How the daemon learns the target output on KWin | Try a KWin script over D-Bus reporting the active output, and `kscreen-doctor` [G for both] | PL12 |
| Q12 | Hinted small text: skrifa hinting plus scan converter at 9 to 12 px against KDE's own rendering | Contact sheet | TY26 confirmed |

**Exit criteria.** All twelve answered with numbers or images in a short report. Section 3 targets confirmed or changed by explicit decision. Failure matrix (section 4.11) agreed. Crate skeleton in place. Slug reference licence re-read from the repository's licence files.

**Risks.** The spike answers some questions with a non-production setup (cosmic-text, Mailbox). Treat its text and pacing numbers as indications only.

**Prototype first.** Q1, Q9 and Q6. Cold start and the shm frame decide the process model; the Slug port decides the text path.

### Phase 1: First shippable popup (Swiss 12)

**Goal.** Ship the Swiss 12 family as `voxtype-osd-gpu`, opt-in through the launcher. It must have better text than the GTK overlay and meet the latency and memory targets. It falls back to GTK on its own, meets the accessibility baseline, and has a tested response to every failure.

**Capabilities delivered.**
- Platform: PL01 to PL10, PL12 (output from the daemon), PL13 (CPU first frame, if Q9 passed), PL14 (failure handling, `--diagnose`, `--probe`), PL15 (placement).
- IPC: IO01 (exists), IO02, IO04, IO05 (Enter flag and output method), IO06 (duration and word count), IO08, IO09, IO10 (real daemon states).
- Text: TY01, TY02 (harfrust path), TY03, TY04, TY05 basic, TY06 (with the final swap), TY07, TY08, TY13, TY14 (for `12-ticker`), TY15, TY20, TY25 (balanced wrap), TY26 (hinted small text).
- Vector: VE01, VE03, VE04, VE05, VE06, VE09, VE12 (for `12-light`).
- Effects: FX01 (direct path only), FX04, FX09, FX14.
- Motion and layout: MO01 to MO08, MO09 basic (reduced motion switch), LY01 (only what 12 needs), LY02, LY03.
- Accessibility: AX01 to AX04.

**Mockups buildable.** The ten phase 1 cells of the matrix in 5.1: 12, 12-min, 12-ticker, 12-tall, 12-finish, 12-pasted, 12-light, 12-warm, 12-cool, 12-green. Plus the 12 looks for the states with no mockup.

**Exit criteria.**
- Hotkey to first frame p95 under 150 ms cold and under 30 ms warm on the gate GPU, smoke run on the office GPU, or a recorded decision that changes the target.
- GPU p99 under 1.0 ms, CPU p99 under 1.5 ms, dropped frames under 0.5 % at the refresh rates in scope.
- Visible PSS under 130 MB. Zero wakeups when hidden.
- Text quality suite passes, including small static text and the final text swap. Hairlines are 1 physical pixel at all listed scales.
- Accessibility suite passes. Reduced motion and hide transcript work.
- Every failure matrix row has a passing test.
- On the replay corpus, unchanged words never move sideways on a left-aligned line, and no revision animation runs longer than 250 ms.
- Goldens for all ten designs in every scenario.
- Oystein approves the looks for error, cancel, empty, loading, streaming and clipboard only.
- One week of daily use by Oystein, no fallback caused by a crash (supervisor counter).
- Oystein prefers it to GTK in the side-by-side check, including over a dark and a light wallpaper.
- Packaging: the binary builds in the project's Docker images with the glibc floor and instruction checks; user manual, configuration guide, troubleshooting and `--help` cover the new keys.

**Size.** 6 to 9 weeks. [G]

**Main risks.** Cold start over budget on NVIDIA (R1). Slug and hinted text quality at small sizes (R4). Word diff quality on real streams and at the final swap (R5). Pixel snapping versus smooth motion (R15). Daemon changes for the real states touch hot code (R18).

**Prototype first.** (1) The startup log and shm first frame in the real binary skeleton. (2) The word model against the recorded corpus, with no GPU. (3) "VOICE DICTATION" at 11 px (hinted) next to the 56 px transcript (Slug).

### Phase 2: The picked designs

**Goal.** Build the families 21 (dot matrix), 22 (mission control, simplified), 17 (command palette) and 03 (dynamic island, mid-screen), plus 12-serif and 12-rich. Decide whether a component layer and a design file format are worth it. Add design switching by config. Choose the default design with Oystein at the end.

Note on 17: its main feature, per-word confidence, ships with a stand-in (word age and revision count) unless open question 8 gets real engine confidence. The 17 family as drawn is not fully delivered until then.

**Capabilities delivered.**
- Text: TY09 (animators), TY10 (tentative and confidence styling, proxy first), TY11 (variable axes, quantised re-instancing), TY12 (decorations), TY16 (dot-matrix type), TY17 (coverage effects: blur-in, reveal, halo), TY24 (scramble and typewriter for 21).
- Vector: VE02, VE07, VE08, VE10, VE11.
- Effects: FX02 effect path and layer targets, FX03 (bloom), FX05 (analytic glow), FX08 (rim, sheen, orbit), FX10 (noise), FX13 subset (scan texture for 22).
- Particles: PA09 grid-snapped dot particles only if 21 wants dots peeling off (decided at the 21 prototype).
- Motion: MO09 full (quality tiers, battery trigger from UPower [G]).
- IPC: IO03 proxy, IO06 full (VRAM only if open question 9 says yes), IO05 target app name if open question 5 is solved.
- Platform: PL11 only if Oystein wants real buttons (open question 4).
- Authoring: component layer and design file format if the phase 2 decision bead says yes.
- Test: lavapipe goldens in CI.

**Mockups buildable.** The 48 phase 2 cells of the matrix in 5.1.

**Exit criteria.**
- Every phase 2 design has goldens and passes the phase 1 performance targets, with bloom on: GPU p99 under 2.0 ms.
- Switching designs in config takes effect on the next recording without restart of the daemon.
- 21: dots sit on whole physical pixels at all listed scales when settled.
- 17: the three confidence levels render from the proxy and from a scripted confidence stream.
- 03: the capsule morph from dot to capsule to panel shows no corner artefacts at any radius (golden sweep).
- Accessibility suite passes for every new design.
- Oystein picks a default design.

**Size.** 8 to 10 weeks. [G]

**Main risks.** Scope: four families at once (R10). Confidence data does not exist (R12). Font choice (R16).

**Prototype first.** The dot-matrix font as data with dot instances (21), and the capsule morph shader (03). Then the component layer decision bead, with 12, 21 and 03 as evidence.

### Phase 3: Particle words

**Goal.** The 09 family: words form from particles where they will land, revisions scatter and re-form only the changed word, committed words are solid text. Plus 03-rich.

**Capabilities delivered.** PA01 to PA08, TY18 (glyph point sampling), TY19 (text SDF), FX11 (feedback buffer), FX12 (motion blur and streaks), FX16 (bokeh), D301 (2.5D layers) and D302 (3D particles) for the tilted ring, D305 for 09-rich.

**Mockups buildable.** The 13 phase 3 cells of the matrix in 5.1.

**Exit criteria.**
- A new word's solid glyph reaches 50 % opacity within 200 ms of the recogniser update on the replay corpus, and Oystein judges the look on a contact sheet and on screen. [G target, open question 7]
- No pop at the handover from particles to solid glyph: the word's summed luminance varies under 5 % across the handover [G].
- 20k particles active: GPU p99 under 3.0 ms. CPU p99 under 1.5 ms.
- The frame loop stops within 1 s after all emitters go idle.
- Particle goldens are deterministic on lavapipe.
- First frame of a 09 design meets the phase 1 latency target, with particle pipelines compiled after frame 1.
- Reduced motion replaces particles with the solid-text path.

**Size.** 4 to 6 weeks, assuming phase 0 Q10 settled the look. [G]

**Main risks.** Legibility delay and the look on real revision streams (R6). Extra pipelines on the cold path (R1).

**Prototype first.** Start from the Q10 prototype; build the full emitter library only after the formation and handover pass on the corpus.

### Phase 4: Glass and blur

**Goal.** All six `glass` variants, plus 17-rich.

**Before sizing.** A short design check: render 12-glass with the fake material over three wallpapers (dark, light, busy) as static images, and decide with Oystein whether the fake is good enough to build, or whether glass waits for real compositor blur. The wallpaper snapshot idea is rejected (section 10).

**Capabilities delivered.** FX06 (glass material), FX07 (compositor blur behind, if Q5 passed), `glass-focus` motion, D305 (floor reflection and light cone, if not already done in phase 3).

**Mockups buildable.** The 7 phase 4 cells of the matrix in 5.1. With this phase all 78 picked designs exist.

**Exit criteria.**
- The material looks right on a dark, a light and a busy wallpaper (contact sheet reviewed by Oystein).
- Text over glass meets 4.5:1 against pure white and pure black backdrops.
- With real blur available, the blur region follows the morphing panel with no lag over one frame, measured by commit timestamps against region updates, and no visible stair-step at the corners.
- GPU p99 under 3.0 ms with glass and bloom.

**Size.** 3 to 5 weeks. [G]

**Main risks.** No real blur on layer-shell surfaces (R7). The fake may not convince on busy backgrounds.

**Prototype first.** The glass shader on 12-glass, the flattest design, so the material is judged alone.

### Phase 5: Typography beyond the mockups

**Goal.** The parts of "state of the art typography" that no current mockup needs but that make the renderer complete.

**Capabilities delivered.** Move shaping to parley behind the existing adapter; TY05 full (bidi, complex scripts, CJK, measured fallback cost), TY22 (COLRv1 emoji), TY23 (text on a path), TY21 (glyph outline morph), automatic optical size, optical margin alignment and hanging punctuation for the 12 card, FX15 (HDR output and colour tagging) once NVIDIA support is checked.

**Mockups buildable.** No new picked designs. Round 1 concepts 13, 14, 20, 23, 24 become closer to buildable. [I]

**Exit criteria.** Text quality suite extended to Arabic, Hebrew, CJK and emoji passes. Word model and reflow work on right-to-left lines. Fallback adds under 20 ms to a cold start in which a fallback glyph is needed. HDR output verified on an HDR display, or deferred by decision.

**Size.** 4 to 6 weeks. [G]

**Main risks.** fontconfig enumeration cost in a per-recording process. HDR on the NVIDIA Vulkan path. [G]

**Prototype first.** Lazy fallback timing with a Japanese and an emoji test string.

### Phase 6: 3D

**Goal.** 3D scenes that interact with the text and the voice: depth-ordered scenes, SDF raymarched hero shapes, extruded text, scenes that react to words through the text distance field. Text stays in front, per the depth rule in 4.3.

**Capabilities delivered.** D303 (SDF raymarched hero shapes), D304 (depth-ordered scene graph), D306 (extruded 3D text), PA08 extended to 3D. D307 (glTF and PBR) stays rejected unless a design needs it, because of memory and start-up cost.

**Mockups buildable.** No picked design needs 3D at must. Round 1 concepts 05, 15, 20 and new designs made for 3D. [V, `CAPABILITIES.md` summary point 6]

**Exit criteria.** Set when a 3D design is chosen. Fixed parts: phase 1 latency and memory targets still hold for every non-3D design, and a 3D design stays under 160 MB PSS and 3.0 ms GPU p99.

**Size.** 6 to 10 weeks. [G]

**Main risks.** Memory and start-up cost of extra pipelines. No design target yet, which invites building things nobody uses (R10).

**Prototype first.** A raymarched orb that reads the level signal and the text SDF, in an existing design, before any mesh work.

## 6. Build or adopt

| Component | Decision | Reason |
|---|---|---|
| GPU API | **Adopt wgpu** (Vulkan only); `ash` only as a contained presenter fallback | Mature, gives compute and timestamps. Our memory floor is the NVIDIA driver, which ash would not remove. [I, `research-platform.md` section 2] |
| Wayland client | **Adopt smithay-client-toolkit** + calloop | Already used by `voxtype-osd-native`. [V, its source header] |
| Font parsing, outlines, hinting | **Adopt skrifa** | Variation axes, COLRv1 and three hinting engines in one Rust crate. [V for hinting, skrifa README] |
| Shaping | **Adopt harfrust** directly in phases 1 to 4; **parley** on top in phase 5 | Smaller cold path now; parley when bidi and itemisation are needed. Pre-1.0, hence the adapter. [V, https://linebender.org/blog/tmil-25/] |
| Line breaking | **Adopt `unicode-linebreak`**, build the line fitter | Greedy plus balanced fits are short. [I] |
| Glyph rendering, animated and large | **Build: Slug-style analytic coverage**, a port of the reference shaders with the data layout rewritten | Exact at any transform, no atlas rebuilds when axes or sizes animate, same data for icons and particles. Patent dedicated to the public domain. [V, https://terathon.com/blog/decade-slug.html] |
| Glyph rendering, small static | **Build** a tiny hinted cache on skrifa's hinter and scan converter | Slug ignores hinting; skrifa already has the hinters. [V, Slug README; skrifa README] |
| Glyph atlas for animated text (swash, MSDF) | **Do not adopt** | Rebuilds on every size or axis change, corner artefacts for MSDF, LCD AA is wrong over arbitrary content. [I] |
| cosmic-text | **Do not adopt** | Owns its own raster cache; we want outlines. [I] |
| Vector shapes | **Build SDF uber-shader** | A few hundred shapes per frame, one pipeline, tiny memory, morphing for free. [I] |
| CPU first frame | **Adopt tiny-skia** or build a rect fill, if Q9 passes | A few hundred microseconds of CPU drawing. [G] |
| General vector renderer | **Do not adopt Vello** now; `vello_hybrid` behind a feature flag only if imported SVG art is needed | Pre-1.0 (0.0.x), more pipelines and memory on the cold path, text path not ours. [V for version state, https://github.com/linebender/vello] |
| Rive renderer | **Do not adopt** | C++, needs pixel local storage that wgpu does not expose, and its value is its animation runtime, which was ruled out earlier. [I, `research-vector.md`] |
| Skia Graphite | **Do not adopt** | Large C++ dependency, wrong memory profile for a per-recording process. [I] |
| Path maths, stroking | **Adopt kurbo**; lyon only for odd cases | Small, Rust, stroke-to-outline and arc length. [I] |
| Word diff core | **Adopt `similar`**, build the identity, stability and final-swap layer | The diff is solved; identity rules are ours. [I] |
| Pass list | **Build** two fixed pass lists | One target size, 1 to 8 passes; a general graph adds weight for nothing. [I, `research-effects.md` section 5] |
| Bloom, blur, glass | **Build** from published techniques | COD:AW pyramid, dual Kawase, SDF glass. [V for the bloom talk, link in section 4.6] |
| Particles | **Build** | About 600 lines; engine crates pull in an ECS and more. [I, `research-particles.md` section 2.1] |
| Motion core | **Build** | Solver is about 40 lines; we need exact velocity handling. [I] |
| Design description | **Build** as Rust functions plus TOML tokens in phase 1; file format decided in phase 2 | Section 4.9. [I] |
| Authoring tool | **Do not build** a visual editor | [I, `research-animation.md` section 1] |
| Profiling | **Adopt Tracy**, wgpu timestamp queries, RenderDoc | [I] |
| IPC encoding | **Adopt postcard** | Compact, serde-based. [I] |
| 3D meshes | **Defer**; `lyon` triangulation for extruded text; no glTF/PBR unless a design needs it | [I] |

A one-time licence audit bead in phase 1 covers the Slug shaders, wgpu, skrifa, harfrust, kurbo, `similar`, tiny-skia, the bundled fonts (OFL text and attribution in the packages) and any noise code.

## 7. Risk register

Likelihood and impact are Low, Med, High, all [G].

| ID | Risk | Likelihood | Impact | Mitigation | Watch signal |
|---|---|---|---|---|---|
| R1 | Cold first frame stays over 150 ms on NVIDIA even with a pipeline cache | High | High | Phase 0 measures first. CPU shm first frame (Q9), parallel init, precompiled SPIR-V, font blob, lazy pipelines, `--prewarm`, keep-warm if still needed | Startup log p95 |
| R2 | NVIDIA does not offer premultiplied alpha on a Wayland surface | Med | High | Q3 in one run. If only `Opaque` is offered, the GPU overlay does not run and GTK stays; Qt Quick is the fallback for the whole overlay | Surface capabilities |
| R3 | Frame pacing stutter on KWin with the NVIDIA driver | Med | Med | FIFO with fifo-v1 present; ash presenter with present wait if needed. [V for history of pacing problems, https://invent.kde.org/plasma/kwin/-/work_items/124] | Dropped-frame count |
| R4 | Text too soft at small sizes or too costly at large ones | Med | Med | Hinted path for small static text, cap-height snapping, contrast uniforms, band tuning | Text quality suite, GPU timings |
| R5 | Word diff makes the wrong words move on real streams or at the final swap | Med | High | Recorded corpus with final texts from day one, property tests, churn metric in CI | Id churn per revision |
| R6 | Particle words read too late or look noisy | High | Med | Look tested in phase 0 (Q10); flat-energy handover; committed words always solid; reduced-motion tier drops particles | Readability time on corpus |
| R7 | KWin does not blur behind layer-shell surfaces | Med | Low | Fake glass with an opaque enough tint; design check before phase 4 | Q5 result |
| R8 | parley, harfrust or wgpu API churn | High | Low | Pin versions, adapter traits, upgrade as a planned bead once per quarter | Upgrade diff size |
| R9 | Memory creeps up as passes and pipelines are added | Med | Med | Memory ledger and PSS in the bench, gate per phase | PSS in bench |
| R10 | Scope: "state of the art in everything" with one developer | High | High | Thin phase 1; every phase ships; variants are tokens; designs drive capabilities | Phase calendar slip |
| R11 | Upstream voxtype does not want a large renderer | Med | Med | Own workspace members behind a feature, protocol as the only shared surface; decide location early (open question 1) | Maintainer response |
| R12 | Data gaps: confidence, target app, VRAM | High | Med | Proxies, hide missing fields, separate research beads | Daemon support per engine |
| R13 | Overlay steals CPU or GPU from transcription | Low | High | 5 % gate, `nice 5`, zero frames when settled, ambient loops at 30 fps, GPU contention bench | CPU and GPU contention bench |
| R14 | Driver update empties the pipeline cache and the next recording starts slowly | High | Low | `--prewarm` at daemon start, cache keyed by driver version and device UUID, warm process exits on driver change | First-run time after update |
| R15 | Pixel snapping makes moving hairlines and dots jitter | Med | Med | Snap only settled elements, fade between snapped and unsnapped as the spring settles | Sweep goldens |
| R16 | No OFL TrueType variable font has the axes, Norwegian coverage and the right look | Med | Med | Decide in phase 0; allow two fonts; accept fewer axes | Font evaluation |
| R17 | Agent-written shader code passes review but looks wrong | Med | Med | Goldens and contact sheets on every GPU bead; Oystein reviews contact sheets at gates | Golden diffs |
| R18 | Daemon changes for the real states and the protocol break existing behaviour | Med | High | Protocol behind a config key; GTK overlay reads files until moved; backwards-compatibility test with an old config | Regression test skill run |
| R19 | A visible failure (opaque box, stuck overlay) reaches a user | Low | High | Failure matrix with tests; opt-in period; `--diagnose` | Bug reports, supervisor counters |
| R20 | Gates stall waiting for the office GPU | Med | Low | Home GPU is the gate, office is a scripted smoke run | Gate dates |

## 8. Decisions (Oystein, 2026-10-26)

1. **Code home.** Its own repo. Only the small `osd-protocol` crate goes into voxtype.
2. **Slug source.** Copy HarfBuzz `hb-gpu`'s WGSL shader and blob format (MIT, merged 2026-03-28, shipped in HarfBuzz 14.0) into our Rust code, with no C library at run time. Do not write our own port from Lengyel's reference.
3. **Keep warm.** Allowed at about 80 to 90 MB for a few minutes after each recording. Exit early when the pressure level is `slow` or `pause`, as the warm worker does.
4. **Font.** The Plasma system font (read from kdeglobals, Noto Sans by default). The serif variants use the fontconfig `serif` default. No paid fonts.
5. **Live typing end.** A short done beat with no check mark.
6. **Buttons.** Display only. The overlay stays fully click-through.
7. **App name.** Yes. Ask KWin for the target app, as auto-Enter already does.
8. **Particle readability.** Each new word is a solid glyph at 50 % opacity within 100 ms of the recogniser update (stricter than the proposed 200 ms).
9. **Confidence.** Real per-word confidence from the daemon where the model gives it, with word age as the fallback for models that do not.
10. **VRAM in 22.** Dropped.
11. **09-finish.** Spinner or progress while finishing; the check mark only on paste.
12. **Motion storyboards.** Yes. GPT Image frame sequences of show, revise, final swap and paste before phases 2 and 3.
13. **Screen readers.** Sound cues plus an optional notification. No AT-SPI work.
14. **3D.** Keep the door open only. No phase 6 work planned.
15. **Phase 0.** Approved to start on 2026-10-26.

## 9. Execution as beads for agent swarms

### 9.1 Rules for every bead

- One bead touches 1 to 3 files and ends in one signed commit with a pathspec. GPU beads commit before they build or test. Builds and tests go through `heavy-build`. [V, user CLAUDE.md and voxtype CLAUDE.md]
- Every GPU bead adds or updates goldens and attaches a contact sheet path to its close note.
- Every bead that touches a hot path runs `bench` and records the numbers in its close note.
- Execution agents share one branch, no worktree isolation. [V, user CLAUDE.md]
- Agents never open windows on the real display; they use the offscreen harness or a nested private display.
- Model choice: `sonnet` for mechanical beads (protocol plumbing, goldens, token files, tests, docs), `opus` for shader, layout and diff code, a short `fable` consult at the decision beads marked **D** below. [V, user CLAUDE.md model table]

### 9.2 Lanes

| Lane | Owns | Phase 1 agent count [G] |
|---|---|---|
| Platform | `voxtype-osd-gpu`, device and present modules of `vo-render` | 1 |
| Text | word model in `vo-core`, text modules of `vo-render` | 2 |
| Shapes and design | shape modules, design 12 function, tokens | 1 |
| IPC and daemon | `osd-protocol`, daemon changes, launcher | 1 |
| Harness, packaging and docs | harness subcommands, `xtask`, Docker build, docs | 1 |

Interface beads (protocol types and the instance formats) land first, so lanes can then work in parallel.

### 9.3 Phase 0 beads

| Bead | Content | Lane |
|---|---|---|
| P0-01 | Startup stage log and PSS sampler in a minimal wgpu + sctk binary, FIFO | Platform |
| P0-02 | SPIR-V precompile in `xtask`, disk pipeline cache keyed by driver and device | Platform |
| P0-03 | Cold and warm start runs, 50 each, both GPUs (office run scripted) | Platform |
| P0-04 | Alpha mode (spike log) and pacing probe at four refresh rates on a nested display | Platform |
| P0-05 | `ext-background-effect-v1` on a layer surface, with a per-frame region | Platform |
| P0-06 **D** | Slug port design from `hb-gpu`: its blob format in a storage buffer or texture, dilation, contrast uniforms | Text |
| P0-07 | Bring `hb-gpu`'s WGSL shader and encoder into Rust, offscreen, supersampled reference comparison (about 1 week [G]) | Text |
| P0-08 | Revision stream recorder in the daemon with final texts, 20 recorded dictations, stored per 4.12 | IPC |
| P0-09 | Word diff prototype on the corpus, churn and final-swap report | Text |
| P0-10 | harfrust and line fitter cold timing; fontique lazy timing | Text |
| P0-11 | shm first frame on a second surface, crossfade to Vulkan | Platform |
| P0-12 | Particle formation look test from the spike's particle mode | Text |
| P0-13 | Output detection on KWin | IPC |
| P0-14 | Hinted small-text contact sheet against KDE | Text |
| P0-15 **D** | Failure matrix review and test plan | Lead |
| P0-16 | Phase 0 report and target confirmation | Lead |

### 9.4 Phase 1 beads (about 33)

- **Interfaces:** crate skeletons; `osd-protocol` types with the real phases and `Text.source`; frame uniform block; glyph and shape instance formats with reserved effect, emissive and transform fields. (3 beads)
- **Platform:** layer surface with fractional scale and viewporter; oversized surface sized from the output and placement tokens; frame scheduler with settle detection; presenter with FIFO and latency 1; pixel snapping in the vertex stage; init thread and shm first frame; prewarm, probe and diagnose; supervisor, launcher and GTK fallback with the failure matrix tests. (8 beads)
- **IPC and daemon:** socket server with delivery rules; revisions, final text and real phases; progress, output method and Enter flag; telemetry subset, paragraph markers and config; two-way heartbeat; CLI time stamp and output name. (6 beads)
- **Text:** font blob bake with checksum; Slug pass in production form with cap-height snapping; hinted small-text path; harfrust adapter and line fitter; word model with stability and final swap **D**; animated reflow, ticker window and caret; tabular figures, tracked caps and digit roll. (7 beads)
- **Shapes and design:** SDF uber-shader with hairlines, shadows and masks; path fill and trim for icons; level visuals, progress and spinner; spring, smoother, track and statechart core; design 12 function with flags, tokens, Plasma mapping and reduced motion. (5 beads)
- **Harness, packaging and docs:** offscreen render, scrubber, contact sheets and `--dump-state`; golden runner with text quality and accessibility suites; bench; Docker build, glibc and instruction checks, licence audit; user manual, configuration guide, troubleshooting, `--help`. (3 beads, the last two lanes share)
- **Gate:** phase 1 exit review with numbers, contact sheets and Oystein's side-by-side. (1 bead)

### 9.5 Phase 2 to 6 beads (rough)

| Phase | Beads [G] | Main groups |
|---|---|---|
| 2 | about 35 | Component layer decision **D**; animators and selectors; variable axes; decorations; confidence proxy; dot-matrix font, dot assembly and scramble; widgets and telemetry; capsule morph and icon morph; effect path, gradients, analytic glow, bloom pyramid, rim and sheen; quality tiers; four parent functions plus ticker and tall; token files for the variants; lavapipe CI; default-design review **D** |
| 3 | about 18 | Particle core; forces and noise; emitters; glyph point sampler; text SDF; formation and handover **D**; bursts; trails and feedback buffer; motion blur; 2.5D tilt and 3D particles; 09 function and tokens; readability measurement |
| 4 | about 10 | Glass design check **D**; glass material; blur region that follows the panel; glass-focus motion; reflection and light cone; glass token files; wallpaper contact sheets |
| 5 | about 14 | parley behind the adapter; bidi in the word model and reflow; full fallback; COLRv1; text on path; outline morph; optical margins and hanging punctuation; HDR probe |
| 6 | about 15 to 25 | Depth pass and ordering **D**; raymarched shapes; extruded text; scene reactions through the text SDF; first 3D design |

### 9.6 How a phase runs

1. Lead writes the phase plan from this roadmap and turns it into beads with `blockedBy` links (interfaces first).
2. Decision beads (**D**) get a short `fable` consult brief; high-stakes ones also get an Astra check.
3. Swarm of 4 to 6 agents takes ready beads by lane. File reservations through agent mail.
4. After the swarm: grep for leftover stubs, run the full test suite, goldens and bench.
5. Phase gate review with numbers against section 3 and contact sheets for Oystein.

## 10. Decisions on review points not taken

These review points were considered and not adopted, or adopted in a different form.

| Review point | Decision | Why |
|---|---|---|
| Tokens in RON (`review-fable.md` 3.1) | TOML instead | The voxtype config is TOML and users will write token overrides by hand. One format for both. Scenarios stay RON because only tools write them. |
| Follow Plasma font size and DPI settings for overlay text (`review-completeness.md` 4.2) | Not automatic; a `scale` token with a 10 px floor for the smallest label | The overlay is a fixed instrument design whose layout breaks when one font size changes alone. A single scale keeps proportions. Revisit if a user asks. |
| Screen reader support through an AT-SPI live region (4.4) | Not in phases 1 to 4 | The overlay is click-through and unfocusable by design; the daemon's sound cues and an optional notification cover the non-visual channel. Open question 12 asks Oystein. |
| Wallpaper snapshot behind glass (6) | Rejected | It is wrong whenever a window sits behind the overlay, which is nearly always during dictation. |
| Lens-flare streaks, stroked and chrome text, vertical CJK, prompter ramp, four-edge surfaces, soft-body warp, perspective grid, hazard stripes, procedural aurora (1.4) | Rejected or deferred, listed in `CAPABILITIES.md` section 6 | No picked design needs them. |
| Full RTL support in phases 1 to 4 (8) | Deferred to phase 5 | It needs bidi in the word model, reflow, caret and ticker. Phases 1 to 4 shape right-to-left runs but do not reorder mixed lines; this is written in the user docs. |
| Meeting mode (`src/meeting/`) in the overlay (1.2) | Out of scope | Meeting transcription is a different product surface with its own output. |
| Both GPUs as hard gates (3) | Home GPU is the gate; office GPU is a scripted smoke run within a week | Gates should not wait on Oystein being at the office. |
| Coverage target of mean under 1/255, max under 6/255 (Fable section 6) | Replaced by "no regression from the phase 0 number" | A correct Slug port may not meet an exact-area bar while looking right. |
| Keep lavapipe goldens in phase 1 | Moved to phase 2 | No CI for the crates exists yet; NVIDIA goldens cover phase 1. |
| Inspector socket and TUI | Replaced by `--dump-state` | Same information for agents, no socket, no text leak from a live session. |
| Design hot reload and the binding model in phase 1 | Moved to the phase 2 decision bead | The format would be designed from one design. |
| 12-serif in phase 1 | Moved to phase 2 | It needs the second font decision. |

## Sources

- Planning inputs: `/var/tmp/claude/renderer-plan/CAPABILITIES.md`, `research-typography.md`, `research-vector.md`, `research-effects.md`, `research-particles.md`, `research-animation.md`, `research-platform.md`, the six `mockups-*.md` files, `review-fable.md`, `review-completeness.md`.
- Local code: `~/work/voxtype/Cargo.toml` (launcher and frontends), `~/work/voxtype/src/state.rs` (daemon states), `~/work/voxtype/src/config/schema.rs` (`on_demand_loading`), `~/work/voxtype/src/bin/voxtype_osd_native/main.rs`, `~/work/voxtype/src/bin/voxtype_osd_gtk4.rs`, `~/work/voxtype/src/audio/levels.rs`, `~/work/voxtype/src/osd/ipc.rs`, `~/work/voxtype-osd-spike/Cargo.toml` and `src/` (read only).
- Slug: https://terathon.com/blog/decade-slug.html ; reference shaders and README: https://github.com/EricLengyel/Slug
- skrifa hinting: https://github.com/googlefonts/fontations/blob/main/skrifa/README.md
- Parley and HarfRust: https://linebender.org/blog/tmil-25/
- Vello: https://github.com/linebender/vello
- Bloom: https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/
- KWin pacing history: https://invent.kde.org/plasma/kwin/-/work_items/124
- WCAG 2.3.1: https://www.w3.org/WAI/WCAG22/Understanding/three-flashes-or-below-threshold.html
- Protocols on this KWin: `wayland-info` on `oystein-home-primary`, 2026-10-26, as reported in `review-fable.md`.
