# Dev Loop Commands and Build Timings

One command per step, run from the repo root. `just --list` prints them. Every cargo
call goes through `heavy-build`, so a step may wait in its queue when memory is short.

| Step | Command | Typical time after a one-line edit |
|---|---|---|
| Type-check everything | `just check` | 0.3 s |
| CPU tests, whole workspace | `just test` | 0.8 s |
| CPU tests, one crate | `just t vo-core` | 0.4 s |
| Shader assemble and naga validation | `just shaders` | 0.4 s |
| GPU golden images | `just test-gpu` | 1.0 s |
| Rewrite goldens | `just golden-update` | 1.0 s |
| Doc tests (nextest skips them) | `just doctest` | |
| Phase 0 example in release | `just bench p0_slug` | 1 s build |
| Clippy and rustfmt | `just lint` | |
| Budget gate: heap, cold first frame, idle frames | `just budget` | 12 s (3 live runs in a nested KWin) |
| Format | `just fmt` | |
| All of the above | `just all` | |
| Preview a scene, scrub, reload | `just preview title_card` | 0.25 s to a window; see below |
| voxtype fork: type-check the GTK overlay | `just osd-check` | 0.5 s (bin edit), 1.8 s (lib edit) |
| voxtype fork: build the GTK overlay | `just osd-build` | 1.2 s (bin edit), 11.6 s (lib edit) |
| voxtype fork: overlay unit tests | `just osd-test` | 12.6 s (lib edit) |

`just test` and `just t` pass extra arguments to nextest, for example `just test words`
or `just t vo-render -E 'test(/slug/)'`.

## Tests

- CPU and GPU tests are separate. `.config/nextest.toml` has two profiles. The
  default profile leaves out every test binary named `gpu_*`, so `just test` never
  needs a GPU. The `gpu` profile runs only those binaries, one at a time.
- A new GPU test goes in `crates/<crate>/tests/gpu_<name>.rs`. The shared code in
  `crates/vo-render/tests/support/mod.rs` gives a headless Vulkan device, an offscreen
  render to a buffer, and the golden compare. No window is opened, so it is safe on
  the real desktop.
- Goldens are 8-bit grayscale PNGs in `crates/vo-render/tests/goldens/`. A test
  fails when more than 0.05 % of pixels differ by more than 2 levels (out of 255).
  Each run writes `<name>.actual.png`, `<name>.diff.png` (differences in red, 8 times
  brighter) and `contact-sheet.png` (golden, actual, diff side by side) to
  `/var/tmp/claude/vo-golden/`, and prints the contact sheet path.
- To change a golden on purpose, run `just golden-update`, look at the contact
  sheet, and commit the PNGs with the code change. The recipe sets
  `HEAVY_BUILD_ALLOW_RERUN=1` because heavy-build refuses to rerun a command that just
  failed twice when only an environment variable changed.
- The goldens were made on the NVIDIA desktop driver. Another GPU or driver may need
  its own goldens or a wider tolerance. This was not tested.
- `just test` builds only lib and integration test targets (`--lib --tests`), not
  the examples. `just check` covers the examples.

## Build profile

The root `Cargo.toml` changes the dev and test profiles only. Release is unchanged, so
release timings from the phase 0 examples stay comparable with older numbers.

- All dependencies build at `opt-level = 1` with `debug = false`. Test binaries link
  faster without the dependency debug info, and wgpu and naga start the GPU tests
  faster.
- `vo-render` builds at `opt-level = 1`. The HarfBuzz encoder test drops from 0.6 s to
  0.08 s, and it was the slowest test in the CPU suite.
- `vo-core` stays at `opt-level = 0`, because it is edited most and its tests take
  under 10 ms anyway.

## Measured times

Measured on 2026-10-26 in a separate worktree of this repo, with the same edits
before and after. Each edit adds a small function near the top of the file, so rustc
has to redo real work. Times are wall time of the whole `heavy-build` call, the
faster of two runs, and include about 0.17 s of heavy-build overhead per call.
Rust 1.99.1, 20 cores, memory under pressure during the runs.

### This repo

