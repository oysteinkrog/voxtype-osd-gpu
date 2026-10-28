# Engine Gaps Plan: What the Designs Need That vo Cannot Draw Yet

Date: 2026-10-28. Status: plan; beads with labels `engine-gap` (EG-nn) and `round2` (R2-nn).

Source: a review of all 24 concepts, their round 1 and round 2 redesigns, against the engine's code. The gap cards with plain explanations are in the gallery (https://claude.ai/artifact/E3J8iqQE9aAn7XdXQYjgeB, section "What the engine cannot draw yet").

## Decisions

- **No screen capture.** The popup never reads the desktop. Frosted glass uses KDE blur-behind (EG-07); reflections of "the desktop" are procedural (EG-21) or of the popup itself (EG-17).
- **No future words live.** The recogniser knows a word only after it is spoken. Looks that need the whole sentence work in a replay mode (EG-35).
- **The reading-line envelope stays.** Blur, rotation and big moves happen on older lines and around the text.
- **One foundation first.** EG-01 (render what is under an element into a texture) unlocks displacement, refraction, layer blur, water reflections and text on surfaces.

## Engine gap beads

| Bead | What | Size | Needs | Gap cards |
|---|---|---|---|---|
| EG-01 | Backdrop capture: render the layers under an element into a texture | L | - | 3, 4, 5, 11 |
| EG-02 | Materials can sample textures (backdrop and images) | M | EG-01 | 4, 14 |
| EG-03 | Displacement pass: ripples, noise and curls bend a layer | L | EG-01 | 3 |
| EG-04 | Layer blur and frosted shapes | M | EG-01 | 11 |
| EG-05 | Refracting glass: lenses, prisms and slabs bend what is under them | L | EG-01, EG-02, EG-06 | 4 |
| EG-06 | Glass on any outline and on tilted layers | M | - | 4, 12 |
| EG-07 | Frosted glass looks use KDE blur-behind, shaped to the outline | M | - | 1, 10 |
| EG-08 | Effects can leave the popup: automatic margins and a fly-out layer | M | - | 2 |
| EG-09 | Water surface simulation | L | - | 5 |
| EG-10 | Water shading and wobbling reflections | M | EG-03, EG-09 | 5 |
| EG-11 | Fluid simulation core: ink and smoke with memory | L | - | 6 |
| EG-12 | Ink and smoke looks from the fluid | M | EG-11, EG-03 | 6 |
| EG-13 | Ribbons and cloth that twist and fold | L | - | 7 |
| EG-14 | Text on curved surfaces | L | EG-01, EG-13 | 7 |
| EG-15 | Per-glyph transforms in the text renderer | M | - | 8 |
| EG-16 | Per-letter motions: tumbling arrivals, leaning digits, italic arrival, text on a wave | M | EG-15 | 8 |
| EG-17 | 3D reflections from a scene, not a fixed studio | M | EG-01 | 9, 10 |
| EG-18 | Soft shadows in 3D | M | - | 9 |
| EG-19 | 3D scenes with several shapes and see-through insides | L | - | 9 |
| EG-20 | 3D can pass in front of text (except the reading line) | M | - | 9 |
| EG-21 | Plausible desktop reflections without screen capture | S | EG-17, EG-07 | 10 |
| EG-22 | Shapes that bulge and melt together | M | - | 12 |
| EG-23 | Smooth variable-font axes | M | - | 13 |
| EG-24 | Images and app icons | M | EG-02 | 14 |
| EG-25 | Paper, fabric and grain textures | S | EG-24 | 14 |
| EG-26 | Particles form any shape: wire up points and mask forces | S | - | 15 |
| EG-27 | Particles drawn as tiny letters | M | EG-15 | 15 |
| EG-28 | Add the missing fonts | S | - | 16 |
| EG-29 | Pen-stroke order for the real UI font | M | - | 16 |
| EG-30 | Vertical text | S | - | 16 |
| EG-31 | Backgrounds that know where the words are | M | EG-02 | 17 |
| EG-32 | Liquid morph in the live text | M | - | 18 |
| EG-33 | Many tilted panes in one draw | M | - | 19 |
| EG-34 | Trails that drift and turn | S | - | 20 |
| EG-35 | Replay mode for looks that need the whole sentence | M | - | - |

## Round 2 beads, one per concept

Each builds on its look's redesign bead and the engine gaps it needs.

