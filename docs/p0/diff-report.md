# Word diff on real revision streams (P0-08, P0-09)

Phase 0 question Q7: how do words change between live preview revisions and at the final
swap, and does the word model keep their identity? Run on 2026-10-26. This report holds
numbers and synthetic examples only; the corpus is private speech and stays outside this
repo. [V] marks a measured result, [I] an inference, [G] a guess.

## Answer

1. **The live preview only appends.** In 2112 revisions, no word before the last one ever
   changed. Each revision adds 1.6 words on average (0 to 6), and the last word may grow
   or gain punctuation. [V]
2. **The final swap from the Cohere engine is the only real revision.** 97.6 % of final
   words keep a live word id, but 42 % of kept words shift sideways and 12 % change line,
   because earlier words change length. `settle-to-final` must move words, not only
   crossfade the changed ones. [V]
3. **With today's config the final swap is empty.** `live_preview_final = "preview"`
   pastes the last preview text, so the final equals what is on screen in every
   recording where the replay matched the live run (63 of 63). [V]
4. **The diff is cheap.** 6 µs median, 74 µs worst, for up to 299 words, in a release
   build. The 1 ms budget in the roadmap is not a concern; no 200-word window is needed
   for speed. [V]
5. **No per-word confidence yet.** parakeet-rs 0.3.5 drops the decoder scores, so all
   words carry `conf: null`. The overlay must use word age until the crate is patched
   (`protocol.md` section 6). [V]

## Corpus

| Item | Value |
|---|---|
| Recordings | 94 kept recordings from 2026-10-25 and 26 (target was 20) [V] |
| Audio | 1687 s in total; per recording p50 9.0 s, p90 36.9 s, max 161 s [V] |
| Preview model | Parakeet unified 0.6b, int8, CPU, parakeet-rs default streaming context [V] |
| Final text | The text that was pasted at the time: Cohere engine in 14 recordings, the preview's own text in 80 [V] |
| Revisions | 2112 preview revisions [V] |
| Replay check | The replayed preview equals the preview recorded live in 77 of 94. The other 17 differ by 1 to 5 words, all from the same evening. [V] Probably because the kept WAV is 16-bit and the live model saw 32-bit float samples. [G] |

How it was built: voxtype's `replay_revisions` example fed each kept WAV to the preview
model in 100 ms blocks, as the live worker does, and wrote one revision log per
recording in the format of the new `live_preview_revisions_dir` recorder. Wall time is
simulated: a block arrives when its audio would have been spoken, and its revision is out
when the model is done with it, measured on this machine under a 4-CPU cap. [V]

## Live revisions

Numbers summed over all 2112 revisions. "Kept" counts every word that stayed the same in
every revision, so it is large. [V]

| Change | Count |
|---|---|
| kept | 126 089 |
| inserted (new word at the end) | 3 507 |
| extended (last word grew, "confi" to "configuration") | 232 |
| restyled (last word gained punctuation, "file" to "file.") | 155 |
| substituted | 0 |
| removed | 0 |
| revisions that touch any word before the last | 0 |

| Measure | p50 | p90 | max |
|---|---|---|---|
| Time between revisions | 600 ms | 1197 ms | 6699 ms |
| Word first shown, after the word starts in the audio | 1037 ms | 1261 ms | 1385 ms |
| Word final form shown, after the word ends | 951 ms | 1180 ms | 1327 ms |
| Words on screen | 32 | 177 | 299 |
| Diff time per revision (release build) | 6 µs | 35 µs | 74 µs |

Kept words that move: 22 in all 2112 revisions, each one a last word that grew past the
right edge and wrapped to the next line (48-column greedy wrap). At 80 columns: 12. [V]

The recogniser, not the overlay, sets the speed: words appear about one second after they
are spoken, in steps of 0.6 s, matching the streaming chunk plus right context. The
decision 8 budget (solid glyph at 50 % within 100 ms of the update) is small next to
that. [V for the numbers; I for the cause]

## Final swap

The final text was diffed against the whole live paragraph with the same word model. [V]

