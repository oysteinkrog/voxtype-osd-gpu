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
- Budget per look at 600x300, checked by `scripts/osd-vo-budget.sh` in the popup repo: idle-open at most 2 % of one core and 30 frames per second; recording at most 10 % of one core; GPU at most 1 ms p95 and 3 ms p99 per frame.

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

### Over a limit today

- **Idle-open, every look:** about 56 frames per second and 4.5 to 9 % of one core, against 30 frames per second and 2 %. The popup redraws on every audio frame even on room tone, so this is one popup-wide fix (hold the last frame while the level stays under the noise floor), not 20 look fixes. It must land before the IDLE beads, or no idle motion fits the budget.
- **Recording CPU over 10 %:** Type (11.6 %), Scope (10.4 %). Both are debug-build numbers; check again on a release build before changing either look.
- **GPU over 1 ms p95 or 3 ms p99:** Showcase (recording and idle-open), Jarvis (recording), Synthwave (recording p99, idle-open p95).
