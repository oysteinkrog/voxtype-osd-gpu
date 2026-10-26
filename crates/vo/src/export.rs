//! Export to a PNG sequence or a video file (feature `export`, plan decision 10).
//!
//! The app runs through a [`Scenario`] exactly as in [`render_offline`](crate::render_offline),
//! and frame `n` is sampled at exactly `n / fps`. Frames come back through the offscreen
//! readback ring, so the GPU draws frame `n + 1` while a writer thread saves frame `n`. Video goes
//! through an `ffmpeg` child process that reads raw RGBA frames on stdin.
//!
//! Frames are premultiplied, so ffmpeg dropping the alpha channel is the same as compositing
//! over black. Give the root an opaque background for any other colour.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;

use vo_render::offscreen::{write_png, Frame};

use crate::app::App;
use crate::offline::{drive, Frames};
use crate::{OfflineError, Options, Scenario};

/// What to export and where.
#[derive(Clone, Debug)]
pub struct Export {
    pub fps: f64,
    /// Seconds. The export has `round(duration * fps)` frames, whether or not the app exits.
    pub duration: f64,
    /// Writes `frame_00000.png` and onwards here when set.
    pub png_dir: Option<PathBuf>,
    pub video: Option<Video>,
}

impl Export {
    pub fn frame_count(&self) -> u64 {
        (self.duration * self.fps).round().max(0.0) as u64
    }
}

/// A video file written by an `ffmpeg` child process.
#[derive(Clone, Debug)]
pub struct Video {
    pub path: PathBuf,
    /// The ffmpeg program. [`Video::h264`] takes `VO_FFMPEG` from the environment, else
    /// `ffmpeg` on `PATH`.
    pub ffmpeg: PathBuf,
    /// Output arguments, between the raw input and the path: codec, quality, pixel format.
    pub args: Vec<String>,
}

impl Video {
    /// H.264 in yuv420p at CRF 16, with no encoder or time metadata, so the same frames give
    /// the same file.
    pub fn h264(path: impl Into<PathBuf>) -> Video {
        let args = "-c:v libx264 -preset medium -crf 16 -pix_fmt yuv420p -movflags +faststart \
                    -map_metadata -1 -fflags +bitexact -flags:v +bitexact";
        Video {
            path: path.into(),
            ffmpeg: std::env::var_os("VO_FFMPEG").map_or_else(|| "ffmpeg".into(), PathBuf::from),
            args: args.split_whitespace().map(String::from).collect(),
        }
    }
}

/// What an export produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportReport {
    pub frames: u64,
    pub width: u32,
    pub height: u32,
    /// A 64-bit hash of each frame's pixels, for determinism checks.
    pub frame_hashes: Vec<u64>,
}

#[derive(Debug)]
pub enum ExportError {
    Render(OfflineError),
    Io(std::io::Error),
    /// The ffmpeg program could not be started.
    FfmpegMissing(PathBuf),
    /// ffmpeg exited with an error. Its own message went to stderr.
    Ffmpeg(String),
    /// The writer thread panicked or stopped early.
    Writer,
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::Render(e) => write!(f, "render: {e}"),
            ExportError::Io(e) => write!(f, "{e}"),
            ExportError::FfmpegMissing(p) => write!(
                f,
                "cannot run {:?}: video export needs ffmpeg. Install it (for example `pacman -S ffmpeg` \
                 or `apt install ffmpeg`), set VO_FFMPEG to its path, or export a PNG sequence instead",
                p
            ),
            ExportError::Ffmpeg(s) => write!(f, "ffmpeg failed: {s}"),
            ExportError::Writer => write!(f, "the frame writer stopped early"),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<OfflineError> for ExportError {
    fn from(e: OfflineError) -> ExportError {
        ExportError::Render(e)
    }
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> ExportError {
        ExportError::Io(e)
    }
}

/// Exports `app` through `scenario`. See [`export_with`] to see each frame as well.
pub fn export<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    spec: &Export,
    options: Options<A::Msg>,
) -> Result<ExportReport, ExportError> {
    export_with(app, scenario, spec, options, |_, _| {})
}

