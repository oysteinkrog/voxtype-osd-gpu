# Input model

Written 2026-10-26 for M1. It fills the "Input model" gap in `FRAMEWORK-PLAN.md`. Linux (Wayland)
is the only host built in M1. The host types below are shaped so the macOS and Windows hosts in M3
and M5 can implement them without changes to the core.

## Summary

- Nodes opt in to input with `.interactive()` or a handler such as `.on_click(msg)`. Every other node
  is invisible to the pointer.
- The host reports raw pointer events with a timestamp. They go into the event log like every other
  input. The scene turns them into app `Msg` values, so a replay produces the same messages.
- Hit testing uses node bounds sampled at the event's timestamp, from the same pure sampling that
  draws the frame.
- The input region handed to the host is the union of the interactive nodes' shapes. A scene with no
  interactive nodes is fully click-through, which is what the dictation popup is today.
- Keyboard interactivity defaults to none. A popup that edits text takes focus through a separate
  focus proxy surface and keeps its own surface at none (see "Keyboard and focus").

## Hit testing

A node is a hit target when its element sets `.interactive()` or any pointer handler. A hit target
blocks click-through over its shape, even with no handler, so a toast body can swallow clicks without
acting on them.

The hit shape is the node's layout box with its corner radius, after the node's sampled transform
(translate, scale, rotate). The scene tests a point by mapping it through the inverse transform into
the node's local space. Hit targets are tested in reverse paint order, and the first one hit wins.
There is no event bubbling: a click goes to that one node.

These nodes are skipped:

- nodes whose presence is `leaving`, so a toast that is animating out does not catch a second click;
- nodes whose sampled opacity is 0;
- every node on a surface opened with `InputMode::None`.

**Animated bounds.** `scene.hit_test(point, t)` samples the bounds and transforms of the hit targets
and their ancestors at the event time `t`. It does not reuse the last presented frame. Both come from
the same `sample(t)` channels, so they differ only by the time between the frame and the event, at
most one frame. The reason for sampling at `t`: the last presented frame depends on the refresh rate,
so a 144 Hz session and a 60 fps replay would hit different nodes during a spring. Sampling at the
event time keeps the result the same at any frame rate. Sampling only hit targets keeps it cheap.

**Hover with a still pointer.** Hover is re-evaluated on every pointer event and on every app event
that changes the view. It is not re-evaluated per frame, again because that would depend on the frame
rate. A node that slides under a still pointer gets its enter on the next pointer move. For overlays
this is acceptable.

## Events

The host emits raw events in surface-local logical pixels:

```rust
pub struct PointerEvent { pub surface: SurfaceId, pub t: Time, pub pos: Point, pub kind: PointerKind }
pub enum PointerKind { Enter, Leave, Motion, Down(Button), Up(Button), Cancel }
pub enum Button { Primary, Secondary, Middle }
```

`t` is the host's clock at receipt, the same clock that stamps every other event (`FRAMEWORK-PLAN.md`
decision 3). `Time` is the M1 time type in `vo-core`. Scroll is left out until a design needs it.

The scene keeps a small pointer state per surface: the hovered node id and the pressed node id. From
the raw events it derives:

| Derived | Rule |
|---|---|
| enter, leave | the hovered node id changed between two evaluations |
| click | `Up(Primary)` lands on the same node as the `Down(Primary)` before it |
| drag | `Motion` while a node with `.on_drag` is pressed; delivered with the position local to that node |

While a button is down, the pressed node keeps the pointer even when it leaves the shape. Wayland gives
this for free with its implicit grab on button press. If the pressed node is removed, or the host sends
`Cancel`, the press ends with no click.

Each derived event becomes one app `Msg` with the raw event's timestamp, and goes through the same
`update(state, msg, t)` path as IPC and timer messages. The view then runs at `t` as usual.

**Event log.** The log records the raw `PointerEvent`, not the derived message. Replay feeds the raw
events back through the scene, so it tests the current hit testing and handlers rather than repeating
old answers. The log also records each derived `Msg` as a check: on replay, a different message at
the same `t` fails the determinism test. That catches key identity bugs (risk 6 in the plan) early.
Pointer events are a new variant in the versioned log format, and authored scenarios can use them to
script a click.

