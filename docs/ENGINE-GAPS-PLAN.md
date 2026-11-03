

## Gate results (EG-GATE, 2026-11-03)

Framework c7520dc: `just all` exit 0 (1057 CPU tests, 278 GPU tests, budget ok). Popup 0275331 with vo pin 527844f: both cargo test commands exit 0, `just osd-looks` 189 runs 0 failed, the same with `VO_LIGHT=1`, budget ok after a rerun of six rows that were over by noise (docs/REDESIGN-PLAN.md section 6). effect_cost ran on an RTX 3060 Ti; every case is under the 2 ms limit at 600x300, the slowest smoke at 0.521 ms. Each gap's demo was rendered offline into the gallery folder.

GPU ms per frame at 600x300 from effect_cost, p50 / p95 (the plain card alone is 0.069 / 0.073):

| Gap | Demo in the gallery | effect_cost at 600x300 |
|---|---|---|
| EG-00 | fluid_demo, water_demo | the fluid and water cases below |
| EG-01 | effect_cost sheet | `backdrop` 0.092 / 0.098; `backdrop half` 0.092 / 0.095 |
| EG-02 | effect_cost sheet | `material backdrop` 0.103 / 0.111 |
| EG-03 | displace_demo | `displace ripples` 0.095 / 0.097; `displace noise` 0.098 / 0.102 |
| EG-04 | blur_demo | `layer blur r8` 0.120 / 0.129; `frost r8` 0.125 / 0.130; `frost stack` 0.117 / 0.117 |
| EG-05 | glass_demo --refract | `glass lens` 0.114 / 0.123; `glass prism` 0.111 / 0.124 |
| EG-06 | glass_demo --shapes | `glass octagon` 0.086 / 0.087; `glass wavy` 0.135 / 0.143 |
| EG-07 | blur-outline-check.sh (nested KWin) | compositor work, no GPU pass; octagon and turned panel pass, no-blur fallback prints once |
| EG-08 | fly_out_demo --offline | no GPU pass of its own (margins and a surface) |
| EG-09 | water_demo | `water pool` 0.103 / 0.109 (sim included) |
| EG-10 | water_pool_demo | `water pool` 0.103 / 0.109 |
| EG-11 | fluid_demo | `ink` 0.430 / 0.434; `smoke` 0.521 / 0.534 (sim included) |
| EG-12 | ink_demo | `ink` 0.430 / 0.434; `smoke` 0.521 / 0.534; `watercolour` 0.431 / 0.440 |
| EG-13 | ribbon_demo | `ribbon-3d` 0.084 / 0.085 |
| EG-14 | surface_text_demo | demo bench 0.018 ms p95 (no Element path, so no effect_cost case) |
| EG-15 | type_demo | `glyphs-2000-rotating` 0.196 / 0.203 |
| EG-16 | station EG-16 (4 exhibits) | `glyphs-2000-rotating` 0.196 / 0.203 (same per-glyph path) |
| EG-17 | hero_demo | `hero-env` 0.158 / 0.174 |
| EG-18 | shadow_demo | `shadows-3d` 0.159 / 0.160 |
| EG-19 | hero_demo | `hero-scene` 0.255 / 0.287 |
| EG-20 | orbit_words_demo | `orbit-split` 0.136 / 0.159 |
| EG-21 | hero_demo | `hero-desktop` 0.239 / 0.253 |
| EG-22 | capsule_bulge | `shape-group-8` 0.128 / 0.134 |
| EG-23 | breath_demo | `flex-breathing` 0.068 / 0.074 |
| EG-24 | image_demo | `image-icon` 0.072 / 0.073 |
| EG-25 | material_demo | `paper-panel` 0.072 / 0.073 |
| EG-26 | particles_v2 --concept 26 | `particle-shapes` 0.073 / 0.092 |
| EG-27 | particles_v2 --concept 27 | `glyph-particles-2000` 0.113 / 0.152 |
| EG-28 | fonts-specimen-eg28 golden | fonts only, no GPU pass |
| EG-29 | centreline_demo | `tapered strokes` 0.075 / 0.078 |
| EG-30 | station MO-33 on amber (side captions) | layout only, no GPU pass |
| EG-31 | material_demo --grid-words | `grid-words` 0.081 / 0.090; `grid-words-moving` 0.152 / 0.155 |
| EG-32 | station MO-87 | `liquid-revise` 0.070 / 0.111 |
| EG-33 | stage_demo --panes | `panes-20` 0.038 / 0.044 |
| EG-34 | trail_swirl | `trail-drift` 0.096 / 0.109 |
| EG-35 | popup --offline --replay on karaoke | no GPU pass of its own |