/// [`export`] that also hands every frame to `on_frame` with its number, on the writer
/// thread, for contact sheets and checks.
pub fn export_with<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    spec: &Export,
    options: Options<A::Msg>,
    on_frame: impl FnMut(u64, &Frame) + Send,
) -> Result<ExportReport, ExportError> {
    if let Some(dir) = &spec.png_dir {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(parent) = spec.video.as_ref().and_then(|v| v.path.parent()) {
        std::fs::create_dir_all(parent)?;
    }
    // Fail before rendering anything when ffmpeg is not there.
    if let Some(v) = &spec.video {
        check_ffmpeg(&v.ffmpeg)?;
    }
    // Two frames in flight between the render loop and the writer, on top of the ring.
    let (tx, rx) = mpsc::sync_channel::<Frame>(2);
    std::thread::scope(|s| {
        let writer = s.spawn(move || write_frames(spec, rx, on_frame));
        let rendered = drive(app, scenario, spec.fps, Frames::Count(spec.frame_count()), options, &mut |frame| {
            // A send fails only when the writer stopped; its own error is the one to report.
            tx.send(frame).map_err(|_| ExportError::Writer)
        });
        drop(tx);
        let written = writer.join().map_err(|_| ExportError::Writer)?;
        match (rendered, written) {
            (_, Err(e)) | (Err(e), _) => Err(e),
            (Ok(n), Ok(report)) if n == report.frames => Ok(report),
            (Ok(_), Ok(_)) => Err(ExportError::Writer),
        }
    })
}

fn check_ffmpeg(program: &std::path::Path) -> Result<(), ExportError> {
    let status = Command::new(program)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| ExportError::FfmpegMissing(program.into()))?;
    if !status.success() {
        return Err(ExportError::FfmpegMissing(program.into()));
    }
    Ok(())
}

struct Ffmpeg {
    child: Child,
    stdin: ChildStdin,
}

impl Ffmpeg {
    fn spawn(video: &Video, fps: f64, width: u32, height: u32) -> Result<Ffmpeg, ExportError> {
        let mut child = Command::new(&video.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{width}x{height}"), "-framerate", &fps.to_string(), "-i", "-"])
            .args(&video.args)
            .arg(&video.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|_| ExportError::FfmpegMissing(video.ffmpeg.clone()))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        Ok(Ffmpeg { child, stdin })
    }

    fn finish(self) -> Result<(), ExportError> {
        let Ffmpeg { mut child, stdin } = self;
        drop(stdin);
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(ExportError::Ffmpeg(status.to_string()))
        }
    }
}

fn write_frames(
    spec: &Export,
    rx: mpsc::Receiver<Frame>,
    mut on_frame: impl FnMut(u64, &Frame),
) -> Result<ExportReport, ExportError> {
    let mut report = ExportReport { frames: 0, width: 0, height: 0, frame_hashes: Vec::new() };
    let mut ffmpeg: Option<Ffmpeg> = None;
    for frame in rx {
        let n = report.frames;
        if n == 0 {
            (report.width, report.height) = (frame.width, frame.height);
            if let Some(v) = &spec.video {
                ffmpeg = Some(Ffmpeg::spawn(v, spec.fps, frame.width, frame.height)?);
            }
        }
        report.frame_hashes.push(frame_hash(&frame.rgba));
        on_frame(n, &frame);
        if let Some(dir) = &spec.png_dir {
            write_png(&dir.join(format!("frame_{n:05}.png")), frame.width, frame.height, &frame.rgba)?;
        }
        if let Some(f) = &mut ffmpeg {
            if let Err(e) = f.stdin.write_all(&frame.rgba) {
                // A broken pipe means ffmpeg exited; its status says why.
                let status = ffmpeg.take().map(Ffmpeg::finish);
                return Err(match status {
                    Some(Err(fe)) => fe,
                    _ => ExportError::Io(e),
                });
            }
        }
        report.frames += 1;
    }
    if let Some(f) = ffmpeg {
        f.finish()?;
    }
    Ok(report)
}

/// FxHash-style mix over 8-byte words. Fast enough to hash every 1080p frame of an export.
pub fn frame_hash(rgba: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = rgba.len() as u64;
    let words = rgba.chunks_exact(8);
    let tail = words.remainder();
    for w in words {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(K);
    }
    for &b in tail {
        h = (h.rotate_left(5) ^ b as u64).wrapping_mul(K);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_is_exact() {
        let e = Export { fps: 60.0, duration: 10.0, png_dir: None, video: None };
        assert_eq!(e.frame_count(), 600);
        let e = Export { fps: 30.0, duration: 0.1, ..e };
        assert_eq!(e.frame_count(), 3);
    }

    #[test]
    fn frame_hash_sees_one_byte() {
        let mut a = vec![0u8; 1923 * 4];
        let h = frame_hash(&a);
        a[1923 * 4 - 1] = 1;
        assert_ne!(frame_hash(&a), h);
        a[1923 * 4 - 1] = 0;
        a[17] = 1;
        assert_ne!(frame_hash(&a), h);
    }

    #[test]
    fn missing_ffmpeg_is_a_clear_error() {
        let err = check_ffmpeg("/nonexistent/ffmpeg".as_ref()).unwrap_err();
        assert!(matches!(err, ExportError::FfmpegMissing(_)));
        assert!(err.to_string().contains("needs ffmpeg"), "{err}");
    }
}
