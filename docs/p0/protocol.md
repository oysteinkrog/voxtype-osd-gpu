# osd-protocol version 1: message design

Phase 0 design for the `osd-protocol` crate (roadmap section 4.10). It covers the
transport, versioning, and the messages for words, confidence, revisions, status and the
target app. Nothing here is built yet. [V] marks a measured or verified fact, [I] an
inference, [G] a guess.

## 1. What the daemon has today

The design follows what the voxtype live worker actually produces, measured with the
revision recorder (P0-08, see `diff-report.md`). [V]

- The Parakeet preview text is **append-only**. `transcribe_chunk` returns only the text
  new in each audio chunk; the worker concatenates it. Earlier words never change during
  a recording. The only live revision of an existing word is the last word growing
  ("confi" then "configuration") when a chunk boundary splits it.
- The **final text** comes from a separate pass: the Cohere engine on the GPU, or the
  preview model's own text (`live_preview_final = "preview"`). An engine final differs in
  casing, punctuation and some words. A preview final equals the last preview revision.
- **Word timing** is available from parakeet-rs (`get_timed_transcript`), per token, in
  80 ms encoder frames. [V]
- **Per-word confidence is not available** from parakeet-rs 0.3.5: the decoder takes the
  arg max of the logits and drops them. Getting it needs a small patch to the crate
  (section 6). [V]
- **Status** after stop is a fraction and a text line, from `S {"text", "fraction"}`
  worker lines. [V, `src/transcribe/worker.rs`]

## 2. Transport

| Item | Design |
|---|---|
| Socket | Unix stream socket `$XDG_RUNTIME_DIR/voxtype/osd.sock`, mode 0600. The daemon listens; the overlay connects. |
| Frame | `u32` little-endian length of the rest, then a 12-byte header, then the payload. Maximum frame 64 KiB; a longer length closes the connection. |
| Header | `magic: [u8; 4] = "VXOS"`, `major: u16`, `minor: u16`, `kind: u16`, `flags: u16` (0 for now). |
| Payload | `postcard` encoding of the kind's struct. |
| Byte order | Little-endian throughout (postcard varints for integers). |

## 3. Versioning

- **Major** changes only when an existing kind changes meaning or layout. A major
  mismatch in `Hello` closes the connection; the launcher falls back to the GTK overlay.
- **Minor** grows when kinds are added. A reader skips any frame whose `kind` it does not
  know, using the length prefix.
- **A kind's payload is frozen within a major version.** postcard is not
  self-describing and cannot read a struct with fields added at the end, so a new field
  means a new kind (for example `TextV2`), which the sender uses only when the peer's
  `Hello` minor says it knows it. [I]
- Both sides send their `major.minor` in `Hello`; each side sends only kinds the other
  knows.

## 4. Messages

### 4.1 Handshake and liveness

| Kind | Direction | Fields |
|---|---|---|
| `Hello` (1) | both, first frame | `major`, `minor`, `role: Daemon or Overlay`, `pid: u32` |
| `Snapshot` (2) | daemon to overlay, after `Hello` | `recording: u32`, `phase: Phase`, `config: Config`, `text: Option<Text>`, `target: Option<Target>` |
| `Heartbeat` (3) | both | `seq: u32`. Every 500 ms while a recording runs or the overlay is visible. 2 s of silence counts as lost (roadmap 4.10). |
| `Resync` (4) | overlay to daemon | `recording: u32`. Ask for a new `Snapshot`, sent when a `Text` delta does not fit (4.3). |
| `Bye` (5) | both | `reason: u8`. Clean close. |

### 4.2 Phase and status

`Phase` (10), daemon to overlay, queued and never dropped:

```rust
enum Phase {
    Hidden,
    LoadingModel,
    Listening,
    Streaming,                     // text is typed into the app live
    Finishing {
        stage: Stage,              // Transcribe or PostProcess
        fraction: Option<f32>,     // 0..1, None while the model loads
        seconds_left: Option<f32>,
    },
    Pasted {
        method: PasteMethod,       // Typed, Pasted, ClipboardOnly, File
        enter_sent: bool,
        elapsed_ms: u32,
    },
    Empty,                         // no speech
    Cancelled { rewound_chars: u32 },
    LimitWarning { seconds_left: u16 },
    Error { kind: ErrorKind, message: String },
}
```

`recording: u32` goes in every `Phase`, `Text` and `Target` message. It starts at 1 for
the daemon's first recording and grows by one per recording, so a late message from the
last recording is dropped. Status text is not sent: the overlay formats "Finishing:
about 0.4 s" from the numbers, in its own style. [I]

### 4.3 Text, words and revisions

`Text` (11), daemon to overlay. Overwrite-latest per recording (roadmap 4.10), sent as a
word-level delta against the last `Text` the daemon sent on that connection:

```rust
struct Text {
    recording: u32,
    rev: u32,           // 1, 2, 3 ... per recording; the final is the last rev
    base_rev: u32,      // the rev this delta applies to; 0 = none (full text)
    source: Source,     // Live or Final { from: FinalFrom }
    keep: u16,          // words 0..keep of base_rev are unchanged
    words: Vec<WordMsg>,// replace words keep.. of base_rev
    breaks: Vec<u16>,   // absolute word indices that start a new paragraph
}

enum FinalFrom { Preview, Engine }

struct WordMsg {
    text: String,       // the word with its punctuation, no spaces
    start_ms: u32,      // audio time of the word, from the recording start
    end_ms: u32,
    conf: Option<u8>,   // 0..=255 for 0..1; None when the model gives none
}
```

