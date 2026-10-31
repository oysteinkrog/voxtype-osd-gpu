# Redesign Plan: Every Look Runs Its Effects

Date: 2026-10-28. Status: plan; beads with label `redesign` (ids RD-nn in titles).

Inputs: the motion library (73 exhibits from MO-01 to MO-89, all built) and the 24 concept redesigns made with it. Both are in the Popup Effects Library gallery (https://claude.ai/artifact/E3J8iqQE9aAn7XdXQYjgeB). Local copies: redesign images in `/var/tmp/claude/fxlib/redesign`, effect clips in `/var/tmp/claude/fxlib/site/img/fx`.

## 1. Why

The motion library is complete, but the everyday popup shows almost none of it. Features turn on only through `--motion` or `VOXTYPE_OSD_VO_MOTION`, and the daemon passes neither. Each look should run the effects its redesign chose, and every look should feel alive through small idle motion, as the maintainer asked.

## 2. Work in three layers

1. **Foundations.** RD-01 per-look default sets and the `[osd] motion` setting. RD-02 the living-idle policy, frame pacing and a CPU budget per look. RD-03 slots for seven new looks. RD-04 the faint-letters bug. RD-05 an exit check, so no look can hang a show again.
2. **Looks.** For each of the 20 looks: one bead makes its four effects work in its material (FX), then one adds idle motion (IDLE). For each of the 7 new looks: one bead builds the look (LOOK), then one adds its effects and idle motion (FX).
3. **Finish.** RD-06 gate across all 27 looks, RD-07 station page and refreshed gallery, RD-08 package and install.

## 3. Default effect set per look

Feature names as `--motion` takes them. Effects in plain words are built in and need no name.

| Look | Concept | Default set |
|---|---|---|
| Swiss | none | `pause-marks`, `pitch-baseline`, `audio-ruler`, `fold-under` |
| Showcase | none | `particle-source`, `light-letters`, `variable-breath`, `depth-stack` |
| Type | none | `onset-ink`, `confidence-ink`, `width-escrow`, `margin-bell` |
| Amber | 07 | `pause-marks`, `margin-bell`, `onset-ink`, `fold-under` |
| Radial | 10 | `ring-reads`, `acoustic-receipts`, `audio-ruler`, `onset-joint` |
| Tron | 16 | `light-line`, Hand-off, `stroke-write`, `overflow-spine` |
| Phosphor | 06 | `beam-write`, `stroke-write`, `broken-frame`, `confidence-ink` |
| Mission | 22 | `correction-hinge`, `limit-hairline`, `audio-ruler`, `revision-address` |
| Palette | 17 | `correction-hinge`, `width-escrow`, Delivery receipt, `loading-word` |
| Dotmatrix | 21 | `dot-sort`, `count-cast`, `empty-measure`, `confidence-ink` |
| Swissfi | 12 | Palimpsest correction, Final pass sets the type, `spoken-stagger`, `count-cast` |
| Scope | 18 | `sentence-trace`, `kinetic-lattice`, `onset-joint`, `rewind` |
| Captions | 13 | `sound-sculpture`, `fold-under`, `pitch-baseline`, `unthread` |
| Karaoke | 14 | `bowstring`, `pitch-score`, `spoken-tracking`, `margin-mark` |
| Island | 03 | `word-bulge`, `lean-pace`, Delivery receipt, `exhale` |
| Jarvis | 02 | `ring-reads`, Final pass sets the type, `depth-stack`, `onset-ink` |
| Particles | 09 | `particle-source`, `letter-physics`, `rewind`, `pre-echo` |
| Synthwave | 04 | `onset-grid`, `light-line`, `pitch-baseline`, `particle-source` |
| Mecha | 23 | `hydraulic-chain`, `onset-joint`, `look-choice`, `broken-frame` |
| Cyberpunk | 11 | `look-choice`, Palimpsest correction, `broken-frame`, `rewind` |
| Orb (new, `orb`) | 01 | `pre-echo`, `variable-breath`, `light-letters`, Hand-off |
| Chrome (new, `chrome`) | 05 | `liquid-morph`, `ink-diffusion`, `sound-sculpture`, Hand-off |
| Glass (new, `glass`) | 08 | `depth-stack`, `light-letters`, `magnetic-caret`, Delivery receipt |
| Biolume (new, `biolume`) | 15 | `ink-diffusion`, `breath-line`, `exhale`, `variable-breath` |
| Aurora (new, `aurora`) | 19 | `light-letters`, `variable-breath`, `sound-sculpture`, `magnetic-caret` |
| Hologram (new, `hologram`) | 20 | `depth-stack`, `letter-physics`, `liquid-morph`, `glyph-morph` |
| River (new, `river`) | 24 | `kinetic-lattice`, `sound-sculpture`, `stroke-write`, Hand-off |

## 4. Idle motion ideas

Rules (fixed by RD-02, in full in MOTION-PLAN.md rule 8):

- Idle loops use `Element::ambient` at 30 steps a second or slower (`Loop::fps(15.0)` or lower for slow breaths and drifts), never every frame.
- Only while the popup is open. Drop every idle loop from the view in the update that asks to exit, so it settles well inside vo's 3 s exit cap.
- Never on the newest line's glyphs. A caret blinking or breathing in its own box on that line is allowed (type, palette, phosphor).
- Off under reduced motion. The CPU first frame shows the rest pose.
- Inside the element's own box, and no brightness change over 10 % more than 3 times a second.
- A waveform that redraws on every audio frame in silence counts as idle motion.
- Budget per look at 600x300, checked by `scripts/osd-vo-budget.sh` in the popup repo: idle-open at most 2 % of one core and 30 frames per second; recording at most 10 % of one core; GPU at most 1 ms p95 per frame, taken as the lowest p95 of 3 `--offline --gpu-times` runs (the shared GPU makes single runs jump between 0.3 and 4.2 ms; RD-13); p99 is reported, not gated.

- **Swiss:** the accent hairline shimmers once every few seconds; the recording dot breathes; the timer colon softens on each second.
- **Showcase:** a slow nebula drift behind the text; idle particles orbit the waveform.
- **Type:** the caret breathes; the faint baseline grid pulses with the level; a rare ink sheen crosses the last word.
- **Amber:** a 1 to 2 % CRT flicker; a slow rolling scan band; a few drifting dust motes.
- **Radial:** the spectrum ring turns slowly; tick marks shimmer; the centre glow breathes.
- **Tron:** a light trail runs slowly along the border; the grid pulses faintly.
- **Phosphor:** a scanline roll; phosphor persistence ghosts; the block cursor blinks.
- **Mission:** telemetry digits tick; a tiny radar sweep; status LEDs blink at different rates.
- **Palette:** the caret blinks; the selection highlight drifts as a soft gradient; key hints shimmer.
- **Dotmatrix:** idle dots twinkle at low ink; the meter dots drift by one cell.
- **Swissfi:** the grid lines move in slow parallax; the accent bar shimmers.
- **Scope:** the trace keeps a live noise floor; the graticule glows; the trigger marker blinks.
- **Captions:** soft film grain on the letterbox; a faint projector flicker.
- **Karaoke:** the bouncing ball bobs while idle; a slow stage-light sweep.
- **Island:** a glow orbits the capsule edge; the waveform keeps a micro idle.
- **Jarvis:** ring segments counter-rotate slowly; data ticks crawl; a hologram scan line passes.
- **Particles:** idle particles drift around the words in a slow swirl.
- **Synthwave:** the grid scrolls; the sun's stripes shimmer; neon tubes flicker now and then.
- **Mecha:** gauge needles jitter at idle; warning stripes crawl; the hydraulics breathe.
- **Cyberpunk:** a rare glitch slice every few seconds; neon flicker; a faint scanline.
- **Orb:** a slow aurora swirl inside the orb; the glass catches a moving highlight.
- **Chrome:** the chrome blob idles with a slow wobble and moving reflections.
- **Glass:** the glass sheets drift a few pixels in parallax; a soft caustic moves under them.
- **Biolume:** the jellyfish glow pulses slowly; tendrils sway.
- **Aurora:** the aurora mesh flows slowly behind the text.
- **Hologram:** a scan line passes; letters float a pixel up and down out of phase.
- **River:** light keeps flowing along the river under the text.

## 5. CPU, frames and GPU per look today (RD-02)

Measured 2026-10-28 by `scripts/osd-vo-budget.sh` (popup at 055a7f4 plus RD-02, framework pin ad61428), every look with its default motion set and the default `[osd]` config. Each state ran twice in its own nested KWin; the table keeps the run with less CPU. The popup was a debug build (`opt-level = 1`), so a release build uses less CPU; frame rates and GPU times do not depend on it.

- **Recording:** `--live-moment dictation`, scene time 1.5 to 6.5 s, a replayed dictation with words arriving, one revision and a 1 s pause.
- **Idle-open:** `--live-moment long-silence`, scene time 1.8 to 5.9 s, the popup open on room tone with no new words.
- **CPU** is utime + stime of the popup process from `/proc/<pid>/stat`, in % of one core. **GPU** is p95 / p99 ms per frame from `--offline --gpu-times --moment <moment>` at 10 fps, at each look's own popup size.

| Look | Recording CPU % | Recording frames/s | Recording GPU p95 / p99 ms | Idle-open CPU % | Idle-open frames/s | Idle-open GPU p95 / p99 ms |
|---|---|---|---|---|---|---|
| Swiss | 5.01 | 55.8 | 0.075 / 0.080 | 5.26 | 55.9 | 0.071 / 0.071 |
| Showcase | 8.98 | 55.8 | 1.864 / 3.632 | 7.98 | 56.1 | 2.141 / 3.545 |
| Type | 11.60 | 55.2 | 0.177 / 0.228 | 7.27 | 56.1 | 0.147 / 0.170 |
| Amber | 6.01 | 54.8 | 0.244 / 0.255 | 5.49 | 55.9 | 0.135 / 0.161 |
| Radial | 4.60 | 55.0 | 0.080 / 0.082 | 5.01 | 56.1 | 0.074 / 0.078 |
| Tron | 6.21 | 54.4 | 0.341 / 0.364 | 7.00 | 54.9 | 0.247 / 0.253 |
| Phosphor | 7.20 | 55.2 | 0.211 / 0.233 | 7.01 | 56.6 | 0.128 / 0.130 |
| Mission | 8.62 | 55.8 | 0.322 / 0.343 | 8.98 | 56.6 | 0.106 / 0.111 |
| Palette | 5.59 | 56.4 | 0.216 / 0.240 | 5.74 | 55.9 | 0.178 / 0.184 |
| Dotmatrix | 5.99 | 55.8 | 0.035 / 0.037 | 4.50 | 55.9 | 0.027 / 0.028 |
| Swissfi | 7.40 | 53.4 | 0.134 / 0.136 | 7.48 | 55.4 | 0.114 / 0.116 |
| Scope | 10.38 | 55.8 | 0.644 / 0.952 | 8.25 | 56.1 | 0.935 / 1.028 |
| Captions | 7.78 | 55.0 | 0.563 / 0.584 | 5.75 | 56.1 | 0.477 / 0.501 |
| Karaoke | 7.41 | 55.8 | 0.612 / 0.669 | 5.80 | 56.6 | 0.234 / 0.303 |
| Island | 8.58 | 55.4 | 0.090 / 0.092 | 5.74 | 56.1 | 0.053 / 0.056 |
| Jarvis | 9.78 | 54.6 | 1.594 / 3.043 | 8.50 | 55.6 | 0.666 / 0.683 |
| Particles | 5.61 | 55.8 | 0.083 / 0.088 | 5.01 | 56.1 | 0.068 / 0.081 |
| Synthwave | 7.41 | 55.2 | 0.989 / 3.271 | 7.02 | 55.6 | 2.258 / 2.734 |
| Mecha | 5.01 | 55.8 | 0.281 / 0.301 | 4.74 | 55.4 | 0.206 / 0.210 |
| Cyberpunk | 6.20 | 56.4 | 0.461 / 0.668 | 5.51 | 56.1 | 0.201 / 0.210 |

The seven new looks do not exist yet; their LOOK beads run the script once they do.

Scope with its idle motion (m18: the trace's 1 px live floor, the graticule glow breathing 6 to 9 %, the trigger marker's 1 Hz blink), measure build, 2026-10-29: recording 6.79 % CPU, 52.0 frames/s, GPU 0.692 / 0.705 ms; idle-open 2.00 % CPU, 15.4 frames/s, GPU 0.705 / 0.711 ms. Idle-open was 2.00 % before the idle motion too (RD-14), so it costs no measurable CPU, but scope sits on the 2 % limit.

Swiss with its idle motion (v41: a soft highlight along the top hairline once every 8 s, the recording dot breathing 100 to 70 %, the clock colon softening for 0.25 s each second), measure build, popup 954d005, 2026-10-29: recording 3.41 % CPU, 53.8 frames/s, GPU 0.075 / 0.078 ms; idle-open 1.50 % CPU, 14.9 frames/s, GPU 0.070 / 0.071 ms. All within the budget.

Dotmatrix with its idle motion (feo: about 2 % of the unlit meter dots rise to 12 % ink for 0.4 s, and the quiet baseline steps one cell right every other 2 s and back), measure build, 2026-10-31: recording 5.40 % CPU, 52.8 frames/s, GPU 0.033 / 0.034 ms; idle-open 2.20 % CPU, GPU 0.026 ms p95. Idle-open was 1.46 % at 15.4 frames/s without it, so the motion costs about 0.7 % of one core. That is over the 2 % limit above and under the 3 % limit the idle beads use.

Karaoke with its idle motion (6r7: the mic badge bobs 2 px on a 1.2 s cycle while no word comes, standing in for the bouncing ball; a soft stage light at most 8 % alpha sweeps under the newest line every 10 s), measure build, 2026-10-31: recording 7.40 % CPU, 48.4 frames/s, GPU 0.243 / 0.247 ms; idle-open 1.71 % CPU, 15.4 frames/s, GPU 0.153 / 0.166 ms. All within the budget.

Captions with its idle motion (pcn: the backdrop's flat strips above and below the newest line stand in for letterbox bars; they carry a faint grain and a flicker of at most 2 %, stepped 12 times a second), measure build, 2026-10-31: recording 8.20 % CPU, 52.0 frames/s, GPU 0.154 / 0.155 ms; idle-open 1.95 % CPU, 12.7 frames/s, GPU 0.155 / 0.157 ms. All within the budget. The same script measured 2.20 % idle-open without the motion on the same day, so the spread between runs is larger than its cost. With 48 grain specks per bar instead of 24, idle-open was 2.93 %.

Palette with its idle motion (5wu: the caret's 530 ms blink stepped at 15 fps; the waveform's violet middle drifting 8 px/s out and back over 10 s, standing in for a selection highlight; a 1 s sheen across the key badge every 10 s), measure build, 2026-10-31: recording 5.39 % CPU, 51.4 frames/s, GPU 0.193 / 0.240 ms; idle-open 1.47 % CPU, 15.4 frames/s, GPU 0.153 / 0.180 ms. All within the budget.

Mission with its idle motion (n7b: the latency tile's level meter reading the real level at 4 Hz; a sweep turning inside the status mark's ring once every 4 s, standing in for a radar; the elapsed tile's three dots blinking as LEDs at 0.5, 0.8 and 1.3 Hz), measure build, 2026-10-31: recording 8.40 % CPU, 51.8 frames/s, GPU 0.209 / 0.221 ms; idle-open 1.71 % CPU, 15.4 frames/s, GPU 0.112 / 0.114 ms. All within the budget.

Phosphor with its idle motion (5ti: a 2 px scanline rolling down the screen every 5 s, the block caret blinking 530/530 ms, faint static on the background at 12 frames a second; the scanline and static keep out of the newest line), measure build, 2026-10-31: recording 6.59 % CPU, 52.0 frames/s, GPU 0.193 / 0.197 ms; idle-open 2.20 % CPU. Over the 2 % limit above, under the 3 % the idle beads use.

Jarvis with its idle motion (qwq: ring segments counter-turning at 4, -3 and 2 degrees a second, the outer ticks crawling 10 px a second, a 1 px scan line at 10 % down the text frame every 8 s that skips the newest line's band; the slow loops step 10 times a second), measure build, popup 24a4ab0, 2026-10-31: recording 8.98 % CPU, 54.0 frames/s, GPU median 0.343 ms; idle-open 1.96 % CPU, 11.5 frames/s, GPU median 0.180 ms. All within the budget.

Cyberpunk with its idle motion (rq8: a 6 px slice of a side edge jumps 4 px out for one 67 ms step every 6 s, sides in turn; the hazard slashes dim to 70 % for one step every 9 s; scanlines at 0.3 % alpha, about 3 % as seen, scroll down the waveform window at 4 px/s in 5 steps a second), measure build, popup d8432b3, 2026-10-31, machine under load (load 35 to 100): idle-open 1.71 %, 1.95 % and 2.20 % CPU over three runs at 5.9 to 15.4 frames/s, GPU median 0.16 ms; recording 5.2 to 8.2 % CPU. Without the motion (popup 94e3ae3) idle-open was 1.96 % at 15.4 frames/s. Within the 3 % limit of the idle beads.

Swissfi with its idle motion (di6: the waveform's tick row drifts 1 px every 4 s, out to 4 px and back over 32 s, standing in for a background grid; a 40 px accent highlight runs along the baseline once every 7 s, standing in for an accent bar; both draw in the ticks canvas, which redraws while listening anyway), measure build, popup 3a92791, 2026-10-31: recording 7.20 % CPU, 52.0 frames/s, GPU 0.072 / 0.076 ms; idle-open 1.96 % CPU, 15.4 frames/s, GPU 0.056 / 0.061 ms. Idle-open was 2.68 % in the run before the change, so the difference is noise. All within the budget.

Island with its idle motion (ucu: a 12 px glow at 35 % alpha orbits the capsule's edge once every 6 s, drawn under an opaque underlay so only its outer half shows; bars at their flat height rise by at most 1 px with the room's noise floor), measure build, popup d2b28c5, 2026-10-31: recording 6.39 % CPU, 51.6 frames/s, GPU 0.087 / 0.090 ms; idle-open 2.20 % CPU, 15.4 frames/s, GPU 0.049 / 0.052 ms. The same run without the motion gave idle-open 2.68 %, so the difference is noise. Over the 2 % limit above and under the 3 % limit the idle beads use.

Particles with its idle motion (unz: 30 dots at most 25 % alpha circle the text box at 10 degrees a second, 20 to 60 px out, and fade out 2 px before the newest line's band), measure build, popup 80bf770, 2026-10-31: recording 7.40 % CPU, 49.4 frames/s, GPU 0.078 / 0.083 ms; idle-open 2.19 % CPU, 15.4 frames/s, GPU 0.054 / 0.072 ms. The same run without the dots gave idle-open 2.43 %, so the difference is noise. Over the 2 % limit above and under the 3 % limit the idle beads use. The look's own GPU particles (strands, dust and the newest word's trail) already change pixels in the newest line's box between idle frames, so the bead's "none inside the newest line's box" holds for the new dots only.

River with its effects and idle motion (7kg: kinetic-lattice, sound-sculpture and stroke-write on by default; 14 specks of light at most 40 % alpha drift along the ribbon at 30 px a second), measure build, popup b606fe6, 2026-10-31: recording 7.21 % CPU, 53.2 frames/s, GPU 0.257 / 0.261 ms; idle-open 2.44 % CPU, 15.4 frames/s, GPU 0.258 / 0.267 ms. Over the 2 % limit above and under the 3 % limit the idle beads use; the ribbon itself redraws while listening.

Tron with its idle motion (ie8: the border's light head with a 40 px trail laps the border once every 6 s; the badge's faint outer ring breathes between 50 % and 80 % alpha every 3 s, standing in for a background grid), measure build, popup 29aae95, 2026-10-31, two runs: recording 4.59 and 4.80 % CPU, 52 frames/s, GPU median 0.27 ms; idle-open 1.50 and 1.50 % CPU, 15 frames/s, GPU median 0.18 ms. All within the budget.

Radial with its effects and idle motion (mu2: ring-reads, acoustic-receipts, audio-ruler and onset-joint on by default; zkr: the ring of bars turns 6 degrees a second, one tick brightens 30 % for 300 ms every 0.5 to 1.5 s, the badge glow breathes 85 to 100 % every 4 s), measure build, popup 29aae95, 2026-10-31, two runs: recording 2.99 to 5.51 % CPU, 52 frames/s, GPU median 0.04 ms; idle-open 1.75 and 2.01 % CPU, 15 frames/s. Within the 3 % limit of the idle beads.

Amber with its idle motion (rgl: the glass darkens by at most 2 % in random 80 to 200 ms steps, round the newest line's box, in place of the layer flicker that dimmed the text too; a 24 px band at 0.5 % amber rolls down every 9 s and leaves out the newest line's box; four 1 px motes at 15 % alpha drift at 3 px/s in the side margins), measure build, popup 29aae95, 2026-10-31, two runs: recording 6.20 and 8.60 % CPU, 50 frames/s, GPU median 0.22 ms; idle-open 1.50 and 2.40 % CPU, 15 frames/s. Within the 3 % limit of the idle beads.

Type with its idle motion (09b: the status square breathes 100 to 60 % every 2 s, standing in for a caret; the hairline under the text moves up to 20 % toward the text colour with the smoothed level, standing in for a baseline grid; a 1 s sheen at 5 % white crosses the status word every 12 s), measure build, popup 29aae95, 2026-10-31, two runs: recording 5.59 and 8.98 % CPU, 52 frames/s, GPU median 0.07 ms; idle-open 2.00 and 2.01 % CPU, 15 frames/s. Within the 3 % limit of the idle beads.

Showcase (lq9: no new motion; its orbs already drift like a nebula and its ring's particles orbit above the ribbon; reduced motion now leaves out the ring, the orbs and the rim's light head), measure build, popup e24b0f1, 2026-10-31, four runs: idle-open 2.49, 2.51, 3.49 and 3.99 % CPU at 15 frames/s, recording 8.2 to 12.4 % CPU, GPU median 0.29 to 0.49 ms. Idle-open is over the 3 % limit in two runs of four, without any new idle motion; one run with `no-variable-breath` gave 4.50 %, so the spread is load noise, not one feature.

Showcase at 10 frames a second on room tone (lq9, popup 06666ee: the orbs, the ring and the rim's light head step 10 times a second; variable-breath holds the newest word still on room tone), measure build, 2026-10-31, two runs: idle-open 2.25 and 1.50 % CPU at 10.2 frames/s, recording 8.2 % CPU, GPU median 0.29 to 0.48 ms. Within the 3 % limit of the idle beads.

### Over a limit today

- **Idle-open, every look:** about 56 frames per second and 4.5 to 9 % of one core, against 30 frames per second and 2 %. The popup redraws on every audio frame even on room tone, so this is one popup-wide fix (hold the last frame while the level stays under the noise floor), not 20 look fixes. It must land before the IDLE beads, or no idle motion fits the budget.
- **Recording CPU over 10 %:** Type (11.6 %), Scope (10.4 %). Both are debug-build numbers; check again on a release build before changing either look.
- **GPU over 1 ms p95 or 3 ms p99:** Showcase (recording and idle-open), Jarvis (recording), Synthwave (recording p99, idle-open p95).
