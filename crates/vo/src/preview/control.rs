//! The preview's control socket: one command per line on a Unix socket, one reply line back.
//! The `vo-preview` launcher uses it to read the state before a restart, and scripts and
//! agents use it to drive the window (`vo-preview ctl seek 2.5`).
//!
//! Commands:
//!
//! | Command | Effect |
//! |---|---|
//! | `seek T` | go to `T` seconds and pause |
//! | `play`, `pause`, `toggle` | transport |
//! | `step N` | move `N` frames (negative goes back) and pause |
//! | `speed S` | playback speed, 0.25 to 2 |
//! | `marker NAME` | go to a named marker |
//! | `loop A B`, `loop off` | loop a range |
//! | `overlay bounds\|springs\|graph\|stats\|effects on\|off` | debug overlays |
//! | `ref PATH` | load a reference image (`ref off` unloads it) |
//! | `ref mode over\|under\|diff\|split\|off` | how the reference shows |
//! | `ref opacity O`, `ref split S` | 0 to 1 |
//! | `ref crop X Y W [H]` | the part of the image to use, in its pixels; `ref crop full` |
//! | `ref backdrop ref\|#rrggbb` | what the stage is compared over in diff mode |
//! | `ref score` | reply with how far the frame is from the reference |
//! | `fit` / `1:1` | stage scale |
//! | `size W H` | resize the window (logical units) |
//! | `shot PATH` | write the window as a PNG |
//! | `state` | reply with the state as JSON |
//! | `quit` | write the state file and exit |

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use vo_host::Waker;

use super::chrome::Overlay;

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Seek(f64),
    Play,
    Pause,
    Toggle,
    Step(i64),
    Speed(f64),
    Marker(String),
    Loop(Option<(f64, f64)>),
    Overlay(Overlay, bool),
    OneToOne(bool),
    Size(f32, f32),
    Shot(PathBuf),
    State,
    Quit,
    Ref(RefCmd),
}

#[derive(Clone, Debug, PartialEq)]
pub enum RefCmd {
    Load(PathBuf),
    Unload,
    Mode(super::reference::RefMode),
    Opacity(f32),
    Split(f32),
    /// x, y, w and h or x, y, w (the height follows the stage), or `None` for the whole image.
    Crop(Option<Vec<f32>>),
    Backdrop(super::reference::Backdrop),
    Score,
}

pub fn overlay_name(o: Overlay) -> &'static str {
    match o {
        Overlay::Bounds => "bounds",
        Overlay::Springs => "springs",
        Overlay::Graph => "graph",
        Overlay::Stats => "stats",
        Overlay::Effects => "effects",
    }
}

pub fn parse_overlay(s: &str) -> Option<Overlay> {
    [Overlay::Bounds, Overlay::Springs, Overlay::Graph, Overlay::Stats, Overlay::Effects]
        .into_iter()
        .find(|o| overlay_name(*o) == s)
}

pub fn parse(line: &str) -> Result<Cmd, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let num = |i: usize| -> Result<f64, String> {
        words.get(i).ok_or_else(|| format!("'{line}' needs a number"))?.parse::<f64>().map_err(|e| format!("{e}"))
    };
    Ok(match words.as_slice() {
        ["seek", _] => Cmd::Seek(num(1)?),
        ["play"] => Cmd::Play,
        ["pause"] => Cmd::Pause,
        ["toggle"] => Cmd::Toggle,
        ["step"] => Cmd::Step(1),
        ["step", _] => Cmd::Step(num(1)? as i64),
        ["speed", _] => Cmd::Speed(num(1)?),
        ["marker", name] => Cmd::Marker(name.to_string()),
        ["loop", "off"] => Cmd::Loop(None),
        ["loop", _, _] => Cmd::Loop(Some((num(1)?, num(2)?))),
        ["overlay", o, on @ ("on" | "off")] => {
            Cmd::Overlay(parse_overlay(o).ok_or_else(|| format!("no overlay '{o}'"))?, *on == "on")
        }
        ["fit"] => Cmd::OneToOne(false),
        ["1:1"] => Cmd::OneToOne(true),
        ["size", _, _] => Cmd::Size(num(1)? as f32, num(2)? as f32),
        ["shot", path] => Cmd::Shot(PathBuf::from(path)),
        ["state"] => Cmd::State,
        ["quit"] => Cmd::Quit,
        ["ref", "off"] => Cmd::Ref(RefCmd::Unload),
        ["ref", "score"] => Cmd::Ref(RefCmd::Score),
        ["ref", "mode", m] => Cmd::Ref(RefCmd::Mode(
            super::reference::RefMode::parse(m).ok_or_else(|| format!("no reference mode '{m}'"))?,
        )),
        ["ref", "opacity", _] => Cmd::Ref(RefCmd::Opacity(num(2)? as f32)),
        ["ref", "split", _] => Cmd::Ref(RefCmd::Split(num(2)? as f32)),
        ["ref", "crop", "full"] => Cmd::Ref(RefCmd::Crop(None)),
        ["ref", "crop", rest @ ..] if (3..=4).contains(&rest.len()) => Cmd::Ref(RefCmd::Crop(Some(
            (2..2 + rest.len()).map(num).collect::<Result<Vec<f64>, _>>()?.into_iter().map(|v| v as f32).collect(),
        ))),
        ["ref", "backdrop", b] => Cmd::Ref(RefCmd::Backdrop(
            super::reference::Backdrop::parse(b).ok_or_else(|| format!("backdrop '{b}': give ref or #rrggbb"))?,
        )),
        ["ref", path] => Cmd::Ref(RefCmd::Load(PathBuf::from(path))),
        _ => return Err(format!("unknown command '{line}'; see `vo-preview ctl help`")),
    })
}

