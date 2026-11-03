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
or a modifier change stops the repeat. Key events go into the event log's `input` records (format
below).

### Event log format, version 2

`vo_core::event_log` writes JSON. Version 2 adds keyboard input as first-class records:

- The file has `version`, `seed`, `events` (the app's messages with their `t`) and, in version 2, an
  `input` list. Each input entry is `{ "t": ..., "event": { "kind": ..., ... } }`.
- Input kinds: `key` (state `Down` or `Up`, the logical key or keysym, the raw code, the text it
  types and the modifiers), `keyboard_focus` (gained, requested), `modifiers`, `keymap` (the layout
  name) and `repeat_info` (rate and delay). Repeats are not logged: the `Driver` makes them again on
  the host clock, so a replay at 60 Hz and at 144 Hz gives the same keys. Repeat info must come
  before the first key; the writer and the reader both refuse a key without it.
- `t` is seconds from the log's start on the host clock, the clock that stamps keys.
- A log with no input writes version 1, byte for byte as before. A log with input writes version 2.
- The reader reads version 1 and 2. It skips input records of a kind it does not know, so a later
  kind does not break it, and refuses versions above 2 with "update the tool".
- The `input` list is additive. A reader that knows only `version`, `seed` and `events` reads a
  version 2 file and ignores the keys. A build with the version 1 check refuses a version 2 file,
  which is right: it could not replay the keys.
- `vo::offline::Scenario::to_log` and `Scenario::from_log` turn a scripted scenario (for example
  `type_text` and `chord`) into a log and back. Pointer inputs are not logged yet.

**Routing.** A key goes to the focused node first (`Element::on_key`) and reaches `App::key` only
when no node has focus.

## Focus order

Added 2026-11-03 (F4c). A scene can hold several focusable nodes (`Element::on_key`).

- **Tab order.** Nodes with a positive `Element::tab_index` come first, lowest first, then nodes
  with index 0 (the default) in document order: depth first, in layout order. A negative index
  leaves the node out of the tab order. It can still hold focus.
- **Tab and Shift+Tab** (without Ctrl, Alt or Super) move focus one step forward or back and
  wrap around. They do this only when there is another node in the tab order to move to, so with
  one focusable node Tab reaches that node like any other key. The release of a Tab that moved
  focus goes nowhere, so the new node never sees a stray key up. A held Tab repeats like any key.
- **First focus.** On each view the scene keeps focus on its node while that node is still
  focusable, which holds for as long as the view keeps the node's key path. Otherwise focus goes
  to the first node in the tab order, or to the first focusable node when all opted out.
- **Messages.** `Element::on_focus(enter, leave)` gives the app a message when focus comes to the
  node and when it goes: the old node's leave first, then the new node's enter. A removed node
  gets no leave. The driver sends them after the view that caused them, or after the Tab.
- **Focus ring.** `Element::focus_ring(FocusRing { color, width, gap })` draws a stroke `gap`
  outside the node's box while it has focus. The look picks colour and width. No node has a ring
  by default, so existing looks draw nothing new.
- `Keyboard::None` stays the default for every surface. Scene focus says which node gets keys;
  the surface still needs keyboard focus (the focus proxy) for keys to arrive.

**Shortcuts.** v1 does not use `zwp_keyboard_shortcuts_inhibit_manager_v1`. KWin global shortcuts keep
working while the popup has focus, as they do in any text box. The edit keys the popup uses (arrows,
Home, End, Backspace, Delete, Ctrl+Z and the like) are not global shortcuts, so nothing needs to be
inhibited.

## Pointer in text boxes

Added 2026-11-03 (F6). `Element::on_pointer(fn(PointerInput) -> M)` gives a node the primary
button's press, drag and release at node-local positions (its box from the top left, after its
transform), so a text box can place the caret and select.

- **Press** carries a click count: 2 and 3 for a press on the same node within
  `MULTI_CLICK_SECS` (0.4 s) and `MULTI_CLICK_PX` (4 px) of the press before. A fourth press
  starts at 1 again. The count comes from event times, so a replay counts the same.
- **Drag** comes for every motion while the button is held, also outside the box: the pressed
  node keeps the pointer, as Wayland's implicit grab does. **Release** ends it.
- `Element::pointer_tag(u32)` is passed back in `PointerInput::tag`, so one handler serves
  several nodes, such as the line nodes of a look that draws one live text per line.