## Click-through region

After each `sample(t)` the scene builds the input region from the display list's hit shapes:

1. Take every hit target that is not skipped (the rules above).
2. Turn each rounded shape into integer rectangles, rounded outward, in surface logical pixels. The
   spike's `rounded_rects` in `popup.rs` already does this for the blur region.
3. Send the union to the host with `SurfaceChange::Input` only when it changed since the last send.

While a spring moves a hit target, the region follows it every frame. On Wayland the input region is
double-buffered surface state, so it rides on the frame's own commit and costs no extra commit. When
the scene settles the region stops changing, and no frames are drawn (decision 11).

Hosts:

- **Wayland:** `wl_surface.set_input_region` with a `wl_region` built from the rectangles. An empty
  region means fully click-through, as in the spike (`popup.rs:194-195`). `InputMode::Whole` passes
  no region, so the whole surface takes input.
- **macOS (M3):** the content view overrides `hitTest:` and returns nil outside the region. Fully
  click-through panels set `ignoresMouseEvents`.
- **Windows (M5):** `WS_EX_TRANSPARENT` makes the whole window click-through. Per-region input
  probably needs the host to toggle that style as the cursor crosses the region, which is the open
  question in plan risk 3. A host that cannot do regions reports `ClickThrough::Whole` in its caps,
  and the app decides what to give up.

## Keyboard interactivity

`OverlaySpec` gets a `keyboard` field with three modes, matching layer-shell:

| Mode | Layer-shell | Meaning |
|---|---|---|
| `None` (default) | `keyboard_interactivity: none` | never takes focus; typing goes to the app below |
| `OnDemand` | `on_demand` | takes focus only when clicked |
| `Exclusive` | `exclusive` | grabs the keyboard while mapped |

M1 implemented `None` only. The Wayland host now maps all three, but every visible overlay still uses
`None`: overlays must never steal focus from the window the user is dictating into. Keyboard focus for
editing comes from the focus proxy below. On macOS `None` maps to a non-activating `NSPanel` that
refuses key status. On Windows it maps to `WS_EX_NOACTIVATE`.

## Keyboard and focus

Added 2026-10-30 for keyboard edits in the dictation popup: one editable text inside an overlay.

**Focus proxy.** `HostCtl::open_focus_proxy(FocusProxySpec)` maps a second layer surface: 1x1,
transparent, on the Overlay layer, `keyboard_interactivity: on_demand`, an empty input region and a
buffer attached at its first configure. KWin activates an `on_demand` layer surface when it is mapped
with a buffer, so the proxy gets the keyboard. `close_focus_proxy` sets a "requested" flag and destroys
the surface, and KWin's focus chain gives focus back to the window that had it. A focus loss the app
did not request (click-away, Alt+Tab, lock screen, output removal) arrives as
`HostEvent::KeyboardFocus { gained: false, requested: false }`, and focus is never taken back by
itself. `FocusProxySpec::output` puts the proxy on the output the target window is on.

The visible popup stays at `Keyboard::None`, for this reason: KWin's `handleAcceptsFocusChanged` has
no branch for a surface that switches back from `on_demand` to `none`, so a popup that turned
`on_demand` for editing would keep it, and an `on_demand` popup takes focus every time it is mapped.
That would steal focus from the dictation target on every recording. A separate surface that exists
only while editing avoids both problems. Layer surfaces stay out of Alt+Tab, the pager and the
taskbar, so the proxy is never seen.

**Keys.** The Wayland host keeps its own xkbcommon state and compose table (locale from `LC_ALL`,
`LC_CTYPE` or `LANG`) and sends `HostEvent::Key`, `Modifiers` and `KeyRepeatInfo`. It binds `wl_seat`
at version 9 or lower for the keyboard: with `wl_keyboard` version 10 KWin repeats keys itself and
reports a repeat rate of 0. Key repeat runs in the `Driver`, on the host clock, so offline runs and
replays repeat the same way. Keys held when focus arrives are not replayed, and focus loss, a key up
or a modifier change stops the repeat. Key events go into the event log's `input` records, which
version 1 readers skip.

