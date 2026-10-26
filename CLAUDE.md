# voxtype-osd-gpu: agent rules

- Read `docs/ROADMAP.md` section 8 (decisions) and the bead you are working on first.
- Builds and tests go through `heavy-build` (load the `heavy-build` skill). Long builds run detached.
- Never open a window on the real display. Use offscreen rendering or a nested
  `kwin_wayland --virtual` (see the `headless-app-ui` skill).
- One work unit, one commit, `git commit -m ... -- <paths>`. Commit before building.
- Several agents share this checkout: never switch branch, reset, rebase or amend.
- No names, hostnames or private words from the dictation word list in this repo.
- Plain language in docs and commit messages, no em dashes.
