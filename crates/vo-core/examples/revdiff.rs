//! Run the word model over recorded revision logs (voxtype
//! `live_preview_revisions_dir` or its `replay_revisions` example) and print
//! churn and final-swap numbers as JSON. Prints no corpus text.
//!
//! ```sh
//! cargo run -p vo-core --example revdiff -- DIR [WRAP_COLUMNS]
//! ```

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use vo_core::words::{Change, ChangeKind, WordId, WordModel};

const KINDS: [ChangeKind; 6] = [
    ChangeKind::Kept,
    ChangeKind::Restyled,
    ChangeKind::Extended,
    ChangeKind::Substituted,
    ChangeKind::Inserted,
    ChangeKind::Removed,
];

fn kind_name(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Kept => "kept",
        ChangeKind::Restyled => "restyled",
        ChangeKind::Extended => "extended",
        ChangeKind::Substituted => "substituted",
        ChangeKind::Inserted => "inserted",
        ChangeKind::Removed => "removed",
    }
}

fn logs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            logs(&path, out);
        } else if path.to_string_lossy().ends_with(".revisions.jsonl") {
            out.push(path);
        }
    }
}

/// Greedy wrap at `width` characters: (line, column) of each word, keyed
/// by id. A proxy for where a word sits in the popup.
fn positions(model: &WordModel, width: usize) -> HashMap<WordId, (usize, usize)> {
    let (mut line, mut col, mut out) = (0, 0, HashMap::new());
    for w in model.words() {
        let len = w.text.chars().count();
        if col > 0 && col + 1 + len > width {
            line += 1;
            col = 0;
        }
        if col > 0 {
            col += 1;
        }
        out.insert(w.id, (line, col));
        col += len;
    }
    out
}

