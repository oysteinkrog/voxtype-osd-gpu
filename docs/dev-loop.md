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

The WGSL files in `crates/vo-render/src/text/wgsl/` are pulled in with `include_str!`.
After an edit, `just shaders` rebuilds the vo-render test binary and runs the naga
parse and validate tests in about 0.4 s. `just test-gpu` then shows the result on the
contact sheet. There is no shader hot reload yet: the phase 0 examples render once and
exit, so there is nothing to reload into. When a windowed example exists, reading the
shader files from disk at run time behind an environment variable would be the cheap
way to add it.

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