| Step | Before | After |
|---|---|---|
| vo-core edit, then `cargo nextest run -p vo-core` | 0.48 s | 0.40 s |
| vo-render edit, then `cargo nextest run --workspace` (CPU) | 1.77 s | 0.78 s |
| vo-render edit, then GPU golden test | 1.71 s | 0.97 s |
| shader edit, then `cargo test -p vo-render --lib` | 0.46 s | 0.44 s |
| vo-core edit, then `cargo check --workspace --all-targets` | 0.35 s | 0.33 s |
| vo-core edit, then `cargo test --workspace` (with doc tests) | 2.43 s | 1.08 s |
| No edit, CPU suite with nextest | 0.97 s | 0.36 s |
| No edit, GPU golden test | 1.02 s | 0.51 s |
| Clean `cargo check --workspace --all-targets` | 10.7 s | 14.7 s |
| Clean `cargo test --workspace --no-run` | 16.2 s | 19.8 s |
| vo-render edit, then release build of `p0_slug` | 0.95 s | 1.10 s (no change made; noise) |

The clean builds are about 4 s slower, because dependencies now get some
optimisation. A clean build happens once per toolchain update or worktree, so the
loop gain is worth it.

### Options measured and not adopted

| Option | Result | Why not |
|---|---|---|
| mold through clang (`.cargo/config.toml`) | vo-render edit to GPU test 2.0 s with opt-1 deps (0.94 s without mold), 2.98 s with default deps | Rust 1.99 already links with rust-lld by default, and that was faster here in every linking step |
| Dependencies at `opt-level = 3` | Same loop times as opt-level 1 | Clean check 20.6 s against 16.2 s |
| All of our crates at `opt-level = 1` | vo-core edit to tests 0.94 s (0.45 s at opt-level 0) | Only `vo-render` gets it |
| `incremental = true` for release | Release example rebuild 0.55 s against 0.95 s | It changes code generation, so phase 0 release timings would no longer be comparable |
| sccache | Not measured | It cannot cache incremental crates, which are exactly our own crates. It only helps a clean build of dependencies in a new worktree, and it needs a resident server |
| A separate WGSL check tool with no wgpu | Not built | The existing naga unit tests already rebuild and run in 0.44 s after a shader edit |

### voxtype fork (GTK overlay)

Measured in a worktree of the fork with its target directory copied by reflink, so
only the voxtype crate rebuilt. The edit is a new function in
`src/bin/voxtype_osd_gtk4.rs` (bin edit) or `src/osd/theme.rs` (lib edit). Nothing in
the fork was changed.

| Step | Bin edit | Lib edit |
|---|---|---|
| `cargo build --release --features osd-gtk4 --bin voxtype-osd-gtk4` | 15.6 s | 58.5 s |
| `just osd-build` (dev profile) | 1.2 s | 11.6 s |
| `just osd-check` | 0.5 s | 1.8 s |
| `just osd-test` (lib tests named `osd::`) | | 12.6 s |

The release profile in the fork uses `lto = true` and `codegen-units = 1`, which makes
every release build of the overlay take 15 to 60 s. For a look at the overlay, use
`just osd-build`. Use the release build only for the final check, and the localbuild
recipe for the installed package. The fork's dev profile has `opt-level = 1` for
every crate, including voxtype itself, which is why a lib edit still takes 11 s. That
setting comes from upstream ("whisper still usable"), so it was left alone.

## rust-analyzer

rust-analyzer runs `cargo check` in the same `target/` directory as heavy-build. Keep
it that way:

- With the same flags (`--workspace --all-targets`, no features) it reuses what
  `just check` built, and the other way round.
- While it runs it holds cargo's lock on `target/`, so a heavy-build build waits for it,
  usually under 1 s after an edit. Its checks also skip heavy-build admission, but an
  incremental check here uses about one core for well under a second.
- A separate target directory for rust-analyzer (`rust-analyzer.cargo.targetDir`) would
  remove the lock waits, but it means a second full build of all dependencies on disk.
  The machine rules forbid new target directories, so do not set it.
- Leave `rust-analyzer.check.command` at `check`. Run clippy through `just lint`, not on
  every save.

## Shader iteration

The WGSL files in `crates/vo-render/src/wgsl/` and `crates/vo-render/src/text/wgsl/` are
pulled in with `include_str!`. After an edit, `just shaders` rebuilds the vo-render test
binary and runs the naga parse and validate tests in about 0.4 s. `just test-gpu` then shows
the result on the contact sheet.