/// Kept-id words whose position changed: (moved sideways or across lines,
/// changed line).
fn moved(
    before: &HashMap<WordId, (usize, usize)>,
    after: &HashMap<WordId, (usize, usize)>,
    changes: &[Change],
) -> (usize, usize) {
    let (mut moved, mut lines) = (0, 0);
    for c in changes.iter().filter(|c| c.old_index.is_some() && c.new_index.is_some()) {
        if let (Some(a), Some(b)) = (before.get(&c.id), after.get(&c.id)) {
            if a != b {
                moved += 1;
            }
            if a.0 != b.0 {
                lines += 1;
            }
        }
    }
    (moved, lines)
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn dist(mut v: Vec<f64>) -> Value {
    let n = v.len();
    let mean = if n > 0 { v.iter().sum::<f64>() / n as f64 } else { f64::NAN };
    json!({
        "n": n,
        "mean": (mean * 10.0).round() / 10.0,
        "p50": pct(&mut v, 0.5),
        "p90": pct(&mut v, 0.9),
        "max": pct(&mut v, 1.0),
    })
}

#[derive(Default)]
struct Counts(HashMap<&'static str, usize>);

impl Counts {
    fn add(&mut self, changes: &[Change]) {
        for c in changes {
            *self.0.entry(kind_name(c.kind)).or_default() += 1;
        }
    }
    fn json(&self) -> Value {
        let mut m = serde_json::Map::new();
        for k in KINDS {
            m.insert(kind_name(k).into(), json!(self.0.get(kind_name(k)).copied().unwrap_or(0)));
        }
        Value::Object(m)
    }
}

#[derive(Default)]
struct FinalStats {
    recordings: usize,
    identical: usize,
    counts: Counts,
    live_words: usize,
    final_words: usize,
    ids_kept: usize,
    moved: usize,
    line_changed: usize,
    /// Share of the final paragraph (in words) before the first word that
    /// is not `Kept`, per recording that changed.
    still_prefix: Vec<f64>,
    moved_share: Vec<f64>,
}

impl FinalStats {
    fn json(&self) -> Value {
        json!({
            "recordings": self.recordings,
            "identical_to_last_preview": self.identical,
            "live_words": self.live_words,
            "final_words": self.final_words,
            "changes": self.counts.json(),
            "final_words_keeping_a_live_id_pct": pc(self.ids_kept, self.final_words),
            "kept_id_words_that_move": self.moved,
            "kept_id_words_that_change_line": self.line_changed,
            "moved_share_per_changed_recording_pct": dist(self.moved_share.iter().map(|x| x * 100.0).collect()),
            "unchanged_prefix_share_per_changed_recording_pct": dist(self.still_prefix.iter().map(|x| x * 100.0).collect()),
        })
    }
}

fn pc(a: usize, b: usize) -> f64 {
    if b == 0 {
        return f64::NAN;
    }
    (a as f64 * 1000.0 / b as f64).round() / 10.0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(1).expect("usage: revdiff DIR [WRAP_COLUMNS]"));
    let width: usize = args.get(2).and_then(|w| w.parse().ok()).unwrap_or(48);
    let mut files = Vec::new();
    logs(&dir, &mut files);
    files.sort();

    let mut live = Counts::default();
    let (mut revisions, mut non_append, mut live_moved, mut live_line, mut words_with_conf) = (0, 0, 0, 0, 0);
    let (mut intervals, mut first_lag, mut settle_lag, mut diff_us, mut seconds) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut word_counts = Vec::new();
    let mut misaligned = 0;
    let mut finals: HashMap<String, FinalStats> = HashMap::new();
    let mut recordings = 0;

    for file in &files {
        let Ok(body) = std::fs::read_to_string(file) else { continue };
        let lines: Vec<Value> = body.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        recordings += 1;
        if let Some(s) = lines.first().and_then(|h| h["seconds"].as_f64()) {
            seconds.push(s);
        }
        let mut model = WordModel::new();
        let mut prev_t: Option<f64> = None;
        // Per word id: (first seen t_ms, word start ms) and (last change t_ms, word end ms).
        let mut first: HashMap<WordId, f64> = HashMap::new();
        let mut settle: HashMap<WordId, f64> = HashMap::new();
        let mut last_preview = String::new();
        for line in &lines {
            let t = line["t_ms"].as_f64().unwrap_or(0.0);
            let text = line["text"].as_str().unwrap_or_default();
            match line["kind"].as_str() {
                Some("preview") => {
                    let before = positions(&model, width);
                    #[allow(clippy::disallowed_methods, reason = "benchmark timing, not motion")]
                    let start = Instant::now();
                    let changes = model.update(text);
                    diff_us.push(start.elapsed().as_secs_f64() * 1e6);
                    let after = positions(&model, width);
                    revisions += 1;
                    if let Some(p) = prev_t {
                        intervals.push(t - p);
                    }
                    prev_t = Some(t);
                    live.add(&changes);
                    let n_old = changes.iter().filter(|c| c.old_index.is_some()).count();
                    let append_only = changes.iter().all(|c| match c.kind {
                        ChangeKind::Kept => true,
                        // The last word may grow or gain punctuation.
                        ChangeKind::Extended | ChangeKind::Restyled => c.old_index == Some(n_old.saturating_sub(1)),
                        ChangeKind::Inserted => c.new_index.unwrap_or(0) >= n_old.saturating_sub(1),
                        _ => false,
                    });
                    if !append_only {
                        non_append += 1;
                    }
                    let (m, l) = moved(&before, &after, &changes);
                    live_moved += m;
                    live_line += l;
                    // Word timing from the recorder, when its words line up
                    // with the whitespace split.
                    let timed = line["words"].as_array().cloned().unwrap_or_default();
                    words_with_conf += timed.iter().filter(|w| !w["conf"].is_null()).count();
                    let aligned = timed.len() == model.words().len();
                    if !aligned {
                        misaligned += 1;
                    }
                    for c in &changes {
                        let (Some(ni), true) = (c.new_index, aligned) else { continue };
                        let w = &timed[ni];
                        let (s, e) =
                            (w["start"].as_f64().unwrap_or(0.0) * 1000.0, w["end"].as_f64().unwrap_or(0.0) * 1000.0);
                        if c.kind == ChangeKind::Inserted {
                            first.insert(c.id, t - s);
                        }
                        if c.kind != ChangeKind::Kept {
                            settle.insert(c.id, t - e);
                        }
                    }
                    word_counts.push(model.words().len() as f64);
                    last_preview = text.to_string();
                }
                Some("final") => {
                    let Some(text) = line["text"].as_str() else { continue };
                    let source = line["source"].as_str().unwrap_or("unknown").to_string();
                    let st = finals.entry(source).or_default();
                    st.recordings += 1;
                    if text.trim() == last_preview.trim() {
                        st.identical += 1;
                    }
                    let live_words = model.words().len();
                    let before = positions(&model, width);
                    let changes = model.update(text);
                    let after = positions(&model, width);
                    st.counts.add(&changes);
                    st.live_words += live_words;
                    st.final_words += model.words().len();
                    st.ids_kept += changes.iter().filter(|c| c.old_index.is_some() && c.new_index.is_some()).count();
                    let (m, l) = moved(&before, &after, &changes);
                    st.moved += m;
                    st.line_changed += l;
                    let n = model.words().len().max(1);
                    if changes.iter().any(|c| c.kind != ChangeKind::Kept) {
                        let first_change = changes
                            .iter()
                            .filter(|c| c.kind != ChangeKind::Kept)
                            .map(|c| c.new_index.or(c.old_index).unwrap_or(0))
                            .min()
                            .unwrap_or(0);
                        st.still_prefix.push(first_change as f64 / n as f64);
                        st.moved_share.push(m as f64 / n as f64);
                    }
                }
                _ => {}
            }
        }
        first_lag.extend(first.into_values());
        settle_lag.extend(settle.into_values());
    }

    let finals: serde_json::Map<String, Value> = finals.iter().map(|(k, v)| (k.clone(), v.json())).collect();
    let out = json!({
        "recordings": recordings,
        "wrap_columns": width,
        "audio_seconds": dist(seconds),
        "live": {
            "revisions": revisions,
            "changes": live.json(),
            "revisions_not_append_only": non_append,
            "kept_id_words_that_move": live_moved,
            "kept_id_words_that_change_line": live_line,
            "revision_interval_ms": dist(intervals),
            "word_first_shown_after_word_start_ms": dist(first_lag),
            "word_settled_after_word_end_ms": dist(settle_lag),
            "words_on_screen": dist(word_counts),
            "revisions_with_word_timing_misaligned": misaligned,
            "words_with_confidence": words_with_conf,
            "diff_time_us": dist(diff_us),
        },
        "final_swap_by_source": finals,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
