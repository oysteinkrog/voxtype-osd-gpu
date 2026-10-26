# voxtype-osd-gpu: agent rules

- Read `docs/FRAMEWORK-PLAN.md` first: it overrides `docs/ROADMAP.md` where they differ. Then
  `docs/ROADMAP.md` section 8 (decisions) and the bead you are working on.
- Builds and tests go through `heavy-build` (load the `heavy-build` skill). Long builds run detached.
- Never open a window on the real display. Use offscreen rendering or a nested
  `kwin_wayland --virtual` (see the `headless-app-ui` skill).
- One work unit, one commit, `git commit -m ... -- <paths>`. Commit before building.
- Commits are not GPG-signed. This machine has no GPG key, and Oystein accepts unsigned
  commits in his own repos. The bead rule in `docs/ROADMAP.md` 9.1 that asks for signed
  commits does not apply here.
- Several agents share this checkout: never switch branch, reset, rebase or amend.
- No names, hostnames or private words from the dictation word list in this repo.
- Plain language in docs and commit messages, no em dashes.
