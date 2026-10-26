# voxtype-osd-gpu

GPU overlay for voxtype dictation: wgpu + smithay-client-toolkit on Wayland layer shell,
Slug glyph rendering (from HarfBuzz hb-gpu), animated word display.

The framework plan is `docs/FRAMEWORK-PLAN.md`. The renderer plan is `docs/ROADMAP.md` (targets, phases, beads) and `docs/CAPABILITIES.md`.
Only the `osd-protocol` crate is meant to go into voxtype itself.

Builds and tests on the development desktop go through `heavy-build`.