For live reload, open a scene in the preview tool with `--shaders crates/vo-render/src/wgsl`
(below). With `VO_SHADER_DIR` set, `vo_render::hot::wgsl` reads the shape, glyph, composite,
dot, bloom and vector shaders from that folder, and `Renderer::reload_shaders` builds them
again inside a validation error scope: a broken shader prints the naga error and the old
pipelines stay. The Slug glyph shader in `text/wgsl/` and the particle shaders do not go
through `hot` yet. Reloading the shape shader took 3 ms.

## Preview and scrub tool

`vo::preview` (feature `preview`, on for every vo example) opens any `App` with a `Scenario`
in a window: play, pause, step one frame, speed 0.25x to 2x, a loop range, jumps to named
markers, and a track to scrub by dragging, with ticks for every event and marker. Overlays
show node bounds, spring targets against current values with the moving channels listed, a
frame time graph, and display list counts. An example opts in with a few lines at the top of
`main` (see the `vo::preview` module docs; the title card and the toast do it). A scene that
is not an app yet, such as the 3D stack, uses `Preview::custom` with a function that draws
frame `t` into a texture; it gets the window, scrubbing and contact sheets, not the overlays.
`Preview::sheet` gives a contact sheet in a test (`crates/vo/tests/gpu_preview.rs`).

| Step | Command |
|---|---|
| Open a scene, rebuild and replay on Rust edits | `just preview title_card -- --at 2.5 --overlay springs` |
| The same, without the launcher | `cargo run -p vo --example title_card -- --preview` |
| Token reload | add `--tokens crates/vo/examples/title_card.toml` |
| Shader reload | add `--shaders crates/vo-render/src/wgsl` |
| Contact sheet | `just sheet title_card /var/tmp/claude/sheet.png --frames 12` |
| Golden check of a sheet | add `--check golden.png` (exit 1 when more than 0.05 % of pixels differ by more than 2 levels) |
| Seek timings | `just preview-bench title_card` |
| Drive a running window | `target/debug/vo-preview ctl seek 2.5`, `play`, `step -1`, `marker out`, `loop 1 3`, `overlay bounds on`, `shot out.png`, `state`, `quit` |
| Match a mock-up | add `--reference mock.png --ref-crop 430,560,690 --ref-mode diff` |
| Score a frame against a mock-up, headless | `cargo run -p vo --example title_card -- --compare mock.png --at 2 --crop 430,560,690 --out cmp.png` |
| Effect extents and GPU time per pass | add `--overlay effects` |

Run the window only inside a nested `kwin_wayland --virtual` when an agent starts it.

**Seek.** Nodes that finish leaving are dropped, so the scene cannot be sampled before its
last event. `Player` keeps one driver: a forward seek applies the events up to the new time
as export does; a seek back that crosses no event samples as it is, because channels keep
their history; a seek back across an event replays the scenario from 0. A test compares
every frame of a backwards and a random scrub with a forward run. Measured on the 10 s title
card (41 events) in release: worst seek 0.77 ms, a full replay from 0 about 0.3 ms; in a
debug build up to 3.5 ms. Periodic snapshots are not built, because at this cost they would
not help, and they need `Clone` on the app and the scene.

**Reload.** A token file change replays from 0 with the new tokens to the same time (5 to
7 ms on the title card). A Rust change goes through the `vo-preview` launcher: it rebuilds
the example, asks the window for its state over the control socket, closes it and starts the
new build at the same time, play state, loop, overlays and window size. It watches only what
the example can depend on. Measured: the window is up 0.25 s after start; edit to window
1.5 s when cargo needs 1.0 s. In this shared checkout heavy-build often queues the build
behind other lanes, which added 5 to 35 s.

**Reference images.** `--reference PNG` (or `ctl ref PNG`) puts a mock-up with the stage, to
match a theme to it. `--ref-crop X,Y,W[,H]` picks the part of the PNG that shows the scene, in
the PNG's pixels; without H the height follows the stage's aspect. The crop is resampled to
the stage's pixels in linear light, so one reference pixel lies under one stage pixel at any
fit. Modes: `over` and `under` at an opacity (the `-` and `+` buttons, `ctl ref opacity 0.3`),
`split` (the stage left of a line, the mock-up right of it, `ctl ref split 0.4`), and `diff`, a
heat map of the largest channel difference, 4x, black where they match. For the difference the
stage goes over a backdrop where it is transparent: the mock-up itself by default (only what
the stage draws counts), or a flat colour (`--ref-backdrop #101418`), which also counts what
the stage does not draw. `ctl ref score` and the headless `--compare` print the mean
difference, its 95th percentile and the share of pixels off by more than 10%; `--compare
--out` writes the frame, the mock-up and the heat map side by side, and `--max-error PCT`
makes it a gate. The reference is kept in the state file, so it survives a rebuild.