| Measure | Engine final (Cohere) | Preview final |
|---|---|---|
| Recordings | 14 | 80 |
| Final identical to the last preview | 1 | 63 (the other 17 are replay mismatches, see Corpus) |
| Live words, final words | 344, 330 | 3163, 3159 |
| Final words that keep a live id | 97.6 % | 99.8 % |
| kept | 283 | 3127 |
| restyled (case or punctuation) | 30 | 18 |
| extended (prefix grew, also merges) | 4 | 2 |
| substituted (similar word) | 5 | 7 |
| inserted | 8 | 5 |
| removed | 22 | 9 |
| Kept-id words that move, 48 columns | 138 of 330 (42 %) | 126 |
| Kept-id words that change line, 48 columns | 39 (12 %) | 12 |
| Kept-id words that change line, 80 columns | 27 | 11 |
| Share of words that move, per changed recording (p50, p90) | 41 %, 77 % | 5 %, 17 % |
| Unchanged words before the first change, per changed recording (p50) | 0 % | 52 % |

The preview column is mostly replay noise: in the 63 recordings where the replay matches
the live run, the final changes nothing. [V]

For the engine finals, the first change is usually the first word (capital letter), so
nothing on screen can stay still by default. Removals outnumber insertions about 3 to 1.
Grouped into runs of changed words: 8 runs delete words outright (7 of them one word), 3
runs merge 2 or 3 live words into one final word, 6 runs swap one word for one or two, 1 run rewrites 7 words as 5,
and 2 runs insert a word. [V]

### Synthetic examples

Written for this report, not taken from the corpus.

| Live | Final | Word model result |
|---|---|---|
| `hello world how are you` | `Hello, world. How are you?` | 4 restyled, 1 kept, no new ids; every word after "Hello," shifts right |
| `a well dash known name` | `a well-known name` | "well" extended to "well-known" (same id), "dash" and "known" removed |
| `put it over there now` | `put it over their now` | "there" substituted by "their" (same id) |
| `send it today` | `send it tomorrow` | "today" removed, "tomorrow" inserted (similarity 0.25, under 0.5) |
| `open the confi` then `open the configuration file` | (live) | "confi" extended, "file" inserted |

## Design rules for TY06 and settle-to-final

- **Live path: append and grow.** The overlay needs three live motions: a new word
  enters at the end, the last word grows in place, the last word gains punctuation. Mid
  paragraph substitution never happens with this recogniser, so it can use a simple
  fallback (crossfade) without tuning. [V for the data; I for the design]
- **Final path: FLIP for everything.** With an engine final, plan for 40 % or more of
  words to move sideways and about 1 in 8 to change line. `settle-to-final` should
  animate positions of all words by id, crossfade restyled words in place, and fade out
  removed ones. [V for the data; I for the design]
- **Removed words need an exit motion.** 22 removals against 8 insertions in the engine
  finals. [V]
- **The stability rule in TY06** (committed after about 600 ms or two unchanged
  hypotheses) can be simpler: with an append-only stream, every word but the last is
  committed as soon as the next word appears. [I]
- **Window of 200 words.** Not needed for speed. It may still be wanted for layout. [V
  for speed]

## How to reproduce

```sh
# Turn on the recorder in the daemon (off by default), or replay kept recordings:
#   live_preview_revisions_dir = "<private dir>"          in config.toml
cargo run --example replay_revisions --features parakeet -- <corpus dir> <private dir>

# Then, in this repo:
cargo run --release -p vo-core --example revdiff -- <private dir> 48
```

`revdiff` prints JSON with the numbers above and no corpus text.

## Open problems

1. **Confidence.** Needs a parakeet-rs fork patch (about 20 lines, [G]) to keep the
   softmax score of each emitted token. Until then the overlay's confidence styling has
   only word age to go on.
2. **Few engine finals.** Only 14 recordings used Cohere, because the config switched to
   preview finals on 2026-10-25. More engine data would firm up the move and removal
   numbers. A second replay with Cohere on the same WAVs would give 94, at about 2.4 GiB
   VRAM per run. [G for the VRAM]
3. **Simulated wall time.** The replay ran the preview model in a debug build (opt-level
   1) on a 4-CPU cap. Live latency can differ; the live recorder gives real times once it
   is turned on.
4. **Live vs replay mismatch** in 17 of 94 recordings, probably 16-bit WAV rounding. It
   does not change the live churn numbers, but it adds noise to the preview final
   column.
