# Motion Plan: Animation Beyond the Mock-ups

Date: 2026-10-27. Status: plan, nothing built. Each item has a bead in `.beads/` (label `motion`).

Inputs:
- the inventory of today's motion (`/var/tmp/claude/anim-plan/inventory.md`);
- two oracle reviews: Fable (`fable.md`) and GPT-6 Astra through Codex (`astra.md`);
- the 12-look grid render at framework 4770fac.

Item ids `MO-nn` are used in the beads. A tag after each concept says where it came from: **[F]** Fable, **[A]** Astra, **[F+A]** both.

## 1. What is wrong today

Both oracles found the same five problems.

1. **The chrome moves; the words do not.** Frames trace on, rings spin and bars grow. The transcript, the one thing the reader looks at, gets the same small rise and fade in about 14 of the 18 looks inventoried. The looks differ in costume, not in choreography.
2. **No signal reaches a glyph.** The popup has pitch, onsets, level, word timing and revisions. None of them touch the letters. A word said slowly, loudly, as a question or after a correction looks the same.
3. **Corrections hide the thing that changed.** Every revision dips the whole word to 20 to 35 % opacity, which is exactly when the reader wants to see it.
4. **Time is flat.** Fixed 12 ms staggers, one spring for everything. Pauses, hurrying and trailing off leave no trace.
5. **Endings are stickers.** Finishing shows a bar; Done draws a check. The real event, text going into the app, has no motion and about 150 ms of screen time, because the popup exits 0.15 s after the daemon goes idle.

## 2. Motion language

These eight rules apply to every look. Section 6 turns the first two into automated checks.

1. **The reading-line envelope.** On the newest line: baseline shift at most 3 px, tracking change at most 5 %, ink at least 70 %, no rotation, blur or scale. Above it, older lines may do more. Text appears readable in the frame it arrives.
2. **The transcript is the instrument.** Every signal (level, onset, pitch, timing, confidence, revision) reaches a glyph first. Chrome may echo it, never replace it.
3. **React before you know.** Audio leads the recogniser by 100 to 400 ms. Show "heard, not yet read", so the popup never feels behind the speaker's mouth.
4. **Replay spoken time.** Groups of words move in spoken order at a scaled copy of the spoken rhythm, not a fixed stagger.
5. **Show the consequence, not a ceremony.** Finishing sets the type, done sends the text, cancel takes it back. Labels and checks are secondary.
6. **One material per look.** Each look has one physical idea for how words are born, corrected and leave: ink, dots, beam, light line, trace, hydraulics, particles. Same engine, different choreography.
7. **Truthful outcomes.** Tell "recognised", "delivery attempted" and "delivery reported" apart. Never fake progress. A new dictation cuts any old ending short.
8. **Settle to still, within budget.** Within 600 ms of the last event a look is near still. The whole popup stays near 1 ms of GPU per frame at 600x300, and reduced motion keeps every state cue as a static mark.

## 3. Foundations (needed by most concepts)

| Id | What | Size | Where |
|---|---|---|---|
| MO-01 | **Presentation lifecycle.** The host lingers on Done (about 400 ms, cut short by a new dictation) and lets exit presences finish before the process exits. Today only cancel, empty and failed linger. | M | voxtype host |
| MO-02 | **Delivery events in the popup.** Read `result.json` (method, Enter sent, latency, target app) at the moment of delivery, and expose an `Ending::Delivered { .. }` step. | S | voxtype host |
| MO-03 | **Per-word typographic channels.** Springs for tracking, ink (weight by SDF dilation or stroke), baseline offset and per-glyph opacity on live text, all clamped by the reading-line envelope. | M | vo-scene, glyph shader |
| MO-04 | **Signal-to-glyph binding.** Sample onset, pitch and level at a word's spoken time and feed MO-03 channels. Needs word times on the wire, or the onset that arrived nearest before the word. | M | vo-scene, voxtype host |
| MO-05 | **Spoken-time staggers.** `Timeline::stagger` with per-item times from word timing, scaled and clamped (default 0.25x, 200 to 600 ms). | S | vo-core |
| MO-06 | **Glyph outlines as paths.** Morph from a segment, frame or blob into a glyph's outline and back. | M | vo-scene, vector path |
| MO-07 | **Moment scenarios.** Scripted scenarios for the wall and offline renders: first word, pause, long silence, revision burst, line wrap, overflow, final pass with corrections, typed delivery, pasted delivery with Enter, cancel, no speech, failure, loading, time limit. Each gets a contact sheet. | M | voxtype wall, vo-preview |
| MO-08 | **Envelope and budget checks.** A test helper that runs a scenario and asserts the reading-line envelope frame by frame (baseline, tracking, ink, no glyph off its row), plus the GPU budget per look from the offline timing hook. | M | vo-render tests, voxtype |