- `Element::cursor(Cursor::Text)` sets the cursor over a node, or over the pressed node while
  the button is held. The live driver sends `SurfaceChange::Cursor` when it changes. The
  Wayland host shows it through `wp_cursor_shape_v1`, so the compositor draws its own theme's
  I-beam. Without the protocol, the host draws the shape from the XCursor theme
  (`XCURSOR_THEME`, `XCURSOR_SIZE`) on a cursor surface of its own. `scripts/cursor-fallback.sh`
  checks both in a nested KWin, hiding the global with `VO_NO_CURSOR_SHAPE=1` for the fallback.
- `vo_core::edit::word_at` and `line_at` give what a double and a triple click select: the
  run of word characters, separators or spaces under the click (Qt's rule), and the line
  between hard line breaks. `EditAction::Select` selects a range.
- A surface gets pointer events only where its input region says. A popup that should stay
  click-through outside editing opens with `InputMode::Regions` and an empty region: only
  nodes with handlers make the region, so it is empty until a text box takes the pointer.

## Clipboard

Added 2026-11-03 (F5). Copy and paste of plain text through `wl_data_device`, on the seat the
keyboard comes from.

- `Cx::copy(text)` puts text on the clipboard; `Cx::paste()` asks for it and `App::clipboard`
  answers with `Some(text)`, or `None` when the clipboard is empty or holds no
  `text/plain;charset=utf-8`. Live the answer comes on a later event; offline the driver keeps the
  last copy and answers at once.
- Only an explicit copy sets the clipboard. The host never sets the primary selection, so
  selecting text leaves what a middle click or Shift+Insert in another app pastes alone (plan
  R18).
- The compositor checks the serial of a copy against the client's input, and sends the clipboard
  only to the client with keyboard focus. So copy needs a key press since focus came (a copy key
  is one), and both work only while the focus proxy has focus.
- Pipes are read and written on short-lived threads, so a slow peer never blocks the event loop.
  A paste is cut at 4 MiB.
- The text box keymap maps Ctrl+C and Ctrl+Insert to copy, Ctrl+X and Shift+Delete to cut, and
  Ctrl+V and Shift+Insert to paste (`vo_core::edit_keymap`).
- `scripts/clipboard-check.sh` checks it in a nested KWin with `wl-copy` and `wl-paste`.

## Input methods

Input methods (IME, plan stage F7) go through `zwp_text_input_v3` on the seat the keyboard
comes from. The compositor routes keys to the input method while it is on, and the input method
sends back text in batches.

- The app turns it on with `Cx::text_input(Some(state))`, where `state` is the text around the
  caret and the selection in it (`TextInputState::around` keeps it under the protocol's 4000
  bytes). The app calls it again after every change, and with `None` when editing ends. The
  live driver sends it to the focus proxy, so it is only on while the proxy has keyboard focus.
- The caret box for the candidate list comes from the drawn frame
  (`DisplayList::edit_caret_box`, the caret bar of an `Element::edit` live text in surface
  space). The proxy sits at the output's top left, so the host adds the surface's place on the
  output.
- Each `done` arrives as one `App::text_input(TextInputEvent)`: bytes to delete before and
  after the selection, text to commit, and a preedit. `TextInputEvent::actions` turns the
  deletes and the commit into edits; `Composing` keeps the preedit. The preedit is drawn at the
  caret but never enters the text (R55). A focus loss or the end of editing drops it.
- The event log records batches as `text_input` input records; readers from before skip them.
- `examples/fake_ime.rs` is a test input method that KWin starts itself
  (`kwin_wayland --inputmethod`), for nested tests.

## Screen readers

Screen readers (plan stage F8) see an app through AccessKit on AT-SPI, with `vo`'s `a11y`
feature.

- The app describes itself with `App::accessible`: a name, the text it shows, whether that
  text is final, and the field being edited with its selection (`vo_a11y::Accessible`).
- `vo_a11y::tree` makes the full tree: a `Window` named after the app, a `Label` with the
  shown text as a polite live region, and while editing a focused `MultilineTextInput` with one
  `TextRun` per hard line, character lengths by grapheme cluster, and the caret and selection.
- `vo_a11y::Announcer` keeps a screen reader from hearing every revision: live text at most
  every 2 s, a final text at once and once.
- The live overlay asks the app once at the start. When it describes itself, an
  accesskit_unix adapter starts and watches the session bus. While no assistive technology runs
  the adapter stays inactive and the app is not asked again, so it costs nothing. An activation
  wakes the loop and the next change sends the tree. `VO_A11Y=0` turns it off.
- Actions from the screen reader (focus, set selection) are not handled yet.

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

Touch and gestures, scroll, and drag and drop between apps.

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