**Routing.** A key goes to the focused node first (`Element::on_key`; the scene focuses the first
such node, one per scene for now) and reaches `App::key` only when no node has focus.

**Shortcuts.** v1 does not use `zwp_keyboard_shortcuts_inhibit_manager_v1`. KWin global shortcuts keep
working while the popup has focus, as they do in any text box. The edit keys the popup uses (arrows,
Home, End, Backspace, Delete, Ctrl+Z and the like) are not global shortcuts, so nothing needs to be
inhibited.

## The three cases the plan needs

- **Dictation popup:** `InputMode::None`, `Keyboard::None`, no interactive nodes. The region is empty
  and never changes, the same as the spike. The surface never receives a pointer event.
- **Notification toast:** `InputMode::Regions`, `Keyboard::None`. The toast body is `.interactive()`
  with `.on_hover(Msg::Hold, Msg::Release)`, so hovering pauses the auto-dismiss timer. The close
  button has `.on_click(Msg::Dismiss)`. The region is the toast capsule and follows it while it slides
  in. Once the toast is leaving it is skipped, and the region shrinks to empty.
- **Preview and scrub tool:** A dev tool, so it may open a normal `xdg_toplevel` window with
  `InputMode::Whole`. The scrub bar uses `.on_drag(|d| Msg::Seek(d.local.x / d.size.w))` and the
  play button uses `.on_click`. The tool is itself a framework app with its own event log. The scene
  it previews sees only the seek time, not the tool's pointer events.

## Out of scope

Text input and IME, focus traversal and more than one focusable node, clipboard, screen readers,
touch and gestures, scroll, cursor shape changes, and drag and drop between apps. Node ids stay stable by key
path, so accesskit can be added later as the plan says.

## Exact additions

**Host** (to the sketch in the hosts lane report, section 7):

```rust
pub struct OverlaySpec {
    // existing fields ...
    pub input: InputMode,    // was None | Regions(Vec<Rect>)
    pub keyboard: Keyboard,  // new, default Keyboard::None
}
pub enum InputMode { None, Regions(InputRegion), Whole }
pub struct InputRegion { pub rects: Vec<IRect> } // integer logical px, may be empty
pub enum Keyboard { None, OnDemand, Exclusive }

pub enum SurfaceChange { /* existing ..., */ Input(InputMode) }
pub enum HostEvent { /* existing ..., */ Pointer(PointerEvent) } // types as in "Events"
// HostCaps.click_through: ClickThrough::{None, Whole, Regions} is unchanged.
```

**Elements** (`vo-scene`), all on the element builder:

```rust
fn interactive(self) -> Self;                       // hit target, no handler
fn on_click(self, msg: Msg) -> Self;                // implies interactive
fn on_hover(self, enter: Msg, leave: Msg) -> Self;  // implies interactive
fn on_drag(self, f: fn(DragInput) -> Msg) -> Self;  // implies interactive
pub struct DragInput { pub local: Point, pub size: Size, pub phase: DragPhase }
pub enum DragPhase { Start, Move, End }
```

Handlers are message values or plain `fn` pointers, not closures. That keeps elements cheap to diff
and the public API free of extra generics.

**Nodes and scene** (`vo-scene`):

```rust
// on the retained node: a hit flag and an index into the scene's handler table.
pub hit: Option<HandlerId>,

impl Scene {
    pub fn hit_test(&self, pos: Point, t: Time) -> Option<NodeId>;
    pub fn pointer(&mut self, ev: &PointerEvent) -> SmallVec<[Msg; 2]>; // updates hover and press state
}
impl DisplayList {
    pub fn input_region(&self) -> InputRegion; // union of the sampled hit shapes
}
```

**Event log** (`vo-core`): one new entry kind for `PointerEvent`, and one for the derived `Msg`
check.