## 4. Concepts

### 4.1 Hearing: before and as words arrive

| Id | Concept | Moment and what you see | Size |
|---|---|---|---|
| MO-10 | **Breath line, first word finds its seat** [F+A] | Before speech, the empty reading line is a 1 px hairline breathing with level. At the first word it kinks under where the word sits, and the glyph outlines rise out of the line (60 ms) and fill (160 ms). | M (MO-06) |
| MO-11 | **Pre-echo caret** [F] | While you speak and no word has arrived, faint blobs grow along the baseline, one per onset, width by duration, 15 % ink. When words arrive each blob turns into its word box (120 ms) and the letters resolve in it. | M |
| MO-12 | **Exhale and snap back** [F+A] | After 3 s of silence, everything but the text exhales over 1.2 s (wave flat, frame thinner, clock grey, a margin bookmark where speech stopped). On the next onset, before the word exists, the look snaps back in one fast spring. | S |
| MO-13 | **Onset ink** [F] | A word born on a hard onset lands up to 0.6 px heavier and thins over 180 ms; a soft start arrives light and fills in. | S (MO-03, 04) |
| MO-14 | **Pitch baseline** [F] | The newest word's baseline follows pitch by up to 3 px while it is said, so a question tilts up at its tail. It relaxes flat over 600 ms after the word commits. | S (MO-03, 04) |
| MO-15 | **Spoken-time tracking** [F] | A word arrives with letter spacing set by how long it took to say (clamped 0.8 to 1.3 of normal, within the 5 % envelope on the reading line), then settles. A drawn-out word arrives stretched. | S (MO-03, 04) |

### 4.2 Editing: corrections you can read

| Id | Concept | Moment and what you see | Size |
|---|---|---|---|
| MO-20 | **Palimpsest correction** [F+A] | The default revision in every look. Letters that survive do not move. Removed letters sink 4 px into a 20 % ghost that stays readable for 500 ms; new letters arrive in place. Ligatures and joined scripts fall back to a local crossfade. | M |
| MO-21 | **Correction hinge** [A] | When one word becomes several or several become one, a thin bracket marks the span, its joints move from the old word breaks to the new ones (90 ms), and it retracts. | M |
| MO-22 | **Width escrow** [A] | During a burst of revisions, a changed word borrows up to half an em of space, so neighbours do not shake. After 100 ms without a change, neighbours make one move. | M |
| MO-23 | **Revision address** [A] | When a word on an older line changes, a small mark lights beside that line and beside the reading line, then the two contract together. The eye knows where to look without a travelling highlight. | S |
| MO-24 | **Confidence as ink, stability seam** [F+A] | Tentative words are 75 % ink with a little extra tracking. After two unchanged hypotheses they set to full ink with a 1 px baseline settle. A short open seam under an unstable word closes when it stabilises. | S (MO-03) |

### 4.3 Time: pauses, wraps and overflow

| Id | Concept | Moment and what you see | Size |
|---|---|---|---|
| MO-30 | **Silence becomes punctuation** [F+A] | After 600 ms of quiet the caret lays one 1 px tick per 400 ms along the baseline. At 1.5 s the ticks gather into a hollow comma or full stop. On speech they squeeze into a space in 90 ms. The final pass fills the hollow mark or removes it. | S |
| MO-31 | **Margin bell** [F] | When the reading line is nearly full, its tracking tightens slightly (up to 5 %), foreshadowing the wrap. On wrap it relaxes as the new line starts. Type and phosphor add a 3 px carriage-return shift. | S (MO-03) |
| MO-32 | **The line changes its address** [A] | On a wrap the new word appears on the reading line at once; the previous line moves up while its margin mark splits, leaving one half beside the new line. | M |
| MO-33 | **Fold-under overflow** [F] | The top line turns away on its x-axis around its baseline (0 to 80 degrees, 220 ms) while the rest move up; its shadow brushes the next line for 100 ms. Swiss gets a flat version: the line thins to nothing from its baseline. | S (Stage exists) |
| MO-34 | **Overflow spine** [A] | A line that leaves leaves behind a strip of tiny word-width blocks in the top margin, which compacts into one history tick: the reader sees how much they have said. | M |
| MO-35 | **Time-limit hairline** [F] | In the last 10 s before the recording limit, a hairline under the reading line shortens from the right at the real rate; colour changes only at 3 s. | S |

