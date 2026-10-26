# GPU Overlay Failure Cases and Test Plan (P0-15)

Bead P0-15. For each way the GPU overlay can fail, this says what the user sees, what the
code does, and which test proves it. It builds on the roadmap (sections 3.7, 4.1, 4.10,
4.11, 4.13 and the decisions in section 8) and on the phase 0 reports in this folder.
Written 2026-10-26. [V] marks a fact checked in code or by a run, [I] an inference or a
design choice, [G] a guess.

## 1. Rules every case follows

1. **Dictation never depends on the overlay.** Audio, transcription and paste run in the
   daemon and its worker. An overlay crash, hang or GPU loss never stops or delays the
   paste. [V for the process split, roadmap 4.1; I for the rule]
2. **Never an opaque rectangle, never a stuck panel.** If the overlay cannot draw
   correctly it shows nothing. If it loses the daemon it fades out within 2 s and exits.
   [V, roadmap 3.7]
3. **Fallback order:** GPU overlay, then the CPU-drawn frame for the rest of the current
   recording, then the GTK overlay from the next recording, then no overlay. With no
   overlay the user still has the daemon's audio cues and the optional desktop
   notification (decision 13). [I]
4. **Every fallback is logged once and counted.** `voxtype-osd-gpu --diagnose` prints the
   counts, so a silent fallback cannot hide for a week (roadmap 3.7, "no fallback caused
   by a crash in one week"). [I]
5. **Tests never touch the user's session.** They run offscreen, on a nested
   `kwin_wayland --virtual` or a headless Sway, with a private `XDG_RUNTIME_DIR`, and
   never against the real `voxtype.service`. [V, repo `CLAUDE.md`]

## 2. Test tools this plan needs

None of these exist yet. Each is small and is named by the tests below. [I]

| Tool | What it does | Lane |
|---|---|---|
| `fake-daemon` | An `osd-protocol` server in Rust that drives the overlay from a script: phases, `Text` deltas, the final, heartbeats, and faults (stop heartbeats, close the socket, send garbage, delay the final). Scripts come from synthetic revision logs in the format of the voxtype recorder (`diff-report.md`). | IPC |
| `VO_FAULT` | A debug-build switch in the overlay: `device-lost@frame=N`, `vulkan-init-fail`, `alloc-fail`, `alpha-opaque`, `hang-render`. Release builds ignore it. | Platform |
| Nested displays | `kwin_wayland --virtual` (one or two outputs, chosen scale) and headless Sway (`swaymsg create_output`, `output <name> unplug`) for hotplug. | Platform |
| `stub-engine` | A test-only engine in the voxtype live worker that answers after a set delay, never, or with an error, so the final-text cases run with no GPU model. voxtype's remote engine cannot stand in: it is a Whisper mode, and the live preview is off when the engine is Whisper. [V for the last point, `live_preview_active()` in voxtype] | IPC |
| `frame-log` | The overlay's existing stage log plus `wp_presentation` times, read by the tests to check "visible within N ms" and "faded within 2 s". | Platform |
| `--diagnose` counters | Restart and fallback counts, read by the tests after each fault. | Platform |

## 3. The cases

### 3.1 GPU lost or driver reset

**Triggers.** wgpu reports device lost or surface lost: an NVIDIA driver reset (Xid), a
GPU hang caught by the kernel, a driver upgrade under a warm process, or a suspend and
resume that drops the device. [V for the wgpu callbacks; I for the trigger list]

**What the user sees.**
- Mid-recording: the panel disappears for under 1 s and comes back with the same words,
  because the new overlay gets a `Snapshot` on connect (`protocol.md` 4.1). The words do
  not replay their entry motion. [I]
- If it happens 3 times within a minute, the panel comes back as the GTK overlay instead,
  and stays GTK until the daemon restarts or the driver version changes. [V for the
  limit, roadmap 3.7; I for the reset rule]
- A driver reset can also kill the Cohere worker on the same GPU. Then the paste uses the
  preview text, and the status line says so (case 3.5). [V for the preview fallback,
  `src/transcribe/worker.rs` in voxtype]

**What the code does.**
- The overlay fades nothing (the device is gone), logs one line and exits with a "device
  lost" exit code. [V, roadmap 4.11]
- The daemon respawns it at once if a recording is running, otherwise at the next
  recording. A warm overlay compares `/proc/driver/nvidia/version` at each show and
  exits if it changed (roadmap 4.10). [V for the design]
- **Gap:** the existing OSD supervisor in voxtype (`src/osd/supervisor.rs`) backs off 1 s
  to 30 s and stops retrying after 3 failures within 5 s. The GPU overlay needs its own
  policy: restart within 1 s, at most 3 per minute, then switch the launcher to GTK. [V
  for the current policy]

**Tests.**

| Test | How | Pass |
|---|---|---|
| `fault_device_lost_mid_recording` | `fake-daemon` plays a 20 s script; overlay runs with `VO_FAULT=device-lost@frame=120` on a nested KWin | New overlay visible within 1 s of the exit (`frame-log`); its words equal the words before the fault; dictation script runs to the end |
| `fault_device_lost_restart_limit` | Same fault on every start, 4 starts in 60 s | Starts 1 to 3 are GPU; the 4th start is GTK; `--diagnose` shows 3 restarts and 1 fallback |
| `fault_driver_version_change` | Warm overlay; test changes the version string it reads (a test path for `/proc/driver/nvidia/version`) | Overlay exits at the next show; the next start is a fresh GPU overlay |
| `fault_suspend_resume` (manual, phase 1) | Real `systemctl suspend` on the test machine, recording before and after | Second recording shows the GPU overlay or a logged fallback, never a frozen panel |

### 3.2 Vulkan unavailable: fall back to the CPU frame

**Triggers.** No Vulkan loader or ICD, a VM or remote session, only a CPU adapter
(lavapipe or llvmpipe, refused in production), or Vulkan init failing after the window
is up. [V for the adapter rule, roadmap 4.1]

**What the user sees.**
- When the launcher's `--probe` already says "no usable GPU": the GTK overlay, from the
  first recording. Nothing GPU-specific ever shows. [V, roadmap 4.11]
- When init fails after the first frame: the CPU-drawn panel from the first frame stays
  up for the rest of this recording. It shows the panel, status line and transcript text
  with no motion, redrawn at most 10 times a second. From the next recording the GTK
  overlay is used, until the driver version changes. [I]

**What the code does.**
- The first frame is drawn on the CPU into a `wl_shm` buffer on its own surface while the
  init thread creates the Vulkan device (roadmap 4.1, Q9). If the init thread fails, the
  overlay keeps that surface and draws later frames on the CPU too. [I]
- The CPU text path is the hinted small-text rasteriser from P0-14 (skrifa hinting plus a
  scan converter), at one size, so no second text stack is needed. [I]
- The overlay writes a "gpu failed" marker with the driver version to its state
  directory. The launcher reads it and picks GTK while the version matches. [I]

**Tests.**

| Test | How | Pass |
|---|---|---|
| `probe_no_icd` | `voxtype-osd-gpu --probe` with `VK_ICD_FILENAMES` pointing at an empty file | Exit code "no usable GPU"; launcher unit test picks GTK for that code |
| `probe_cpu_adapter_refused` | `--probe` with lavapipe forced | Same exit code in a release build; accepted only with the test override |
| `fault_vulkan_init_after_first_frame` | `VO_FAULT=vulkan-init-fail`, `fake-daemon` script on a nested KWin | Frame 1 visible; CPU frames keep showing new words (golden of the CPU panel at 3 s); no window flicker; marker file written |
| `launcher_honours_gpu_marker` | Launcher unit test with a marker for the current and for an older driver version | GTK for the current version; GPU for the older one |

### 3.3 Compositor without layer shell

**Triggers.** GNOME (Mutter has no `zwlr_layer_shell_v1`), an X11 session, or another
compositor that lacks it. [V for GNOME and X11, roadmap 4.11]

**What the user sees.** The GTK overlay where it works, otherwise no overlay. Dictation,
audio cues and paste work as before. The GPU overlay never opens a normal window as a
substitute, because a normal window takes focus and would receive the pasted text. [I]

**What the code does.**
- The launcher reads the Wayland registry once and picks `gpu` only when layer shell is
  present (roadmap 4.11). Missing optional protocols degrade instead: no fractional scale
  means integer scale, no presentation time means frame-callback time, no background
  effect means fake glass. [V for the design]
- **Unknown:** whether the GTK overlay itself works on GNOME. Not checked in phase 0. [G]

**Tests.**

| Test | How | Pass |
|---|---|---|
| `launcher_protocol_list` | Unit test: the launcher's choice for a list of global names (KWin, Sway, Hyprland, GNOME, empty) | `gpu` only when `zwlr_layer_shell_v1` is listed |
| `probe_headless_sway` and `probe_headless_hyprland` (phase 1) | `--probe` on each headless compositor | Exit 0 with a log of which optional protocols are missing |
| `no_wayland_display` | `--probe` with `WAYLAND_DISPLAY` unset | "no usable display" exit code; launcher falls back; no panic |

### 3.4 Daemon crash in the middle of a dictation

**Triggers.** The daemon process dies (panic, OOM kill, `kill -9`) while recording or
while finishing. systemd restarts it after 5 s (`Restart=on-failure`, `RestartSec=5`).
[V for the unit settings on the development machine]

**What the user sees.**
- The panel shows "Voxtype stopped, nothing was pasted" in the error style for about
  1.5 s, then fades; total under 2 s from the socket closing. [I for the text; V for the
  2 s target, roadmap 3.7]
- The audio of that dictation is lost, because the daemon held it. With live typing on,
  the words already typed stay in the app. [V for where audio lives; I for the user
  effect]
- The next hotkey press after the restart works normally.

**What the code does.**
- The overlay sees the socket close, or no daemon heartbeat for 2 s, runs
  `connection-lost` and exits (roadmap 4.10). [V for the design]
- The live worker sees its stdin close, finishes its own work and exits; its reply goes
  nowhere. [V, `run_live_worker` reads stdin until EOF] With
  `live_preview_corpus_dir` set it still saves the recording, so the words can be
  recovered by hand. [V]
- A warm overlay waiting with no surface also exits on the socket close, so a daemon
  restart never leaves an orphan. [I]

**Tests.**

| Test | How | Pass |
|---|---|---|
| `fault_daemon_socket_close` | `fake-daemon` closes the socket mid-script | Error frame shown, last frame presented within 2 s of the close, process exit code 0 |
| `fault_daemon_heartbeat_stop` | `fake-daemon` stops heartbeats but keeps the socket open | Same as above, 2 s after the last heartbeat |
| `fault_daemon_sigkill_e2e` (phase 1) | A test daemon under a private runtime dir and its own systemd test unit, fed from a WAV, killed with SIGKILL mid-recording | Overlay gone within 2 s; no `voxtype-osd-gpu` or worker process left after 10 s |
| `warm_overlay_orphan` | Warm overlay with no surface; `fake-daemon` exits | Overlay exits within 2 s |

### 3.5 The Cohere final arrives late or never

**Triggers.** The engine model loads slowly, the GPU is busy, the engine errors, or the
engine thread hangs. [V for the first three paths in the worker]

**What the user sees.**
- While waiting: the finishing state with the fraction and time left that the worker
  already reports (`S {"text", "fraction"}` lines, `Phase::Finishing` in
  `protocol.md`), with a spinner (decision 11). [V for the data]
- When the engine fails or the GPU has too little free memory: the preview text is
  pasted, and the status says "Pasted the live text". [V, the worker falls back to the
  preview and writes a "GPU busy" status]
- When the engine never answers: today the user waits until they cancel. **Gap:** the
  daemon has no deadline for the live final; its select loop waits for `Final` or
  `Ended` with no timeout. [V, `src/daemon.rs`] Proposed: a deadline of 10 s plus 0.5 s
  per second of audio, after which the daemon pastes the preview text, says so in the
  status line, and kills the worker. [I for the rule; G for the numbers]
- A final that arrives after the deadline or after a cancel is dropped: its
  `recording` number no longer matches (`protocol.md` 4.2). [I]

**What the overlay does.** `settle-to-final` runs only when a `Text` with
`source: Final` arrives. With a preview final it has nothing to move (diff report: the
preview final equals the screen). With a Cohere final it moves about 60 % of words and
removes some (diff report). [V for the numbers]

**Tests.**

| Test | How | Pass |
|---|---|---|
| `final_late_progress` | `stub-engine` answers after 5 s; real live worker; overlay on `fake-daemon` relay or real daemon in a private runtime dir | `Finishing` frames show a rising fraction; final text pasted; overlay shows `settle-to-final` then `Pasted` |
| `final_engine_error` | `stub-engine` returns an error | Preview text pasted; status says the live text was pasted; no error dialog |
| `final_never` (after the daemon gap is fixed) | `stub-engine` never answers | Preview text pasted at the deadline; worker process gone; status line says so |
| `final_after_cancel` | `fake-daemon` sends `Cancelled`, then a `Text` final for the same recording | Final ignored; overlay stays in the cancelled state and hides |
| `final_swap_replay` | `revdiff` over the synthetic corpus in CI, plus the private corpus locally | Ids kept for unchanged words (roadmap 3.4 "final text swap"); counts within 5 % of `diff-report.md` on the private corpus |

### 3.6 Monitor hotplug

**Triggers.** The output that shows the panel is unplugged or turned off, a new output is
plugged in, or the active output changes during a recording. [I]

**What the user sees.**
- Output with the panel removed: the panel disappears and reappears on the output KWin
  now considers active, within about 2 frames, with the same words. Recording goes on.
  [I; V for "hide, keep recording, re-create", roadmap 4.11]
- New output plugged in or focus moved to another output during a recording: the panel
  stays where it is until the next recording. It never jumps between screens mid
  dictation. [I]

**What the code does.**
- The overlay creates its layer surface with no output, so KWin places it on the active
  output (`output-detection.md`). [I, not checked against KWin source]
- On the layer surface `closed` event or a `wl_output` removal of its output, the
  overlay destroys the surface and swapchain and creates new ones on the next frame. The
  device, pipelines and glyph store stay. [I]
- The new output may have another scale and refresh rate: case 3.7 applies, and frame
  pacing follows the new output's presentation feedback. [I]

**Tests.**

| Test | How | Pass |
|---|---|---|
| `hotplug_remove_active_output` | Headless Sway with two outputs; `fake-daemon` script; unplug the output with the panel at 3 s | New surface on the other output within 100 ms (`frame-log`); same words; no crash |
| `hotplug_add_output_no_jump` | Same, add an output and move focus to it at 3 s | Panel stays on its output until the script ends; next recording appears on the new active output |
| `hotplug_kwin_virtual` | Same as the first test under `kwin_wayland --virtual` with two outputs, if output removal can be scripted there [G] | Same pass rule |

### 3.7 Fractional scale

**Triggers.** Any scale other than 1: 1.25 on the development desktop (`Xft.dpi: 120`
per `text-report.md`), 1.5 and 1.75 on laptops; a scale change while visible, when the
panel moves to another output or the user changes the setting. [V for 1.25]

**What the user sees.** Sharp text and 1-pixel hairlines at every scale, the same layout
in logical pixels, and no jump when the scale changes, only a relayout on the next
frame. [V for the targets, roadmap 3.4]

**What the code does.**
- Uses `wp_fractional_scale_v1` (preferred scale in 120ths) and `wp_viewporter`. The
  buffer is `round(logical size * scale)` physical pixels; layout runs in logical units
  and snaps baselines to physical pixels when words settle (roadmap 3.4). [V for the
  protocols; I for the rounding rule]
- Slug text needs no re-rasterisation at a new scale; hinted small text (P0-14) is
  re-rendered for the new pixel size. [V for Slug being resolution independent,
  `slug-design.md`]
- Without fractional scale support: integer scale (the next whole number) with the
  viewport scaling down. [V for the fallback, roadmap 4.11]

**Tests.**

| Test | How | Pass |
|---|---|---|
| `golden_scale_set` | Offscreen goldens of the phase 1 scenarios at 1, 1.25, 1.5, 1.75, 2 | Golden match; hairline test (one row of full coverage, no partial neighbours) passes at each scale |
| `scale_change_while_visible` | Nested KWin; change the output scale from 1.25 to 1.5 at 3 s of a `fake-daemon` script | One frame of relayout; word positions in logical units unchanged; no frame with a buffer of the wrong size (`frame-log`) |
| `no_fractional_protocol` | Headless compositor without `wp_fractional_scale_v1` at scale 1.5 | Overlay renders at scale 2 and the viewport scales it; no crash; logged once |
| `text_quality_sweep` | Roadmap 3.4 sub-pixel sweep at 1.25 and 1.5 | Coverage varies under 0.5 % per glyph |

## 4. Summary

| Case | User sees | Main test | Gap found |
|---|---|---|---|
| GPU lost or driver reset | Panel gone under 1 s, back with the same words; GTK after 3 restarts a minute | `fault_device_lost_mid_recording` | OSD supervisor policy does not match the target |
| Vulkan unavailable | GTK overlay; or the CPU panel for this recording, then GTK | `fault_vulkan_init_after_first_frame` | CPU text path to build |
| No layer shell | GTK overlay or none; dictation unchanged | `launcher_protocol_list` | GTK overlay on GNOME not checked |
| Daemon crash | Error line, fade within 2 s; dictation lost unless the corpus is on | `fault_daemon_socket_close` | None |
| Cohere final late or never | Progress, then the final; or the live text pasted with a note | `final_late_progress`, `final_never` | No deadline on the live final in the daemon |
| Monitor hotplug | Panel moves to the active output only if its output goes away | `hotplug_remove_active_output` | Hotplug in nested KWin not checked |
| Fractional scale | Sharp text, 1 px hairlines, relayout without jumps | `golden_scale_set`, `scale_change_while_visible` | Scale 1.25 not in the phase 0 KDE check |

The other rows of roadmap 4.11 (render thread hang, version mismatch, malformed messages,
alpha mode, pipeline cache, font blob, missing glyph, revision bursts, allocation
failure) keep the response and test given there.

## 5. When the tests run

| Group | Where | When |
|---|---|---|
| Unit tests (launcher choice, marker file, protocol, word model) | `cargo test`, CI | Every commit |
| `fake-daemon` fault tests on a nested KWin | Local, private display | Before merging any change to the overlay process, IPC or present code |
| Hotplug and scale tests on headless Sway and nested KWin | Local | Phase 1 gate, then weekly |
| `stub-engine` final-text tests | Local, private runtime dir | With any change to the daemon's live path |
| End-to-end daemon kill and suspend tests | Local, manual for suspend | Phase 1 gate |

## 6. Work this plan creates

1. Build `fake-daemon`, `VO_FAULT` and `stub-engine` (IPC and Platform lanes, phase 1).
2. Daemon: a deadline for the live final (case 3.5). Small change in voxtype's live
   streaming path.
3. Daemon: a supervisor policy for the GPU overlay (case 3.1).
4. Overlay: the CPU-only mode after a failed Vulkan init (case 3.2).
5. Check the GTK overlay on GNOME, and whether `kwin_wayland --virtual` can remove an
   output (cases 3.3 and 3.6).