/// A command and where its reply goes.
pub type Request = (Cmd, mpsc::Sender<String>);

/// Listens on `path` on a thread of its own. Each command is sent on the returned channel and
/// the window is woken; the reply goes back to the client.
pub fn listen(path: &Path, waker: Waker) -> std::io::Result<mpsc::Receiver<Request>> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new().name("vo-preview-control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let Ok(read) = stream.try_clone() else { continue };
            let mut out = stream;
            for line in BufReader::new(read).lines().map_while(Result::ok) {
                let reply = match parse(line.trim()) {
                    Ok(cmd) => {
                        let (rtx, rrx) = mpsc::channel();
                        if tx.send((cmd, rtx)).is_err() {
                            return;
                        }
                        waker.wake();
                        rrx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| "error: no reply".into())
                    }
                    Err(e) => format!("error: {e}"),
                };
                if writeln!(out, "{reply}").is_err() {
                    break;
                }
            }
        }
    })?;
    Ok(rx)
}

/// Sends one command and returns the reply line.
pub fn send(path: &Path, line: &str) -> std::io::Result<String> {
    let mut s = std::os::unix::net::UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(15)))?;
    writeln!(s, "{line}")?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply)?;
    Ok(reply.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse("seek 2.5"), Ok(Cmd::Seek(2.5)));
        assert_eq!(parse("step -3"), Ok(Cmd::Step(-3)));
        assert_eq!(parse("loop 1 2.5"), Ok(Cmd::Loop(Some((1.0, 2.5)))));
        assert_eq!(parse("loop off"), Ok(Cmd::Loop(None)));
        assert_eq!(parse("overlay springs on"), Ok(Cmd::Overlay(Overlay::Springs, true)));
        assert_eq!(parse("marker out"), Ok(Cmd::Marker("out".into())));
        assert!(parse("overlay nope on").is_err());
        assert!(parse("seek x").is_err());
        assert!(parse("dance").is_err());
        assert_eq!(parse("overlay effects on"), Ok(Cmd::Overlay(Overlay::Effects, true)));
        assert_eq!(parse("ref /x/mock.png"), Ok(Cmd::Ref(RefCmd::Load("/x/mock.png".into()))));
        assert_eq!(parse("ref mode diff"), Ok(Cmd::Ref(RefCmd::Mode(super::super::reference::RefMode::Diff))));
        assert_eq!(parse("ref opacity 0.3"), Ok(Cmd::Ref(RefCmd::Opacity(0.3))));
        assert_eq!(parse("ref crop 10 20 300"), Ok(Cmd::Ref(RefCmd::Crop(Some(vec![10.0, 20.0, 300.0])))));
        assert_eq!(parse("ref crop full"), Ok(Cmd::Ref(RefCmd::Crop(None))));
        assert_eq!(parse("ref score"), Ok(Cmd::Ref(RefCmd::Score)));
        assert!(parse("ref mode sideways").is_err());
        assert!(parse("ref backdrop blue").is_err());
    }
}