### 4.4 Finishing and delivery

| Id | Concept | Moment and what you see | Size |
|---|---|---|---|
| MO-40 | **The final pass sets the type** [F+A] | The finishing fraction moves a soft 40 px boundary through the text. Ahead of it words are "spoken ink" (lighter, slightly loose); behind it they are set type. A word the final text changes changes exactly when the boundary reaches it (palimpsest), with a 2 px nod; punctuation pops in at the boundary; a capital grows. Only changed words get a bracket. | M (pieces exist, unwired) |
| MO-41 | **Remaining audio as a ruler** [A] | While finishing, a footer ruler shows seconds of audio left as units; finished units close into hairlines; the end joins the final mark. It never runs backwards and never fakes progress. | S |
| MO-42 | **Hand-off: the text leaves** [F] | On a typed delivery, letters leave in reading order at the real typing rate, each shrinking toward its baseline, so the popup empties as the app fills. On a paste, lines collapse into the reading line (160 ms), the line compresses to a bright 2 px hairline (120 ms) and stamps down (80 ms). If Enter was sent, a last return-shaped tick leaves after the text. | M (MO-01, 02) |
| MO-43 | **Delivery receipt** [A] | A miniature silhouette of the line lengths folds into a receipt notch beside the target app's name. The label states the actual result (typed, pasted, Enter), not that the app rendered it. | S (MO-02) |

### 4.5 Endings and other states

| Id | Concept | Moment and what you see | Size |
|---|---|---|---|
| MO-50 | **Rewind cancel** [F] | The text rewinds in spoken order at 4x the spoken rate (clamped to 500 ms), each word shrinking into the caret, the waveform scrubbing backwards in sync. For timeline looks (scope, amber, karaoke, phosphor, mecha, tron). | S (MO-05) |
| MO-51 | **Unthread cancel** [A] | A baseline thread appears under the text and pulls back from the newest word to the first, releasing small word-boundary stitches; the text stays readable until the panel fades. For the type, swiss and swissfi family. | M |
| MO-52 | **No speech: the empty measure** [F+A] | The breath line flatlines for 300 ms; two measuring marks close on nothing; the caret types "No speech" at 25 ms a letter. Clearly different from cancel. | S |
| MO-53 | **Broken frame** [F+A] | On failure the frame line breaks at one point and the two ends curl 4 px apart (150 ms); the stage that failed is named. The words stay still and readable; nothing shakes. | S (MO-06) |
| MO-54 | **The status word is the loading bar** [F+A] | While a model loads, the reading line says "Loading" and each letter fills from outline to solid as progress passes it (only with real progress; otherwise baseline segments trade places). At ready, the word swaps to "Listening", shared letters staying. | S |

### 4.6 One material per look

