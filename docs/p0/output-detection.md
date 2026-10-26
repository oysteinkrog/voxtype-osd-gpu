# Output and target app detection on KWin (P0-13)

Bead P0-13, question Q11 in the roadmap. Measured on 2026-10-26 on KWin 6.8.1 (Plasma
6.8.1), three outputs, Wayland session. [V] marks a measured or tested result, [I] an
inference, [G] a guess.

## Answer

1. **Output: let KWin place the overlay.** A layer-shell surface created without a
   `wl_output` goes on KWin's active output. The overlay learns which output that is from
   the `wl_surface.enter` event, with its scale and refresh rate, at no extra cost. The
   daemon does not need to send an output name for placement. [I: KWin's layer-shell code
   picks `workspace()->activeOutput()` for a surface with no output; not checked against
   the 6.8.1 source.]
2. **When the daemon needs the output name** (logs, a per-output config, the `Config`
   message), it calls `org.kde.KWin /KWin org.kde.KWin.activeOutputName` over D-Bus. It
   returns the connector name, for example `DP-2`. [V]
3. **Target app: a persistent KWin script.** The daemon loads one small KWin script at
   start. The script connects to `workspace.windowActivated` and calls the daemon back
   over D-Bus with the window's `resourceClass`, `desktopFileName` and output name. The
   daemon keeps the latest value, so reading it at recording start or at paste time costs
   nothing. [I for the callback design; V for the fields, see below]
   A persistent script also avoids a failure seen with per-call scripts on 2026-10-26 on
   another Plasma machine: kdotool timed out on every call. KWin gave a newly loaded
   script an id that another loaded script already had (load A, B and C, unload B, and the
   next load gets C's id), so kdotool ran the wrong script and never got its answer. A
   script loaded once and kept loaded by name is not exposed to that id reuse. [V for the
   failure, reported by the lead; I for the cause being avoided]
4. **Fallback** when the script cannot load (not KWin, or scripting disabled): the same
   `kdotool getactivewindow getwindowclassname` call that `~/bin/voxtype-auto-enter`
   uses, run once at paste time. [V]

## Measurements

20 calls each, from a shell, wall time per call. Most of each number is process start,
so an in-process call from the daemon with `zbus` (already a voxtype dependency) is
cheaper. [V for the numbers; G for "cheaper", not measured in-process]

| Method | Per call | Returns |
|---|---|---|
| `busctl --user call ... activeOutputName` | 2.0 ms | `DP-2` |
| `qdbus6 ... activeOutputName` | 4.6 ms | same |
| `kdotool getactivewindow` | 2.9 ms | window UUID |
| `kdotool getactivewindow getwindowclassname` | 3.0 ms | resource class |
| `busctl ... getWindowInfo <uuid>` | 2.0 ms | map of window fields |
| One-shot script: `loadScript` + `run` + `unloadScript` | 6.5 ms | anything the script prints |

`getWindowInfo` returns, among others: `resourceClass`, `resourceName`, `desktopFile`,
`caption`, `pid`, `x`, `y`, `width`, `height`, `layer`, `type`. It has no output name, so
mapping a window to an output from it means comparing its geometry to the output
geometries. KWin's D-Bus API in 6.8 has no "active window" method; the window UUID has to
come from a script (kdotool loads one per call). [V]

The one-shot script probe printed the active screen name, the active window's
`resourceClass`, its `desktopFileName` and its `output.name` to the journal. While the
screen was locked the active window was the lock screen itself (`kwin_wayland`, empty
desktop file name). [V]

## Notes for phase 1

- **Which moment counts.** The `pasted { target_app }` phase needs the window that has
  focus at paste time, not at recording start. Focus can change while the user speaks.
  The persistent script gives both for free. [I]
- **Privacy.** Send the app's display name only: `desktopFileName` mapped to the `Name=`
  of its `.desktop` file, else `resourceClass`. Never send the window caption; titles hold
  document names, chat names and URLs. [I]
- **Lock screen and no focus.** When the active window is KWin's own (`kwin_wayland`) or
  there is none, send no target app. [V for the lock screen value]
- **Active output rule.** KWin's active output follows focus or the mouse, depending on
  the "active screen follows mouse" setting. The overlay should not second-guess it. [I]
- **Auto-Enter can share the script.** `voxtype-auto-enter` spawns kdotool after every
  paste. With the persistent script it could read the class the daemon already has. [I]
- **Not on KWin.** On Sway or Hyprland the compositor IPC differs (`swaymsg -t
  get_tree`, `hyprctl activewindow`). Out of scope for phase 0; the protocol carries the
  target app as optional for that reason.

## Script sketch

Not loaded or tested in this form; the one-shot probe above used the same fields. [I]

```js
// voxtype-focus.js, loaded once with org.kde.kwin.Scripting.loadScript
function report(w) {
    if (!w) return;
    callDBus("org.voxtype.Daemon", "/org/voxtype/Focus", "org.voxtype.Focus", "changed",
             w.resourceClass || "", w.desktopFileName || "",
             w.output ? w.output.name : "");
}
workspace.windowActivated.connect(report);
report(workspace.activeWindow);
```

The daemon owns `org.voxtype.Daemon` on the session bus and unloads the script by name at
exit, so a daemon restart does not leave two copies running.