Rules:

- **Word ids are not in the protocol.** The overlay assigns them with the `vo-core` word
  model, which diffs every revision and the final against what is on screen. One place
  owns the identity rule, and the daemon stays simple. The delta (`keep` plus `words`)
  is only compression; the overlay rebuilds the full word list before it diffs. [I]
- **Live deltas are small.** With an append-only preview a delta is `keep = n - 1` and
  one to three words: the grown last word plus new ones. [V for the append-only shape]
- **The final is always full:** `base_rev = 0`, `keep = 0`, all words. The final often
  restyles words near the start ("hello" to "Hello,"), so a delta saves little, and a
  full text makes the final swap independent of any lost delta.
- **Coalescing.** If the socket is backed up, the daemon drops unsent `Text` messages
  for the same recording and sends one delta against the last `rev` it actually sent.
  The overlay checks `base_rev` against its last applied `rev`; on a mismatch it sends
  `Resync`. [I]
- **Paragraph breaks** carry the live preview's split at 0.8 s pauses after 6 s
  (commit 6179f5e in voxtype). [V for the split]
- **Size.** A word is about 10 to 14 bytes on the wire (text, two varint times, one
  confidence byte). A full 200-word final is about 3 KB; a live delta under 60 bytes. [I]

### 4.4 Confidence

- `conf` is the word's confidence, quantised to `u8`. Defined as the **lowest** token
  probability in the word (the softmax maximum at each emitted token), so one doubtful
  subword marks the whole word. [I]
- `None` means the engine gives none. The overlay then uses word age, per decision 9.
- The final from Cohere has no per-word scores in our code today, so final words carry
  `None`. A word that the final kept from the live text keeps its live confidence on
  screen only if the overlay chooses so; the protocol does not merge them. [I]

### 4.5 Target app and output

`Target` (12), daemon to overlay:

```rust
struct Target {
    recording: u32,
    at: TargetAt,                // RecordingStart or Paste
    app_name: Option<String>,    // display name from the .desktop file, else the class
    app_id: Option<String>,      // desktop file id or resource class, for an icon lookup
    output: Option<String>,      // connector name, e.g. "DP-2"
}
```

- Never the window caption: it holds document names, chats and URLs. [I]
- `None` fields when not on KWin, or when the focused window is the lock screen or
  KWin itself. How the daemon gets these: `output-detection.md`.
- The overlay does not need `output` for placement; KWin puts a layer surface without an
  output on the active one. It is for logs and per-output config. [I]

### 4.6 Telemetry and config

Unchanged from roadmap 4.10, listed for completeness:

- `Telemetry` (13), 1 to 2 Hz: `elapsed_ms`, `words`, `wpm`, `level_dbfs`, `model`,
  `engine_latency_ms`, `mic`.
- `Config` (14), on connect and on change: `design`, `variant`, `theme`, `reduced_motion`,
  `quality_tier`, `hide_transcript`.

### 4.7 Overlay to daemon diagnostics

`Presented` (20), optional, overlay to daemon: `recording`, `rev`, `frame_ms` (time from
receiving the `Text` to the frame that shows it). It lets the daemon log the latency
target in decision 8 (a new word visible at 50 % within 100 ms) on real sessions, with
no screen capture. Sent at most 10 times a second. [I]

## 5. Example

A synthetic recording, words written as `text@start-end`:

| rev | base | keep | words | screen after |
|---|---|---|---|---|
| 1 | 0 | 0 | `open@0-240` `the@240-400` `confi@400-640` | open the confi |
| 2 | 1 | 2 | `configuration@400-880` `file@880-1120` | open the configuration file |
| 3 (final, engine) | 0 | 0 | `Open@0-240` `the@240-400` `configuration@400-880` `file.@880-1120` | Open the configuration file. |

The word model gives: rev 2 keeps `open` and `the`, extends `confi` (same id) and inserts
`file`; the final restyles `open` and `file`, keeps the other two, and nothing gets a new
id.

## 6. Open items

1. **Confidence from Parakeet.** parakeet-rs 0.3.5 `ParakeetUnified::decode_encoder_frames`
   takes the arg max of the joint logits and drops them. A patch that also keeps
   `softmax(logits)[token]` per emitted token, stored next to `accumulated_timed_tokens`,
   is about 20 lines. It needs a fork branch of parakeet-rs and a `[patch.crates-io]`
   entry in voxtype. Cost per token: one softmax over about 1024 entries (1023 vocabulary lines plus blank), small
   next to the decoder run. [V for the code path; G for the size]
2. **Timing resolution.** Token times are 80 ms encoder frames, and a token's start is the
   frame where it was emitted, which can trail the sound. Good enough for word age; not
   for karaoke-style highlight. [V for the frame size; I for the lag]
3. **Live typing.** With `live_typing = true` the words are already in the app; the final
   then rewinds and retypes. The `Streaming` phase and `Cancelled { rewound_chars }`
   cover the states, but the overlay may want to know how many characters a final
   rewrites. Not designed yet.