| Id | Look | Concept | Size |
|---|---|---|---|
| MO-60 | dotmatrix | **Dot sortation** [F]: dots from the meter fall into the new word's glyph cells; the meter is visibly short and regrows them. | S |
| MO-61 | phosphor | **Beam-write** [F]: a bright 3 px beam head writes each word in 80 ms; a revision rewrites only the changed letters. | S |
| MO-62 | jarvis | **The ring reads with you** [F]: the ring's light head points at the current word like an eye and springs word to word; it sweeps once on the final pass. | S |
| MO-63 | scope | **The trace is the sentence** [F]: each word keeps its audio segment as a trace that freezes and compresses into the history with a word tick. | M |
| MO-64 | karaoke | **Bowstring underline** [F]: the underline is a string from the previous word to the current one; it tightens as the word is said and pitch bends it. | S |
| MO-65 | type, swissfi, mission, dotmatrix | **The count takes a cast** [A]: a block the width of the new word contracts into a notch by the word counter as its digit rolls. | S |
| MO-66 | type, swissfi, mission, island | **Leaning digits** [F]: the words-per-minute digits lean up to 8 degrees with speech rate and stand up when you pause. | S |
| MO-67 | synthwave | **Grid of onsets** [F]: each onset sends a grid line from the horizon, so the road's spacing is your speech rhythm and silence is empty road. | S |
| MO-68 | tron | **Light line under words** [F]: at each onset the border's light head detaches, draws a line under the new word and rejoins the border. | M |
| MO-69 | showcase, particle words | **Particles obey timing, negative impression** [F+A]: particles for a word come from the ring segment lit when it was said; after a word lands, a few dots trace its negative under it and fold into a tick. | M |
| MO-70 | mecha | **Hydraulic chain** [F]: a new word pushes the caret and status plate with a 15 ms lag per part; a revision slides the word's plate out and back. | S |
| MO-71 | scope, amber, karaoke, mission | **Acoustic receipts** [A]: under the newest three timed words, the sound that made each word folds into a short underline, then a timestamp tick. | M |
| MO-72 | karaoke, scope, radial, amber | **Pitch margin score** [A]: beside the line, up to six marks place the phrase's pitch and join into a contour. | M |
| MO-73 | mecha, tron, scope, cyberpunk | **Onset joint** [A]: an onset opens a 2 px joint in the caret's base; when the word arrives that joint becomes its baseline tick for 120 ms. | S |
| MO-74 | island | **Word bulge** [F]: each new word bulges the pill's right edge 2 px and relaxes. | S |
| MO-75 | cyberpunk, synthwave, mecha, tron, jarvis, palette, mission, amber, radial, captions, karaoke, scope, island, swissfi | **Per-look choice**: each look picks its revision (MO-20 variant), cancel (MO-50 or MO-51), delivery (MO-42 variant) and overflow (MO-33 or MO-32) in its own material. One bead per look family. | S each |

## 5. Build order

Phase A, foundations and the moments every look shares (in this order):
1. MO-07 moment scenarios and MO-08 envelope checks, so every later step can be judged.
2. MO-01 presentation lifecycle and MO-02 delivery events, then MO-42 hand-off and MO-43 receipt.
3. MO-40 the final pass sets the type (its pieces exist, unwired).
4. MO-20 palimpsest correction as the new default.
5. MO-03 and MO-04 per-word channels, then MO-24 confidence ink, MO-13 onset ink, MO-14 pitch baseline, MO-15 spoken-time tracking.
6. MO-11 pre-echo caret and MO-12 exhale and snap back.
7. MO-05 spoken-time staggers, then MO-50 and MO-51 cancels.

Phase B, time and states: MO-30, MO-31, MO-33, MO-32, MO-34, MO-35, MO-41, MO-52, MO-53, MO-54, MO-21, MO-22, MO-23, MO-06 with MO-10.

Phase C, one material per look: MO-60 to MO-75, one lane per look family.

Both oracles put the hand-off, the final pass and readable corrections first. They split on the rest: Fable leads with hearing (pre-echo, onset and pitch ink), Astra with the counter and the line address. This plan takes Fable's order for phase A because it changes how every look feels, and Astra's items in phase B.

## 6. How each step is judged

- One scenario render (MO-07) per changed moment, before and after, on a contact sheet.
- The envelope check (MO-08) passes on every look that uses the change.
- GPU per frame from the offline timing hook stays within budget (whole popup near 1 ms at 600x300; no effect over 2 ms).
- Reduced motion shows the same state as a static mark.
- The framework gate passes (fmt, clippy, tests, budget) before a pin; the maintainer sees each phase on his own screen before the next starts.

## 7. Risks

- **Readability.** Per-glyph motion can make text harder to read. The envelope (rule 1) and MO-08 are the guard, and every concept has a reduced-motion form.
- **Word timing.** Only the parakeet live preview sends word times today. MO-04 needs a fallback that uses onsets and arrival order, and concepts that need real timing (MO-15, MO-50, MO-63, MO-71) degrade to arrival order without it.
- **Truthful delivery.** The popup cannot see the target app's screen. MO-42 and MO-43 show what the daemon reports, nothing more.
- **Novelty.** Neither oracle can prove no shipping software has done any one idea. The combination, and the rule that signals reach the glyphs, is what is new.