**Effects overlay.** `--overlay effects` (the Effects button) draws every effect's extent over
the stage: offscreen layers with their content box, their bloom's reach and their texture size,
and the glows, drop shadows and glyph halos and blurs that the shape and glyph shaders draw
without a surface. An extent that leaves the surface is red: that glow is cut off at the edge.
Its panel lists the GPU time of each section of the frame, slowest first (`Renderer::
last_timings`, when the device has timestamp queries; the host asks for them in dev builds), and
the stats overlay shows the four slowest.

**Drawing.** The window draws the stage and its own chrome into two textures with the normal
renderer and composites them, so it never merges display lists. The stage is drawn at its
fit scale (at most 2x, or 1:1) and copied pixel for pixel over a checkerboard.

## Budget gate

`just budget` (`scripts/budget.sh`) is the M1 budget check while there is no CI. It renders the
toast offline twice and requires byte-identical frames, then runs it three times live, each in a
fresh private `kwin_wayland --virtual` session, hovers it and clicks its close button with
`fake_click`. It fails when:

- the own heap peak (the toast's counting allocator) is over `VO_BUDGET_HEAP_MB`, default 15;
- the median cold first frame on screen since exec is over `VO_BUDGET_FIRST_FRAME_MS`, default 300;
- any run draws more than `VO_BUDGET_IDLE_FRAMES` frames while idle, default 0. A frame is idle when
  it is the first after a settle, draws the same display list as before the settle, and the host did
  not ask for it.

`VO_BUDGET_RUNS` sets the number of live runs and `VO_BUDGET_ARGS` passes extra toast arguments,
for example `--cpu-first-frame`. It needs `kwin_wayland` and a Vulkan GPU but no real display, and it
takes about 12 s, so it is part of `just all`. A heavy build running at the same time can slow the
cold start; the median of three runs absorbs one slow run.

**GPU time per effect.** The last step runs the `effect_cost` example: the same card with one
effect at a time, at 600x300 and at 1920x1080, 60 frames each, timed on the GPU with timestamp
queries. It writes the table to `$VO_BUDGET_DIR/effect-cost.md` and fails when an effect takes
more than `VO_BUDGET_EFFECT_MS` (default 2) per frame at 600x300. A device without timestamp
queries skips it. `cargo run --release -p vo --example effect_cost` prints the full table with
120 frames; `-- --preview` shows each case for 2 s, with markers.

Measured on an RTX 3060 Ti (median GPU ms per frame; the plain card is 0.062 at 600x300 and
0.191 at 1920x1080, and the other columns are what the effect adds):

| Effect | +ms at 600x300 | +ms at 1920x1080 | Pooled textures at 1920x1080 |
|---|---:|---:|---:|
| glow paint | 0.007 | 0.096 | none |
| drop shadow | 0.009 | 0.125 | none |
| text halo | 0.152 | 2.554 | none |
| text blur | 0.026 | 0.196 | none |
| offscreen layer | 0.006 | 0.000 | 3.9 MB |
| bloom, radius 18 | 0.051 | 0.168 | 42.8 MB |
| bloom, radius 60 | 0.084 | 0.170 | 42.8 MB |
| trail | 0.014 | 0.085 | 33.2 MB |
| motion blur | 0.010 | 0.088 | none |
| CRT style | 0.009 | 0.022 | 3.9 MB |
| glitch | 0.008 | 0.009 | 3.9 MB |

The text halo is the one to watch: it scales with glyph area times halo radius, and at full HD
it is over the 2 ms frame budget on its own. Bloom and trails cost little time but a lot of
texture memory, which the own-heap gate does not see.

`Renderer::set_profiling(true)` turns the timestamps on in any renderer whose device has
`vo_render::profile::timestamp_features()`; `Renderer::last_timings()` then gives the GPU time
of each section of the last frame (each surface's passes, each bloom pyramid, the final pass),
and `Renderer::frame_report()` lists its surfaces: which layer, what it covers, its texture and
its bloom's reach.
