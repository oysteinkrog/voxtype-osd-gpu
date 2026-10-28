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

Rules (to be fixed by RD-02): capped rate, inside the per-look budget, never on the reading line, off under reduced motion, stops on exit.

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