| Bead | Concept | Ideas | Needs |
|---|---|---|---|
| R2-01 | Glass aurora orb | Sonic Ripple, Magnetic Type, Breathing Light | RD-ORB-FX, EG-01, EG-03, EG-07, EG-08, EG-15, EG-16, EG-21, EG-23, EG-27 |
| R2-02 | JARVIS hologram | Living Glass, Particle Orbit, Typewriter Approach | RD-JARVIS-IDLE, EG-01, EG-02, EG-04, EG-05, EG-06, EG-07, EG-15, EG-16, EG-21 |
| R2-03 | Dynamic island | Breathing Lens, Ink Flow, Tape Reveal | RD-ISLAND-IDLE, EG-01, EG-02, EG-05, EG-06, EG-08, EG-11, EG-12, EG-15, EG-16, EG-22, EG-23, EG-24 |
| R2-04 | Synthwave neon | Liquid Echo, Prism Focus, Typewriter Spotlight | RD-SYNTHWAVE-IDLE, EG-01, EG-02, EG-03, EG-05, EG-06, EG-08, EG-09, EG-10, EG-15, EG-16, EG-18, EG-28 |
| R2-05 | Liquid chrome | Breathing Aurora, Typewriter Whisper, Ripple Lens | RD-CHROME-FX, EG-01, EG-07, EG-09, EG-10, EG-15, EG-16, EG-17, EG-19, EG-21, EG-26, EG-27, EG-32 |
| R2-06 | Phosphor terminal | Liquid Surface, Falling Letters, Breathing Halo | RD-PHOSPHOR-IDLE |
| R2-07 | Blade Runner amber | Magnetic Ink Flow, Typewriter Ribbon, Glass Lens Ripple | RD-AMBER-IDLE, EG-01, EG-03, EG-07, EG-30 |
| R2-08 | Spatial glass | Breathing Aurora, Page Reveal, Resonant Droplets | RD-GLASS-FX, EG-01, EG-02, EG-04, EG-05, EG-06, EG-07, EG-08, EG-09, EG-10, EG-14, EG-21, EG-22 |
| R2-09 | Particle words | Ink Flow, Prism Reveal, Letterpress Build | RD-PARTICLES-IDLE, EG-01, EG-02, EG-05, EG-06, EG-07, EG-08, EG-11, EG-12, EG-15, EG-16 |
| R2-10 | Radial spectrum | Liquid Ripple, Typewriter Ribbon, Breathing Halo | RD-RADIAL-IDLE, EG-01, EG-02, EG-06, EG-07, EG-09, EG-10, EG-13, EG-18, EG-20, EG-22, EG-25, EG-28 |
| R2-11 | Cyberpunk glitch | Luminous Ink Flow, Breathing Focus Field, Typewriter Horizon | RD-CYBERPUNK-IDLE, EG-11, EG-12, EG-15, EG-16, EG-28 |
| R2-12 | Swiss sci-fi | Breathing Typescape, Word Particles, Echo Focus | RD-SWISSFI-IDLE, EG-01, EG-03, EG-07, EG-08, EG-13 |
| R2-13 | Cinema captions | Liquid Echo, Prism Capture, Typewriter Set | RD-CAPTIONS-IDLE, EG-01, EG-02, EG-03, EG-05, EG-06, EG-07, EG-09, EG-10, EG-22, EG-28, EG-35 |
| R2-14 | Karaoke prompter | Living Typewriter Ribbon, Breathing Glass Ripple, Connecting Thoughts | RD-KARAOKE-IDLE, EG-01, EG-02, EG-03, EG-07, EG-25, EG-28, EG-35 |
| R2-15 | Bioluminescent | Breathing Membrane, Prism Light, Ink Reveal | RD-BIOLUME-FX, EG-07, EG-08, EG-11, EG-12, EG-13, EG-19, EG-21, EG-23 |
| R2-16 | Tron light lines | Ink Ripple, Breathing Ribbon, Luminous Typewriter | RD-TRON-IDLE, EG-01, EG-08, EG-09, EG-10, EG-13, EG-29, EG-35 |
| R2-17 | Command palette | Breathing Ribbon, Typewriter Reveal, Lens Echo | RD-PALETTE-IDLE, EG-01, EG-02, EG-03, EG-07, EG-13, EG-21, EG-24 |
| R2-18 | Oscilloscope | Typewriter Echo, Breathing Text, Cinematic Rewind | RD-SCOPE-IDLE, EG-01, EG-03, EG-08, EG-09, EG-10, EG-33 |
| R2-19 | Aurora mesh | Prismatic Lens, Luminous Ink, Magnetic Type | RD-AURORA-FX, EG-07, EG-11, EG-12, EG-15, EG-16, EG-21, EG-23 |
| R2-20 | Floating hologram text | Typewriter Waves, Orbiting Words, Breathing Scroll | RD-HOLOGRAM-FX, EG-01, EG-03, EG-04, EG-08, EG-09, EG-10, EG-14, EG-20, EG-32 |
| R2-21 | Dot matrix | Typewriter Ink, Liquid Glass Flow, Orbital Word Rings | RD-DOTMATRIX-IDLE, EG-01, EG-02, EG-05, EG-06 |
| R2-22 | Mission control | Breath Ripple, Typewriter Reveal, Spectral Focus | RD-MISSION-IDLE, EG-01, EG-02, EG-04, EG-25, EG-28, EG-34, EG-35 |
| R2-23 | Mecha cockpit | Magnetic Ink, Glass Prism, Breathing Grid | RD-MECHA-IDLE, EG-01, EG-02, EG-05, EG-06, EG-24, EG-28, EG-31 |
| R2-24 | River of light | Typewriter Ink Reveal, Breathing Glass Panel, Audio Ribbon | RD-RIVER-FX, EG-01, EG-02, EG-03, EG-06, EG-07, EG-11, EG-12, EG-15, EG-16, EG-21, EG-22, EG-25, EG-28, EG-29 |
