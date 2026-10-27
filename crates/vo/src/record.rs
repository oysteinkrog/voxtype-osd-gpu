//! Recording an app's messages, to replay them later as a [`Scenario`] (feature `record`).
//!
//! A live app records every message it is sent, with the time since the recording started,
//! one JSON line each: `{"t":1.25,"msg":...}`. [`load`] reads the file back and [`scenario`]
//! turns it into the inputs of a scenario, so the same dictation plays in the preview, in an
//! offline render or in a golden test, frame for frame.
//!
//! ```ignore
//! let rec = vo::record::Recorder::create("/var/tmp/claude/dictation.jsonl")?;
//! rec.record(&Msg::Audio(frames)); // from any thread
//! // later
//! let events = vo::record::load::<Msg>(path)?;
//! let scenario = vo::record::scenario(size, events, 1.0);
//! ```

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use vo_scene::geom::Size;

use crate::{Input, Scenario};

#[derive(Serialize, Deserialize)]
struct Line<M> {
    t: f64,
    msg: M,
}

/// Writes messages to a file as they come. Cheap to clone; every clone writes to the same
/// file, so reader threads can each hold one.
#[derive(Clone)]
pub struct Recorder {
    inner: Arc<Mutex<(BufWriter<File>, Instant)>>,
}

impl Recorder {
    /// Creates `path` (and its folder) and starts the clock.
    pub fn create(path: impl AsRef<Path>) -> std::io::Result<Recorder> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let file = BufWriter::new(File::create(path)?);
        Ok(Recorder { inner: Arc::new(Mutex::new((file, Instant::now()))) })
    }

    /// Appends `msg` at the time since [`Recorder::create`]. Each line is flushed, so a
    /// recording cut off by a crash is still readable up to its last message.
    pub fn record<M: Serialize>(&self, msg: &M) -> std::io::Result<()> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let t = g.1.elapsed().as_secs_f64();
        self.write(&mut g.0, t, msg)
    }

    /// Appends `msg` at time `t`, for messages whose time is known already.
    pub fn record_at<M: Serialize>(&self, t: f64, msg: &M) -> std::io::Result<()> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.write(&mut g.0, t, msg)
    }

    fn write<M: Serialize>(&self, w: &mut BufWriter<File>, t: f64, msg: &M) -> std::io::Result<()> {
        serde_json::to_writer(&mut *w, &Line { t, msg })?;
        w.write_all(b"\n")?;
        w.flush()
    }
}

/// Reads a recording: each message with its time, in file order. A line that does not parse
/// is an error naming its line number; a cut-off last line is skipped.
pub fn load<M: DeserializeOwned>(path: impl AsRef<Path>) -> Result<Vec<(f64, M)>, String> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let lines: Vec<String> = BufReader::new(file).lines().collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Line<M>>(l) {
            Ok(line) => out.push((line.t, line.msg)),
            Err(_) if i + 1 == lines.len() => break,
            Err(e) => return Err(format!("{} line {}: {e}", path.display(), i + 1)),
        }
    }
    Ok(out)
}

/// A scenario that sends the recorded messages at their times, sorted, and runs `tail`
/// seconds past the last one.
pub fn scenario<M>(size: Size, mut events: Vec<(f64, M)>, tail: f64) -> Scenario<M> {
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let end = events.last().map_or(0.0, |e| e.0);
    Scenario {
        size,
        scale: 1.0,
        duration: end + tail.max(0.0),
        inputs: events.into_iter().map(|(t, m)| (t.max(0.0), Input::Msg(m))).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    enum Msg {
        Level(f32),
        Text(String),
    }

    #[test]
    fn a_recording_plays_back_as_a_scenario() {
        let dir = std::path::PathBuf::from(format!("/var/tmp/claude/vo-record-test-{}", std::process::id()));
        let path = dir.join("a.jsonl");
        let rec = Recorder::create(&path).unwrap();
        let other = rec.clone();
        rec.record_at(0.5, &Msg::Text("hello".into())).unwrap();
        other.record_at(0.25, &Msg::Level(0.3)).unwrap();
        rec.record(&Msg::Level(0.1)).unwrap();
        drop((rec, other));
        let events: Vec<(f64, Msg)> = load(&path).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], (0.5, Msg::Text("hello".into())));
        let s = scenario(Size::new(100.0, 50.0), events, 1.0);
        // The message recorded by the clock came right after creation, so it sorts first.
        let times: Vec<f64> = s.inputs.iter().map(|i| i.0).collect();
        assert!(times[0] < 0.25 && times[1..] == [0.25, 0.5], "{times:?}");
        assert_eq!(s.duration, 1.5);
        // A cut-off last line is skipped; a bad line in the middle is an error.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"t\":2.0,\"msg\":{\"Lev");
        std::fs::write(&path, &text).unwrap();
        assert_eq!(load::<Msg>(&path).unwrap().len(), 3);
        std::fs::write(&path, format!("nonsense\n{text}")).unwrap();
        assert!(load::<Msg>(&path).unwrap_err().contains("line 1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
