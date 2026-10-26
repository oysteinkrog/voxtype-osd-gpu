//! The preview and scrub tool (feature `preview`): any [`App`] with a [`Scenario`], played,
//! paused and scrubbed in a window, with live reload, debug overlays and contact sheets.
//!
//! An example opts in with a few lines at the top of `main`:
//!
//! ```ignore
//! if vo::preview::wanted() {
//!     let p = vo::preview::Preview::new("title_card", TitleCard::new, options, scenario)
//!         .markers_from(&timeline().schedule());
//!     std::process::exit(vo::preview::main(p));
//! }
//! ```
//!
//! Then `cargo run -p vo --example title_card -- --preview` opens it, and the `vo-preview`
//! launcher adds rebuild-and-replay on Rust changes. `--sheet OUT` writes a contact sheet and
//! `--bench-seek` measures seeking. `main` prints every option with `--preview --help`.
//!
//! - [`Player`]: the scene at any time, in any order, exactly as an export renders it.
//! - `window` (with the `wayland` feature): the window, its timeline and overlays.
//! - `control`: the socket that drives a running window.
//! - `reload`: token files and watched paths.
//! - `sheet`: contact sheets and golden checks.
//! - `source`: what the stage shows, an app or a custom draw function (`Preview::custom`).

mod chrome;
mod composite;
pub mod control;
mod player;
pub mod reload;
pub mod sheet;
mod source;
#[cfg(feature = "wayland")]
mod window;

use std::path::PathBuf;
use std::time::Instant;

pub use chrome::Overlay;
pub use player::{Player, SeekStats};

use source::{AppSource, CustomSource, Source};
use vo_render::Target;
use vo_scene::geom::Size;

use crate::{App, Options, Scenario};

/// What the window shows and how, kept across restarts in the state file.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct State {
    pub t: f64,
    pub playing: bool,
    pub speed: f64,
    pub looping: bool,
    pub range: (f64, f64),
    pub overlays: Vec<Overlay>,
    pub one_to_one: bool,
    /// Window size, logical units.
    pub size: (f32, f32),
}

impl Default for State {
    fn default() -> State {
        State {
            t: 0.0,
            playing: false,
            speed: 1.0,
            looping: false,
            range: (0.0, 0.0),
            overlays: Vec::new(),
            one_to_one: false,
            size: (1280.0, 820.0),
        }
    }
}

/// A scene ready to preview: an app with its scenario, or a custom draw function.
pub struct Preview {
    name: String,
    source: Box<dyn Source>,
    fps: f64,
    markers: Vec<(String, f64)>,
}

impl Preview {
    /// `make` builds the app in its starting state and `options` its fonts and tokens. Both
    /// run again whenever the preview replays from 0.
    pub fn new<A: App + 'static>(
        name: impl Into<String>,
        make: impl Fn() -> A + 'static,
        options: impl Fn() -> Options<A::Msg> + 'static,
        scenario: Scenario<A::Msg>,
    ) -> Preview {
        let source = Box::new(AppSource::new(Player::new(make, options, scenario)));
        Preview { name: name.into(), source, fps: 60.0, markers: Vec::new() }
    }

    /// A scene that is not an app: `make` runs once a GPU device exists and returns the
    /// function that draws frame `t` into the target it is given (an `Rgba8UnormSrgb`
    /// texture at the stage's size in pixels; `size` is the scene's logical size, which sets
    /// the aspect). The function must give the same pixels for the same `t` in any order.
    /// Overlays, tokens and the seek bench need an app and do nothing here.
    pub fn custom<D: FnMut(f64, &Target) + 'static>(
        name: impl Into<String>,
        size: Size,
        duration: f64,
        make: impl Fn(&wgpu::Device, &wgpu::Queue) -> D + 'static,
    ) -> Preview {
        let make: source::MakeDraw = Box::new(move |d, q| Box::new(make(d, q)));
        Preview {
            name: name.into(),
            source: Box::new(CustomSource::new(size, duration, make)),
            fps: 60.0,
            markers: Vec::new(),
        }
    }

    /// The frame rate that steps and sheets snap to. 60 unless set.
    pub fn fps(mut self, fps: f64) -> Preview {
        self.fps = fps;
        self
    }

    /// A named point on the timeline, to jump to.
    pub fn marker(mut self, name: impl Into<String>, t: f64) -> Preview {
        self.markers.push((name.into(), t));
        self.markers.sort_by(|a, b| a.1.total_cmp(&b.1));
        self
    }

    /// A contact sheet of this scene, for golden tests (`sheet::check_golden` compares it).
    pub fn sheet(&mut self, spec: &sheet::SheetSpec) -> Result<sheet::Sheet, crate::OfflineError> {
        sheet::render_sheet(self.source.as_mut(), spec)
    }

    /// Every marker of a timeline schedule.
    pub fn markers_from(mut self, schedule: &vo_core::timeline::Schedule) -> Preview {
        for c in schedule.cues(0.0) {
            if c.edge == vo_core::timeline::Edge::Marker {
                self = self.marker(c.name, c.t);
            }
        }
        self
    }
}

/// The first argument asks for a preview mode: `--preview`, `--sheet` or `--bench-seek`.
pub fn wanted() -> bool {
    std::env::args().nth(1).is_some_and(|a| matches!(a.as_str(), "--preview" | "--sheet" | "--bench-seek"))
}

const HELP: &str = "\
Preview modes (the first argument picks one):

  --preview [options]     a window with a timeline, scrubbing and live reload
      --at T              start at T seconds (VO_PREVIEW_AT)
      --play              start playing
      --speed S           0.25 to 2
      --overlay NAME      bounds, springs, graph or stats; repeat for more
      --tokens PATH       TOML token file, reloaded on change (VO_PREVIEW_TOKENS)
      --shaders DIR       WGSL directory, reloaded on change (VO_SHADER_DIR)
      --control SOCK      control socket for `vo-preview ctl` (VO_PREVIEW_CONTROL)
      --state PATH        write the state here on exit (VO_PREVIEW_STATE)
      --resume PATH       start from a state file (VO_PREVIEW_RESUME)
  --sheet OUT.png [options]  a contact sheet of frames across a range
      --from A --to B     range in seconds (default the whole scenario)
      --frames N          frames on the sheet (default 12)
      --cols C            columns (default 4)
      --thumb W           thumbnail width in pixels (default 400)
      --tokens PATH       render with these tokens
      --check GOLDEN.png  compare with a golden sheet; exit 1 on a mismatch
  --bench-seek [N]        time N random seeks and a backwards scrub (default 500)
";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Runs the mode the command line asks for and returns the process exit code.
pub fn main(preview: Preview) -> i32 {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return 0;
    }
    let result = match mode.as_str() {
        "--preview" => run_window(preview, &rest),
        "--sheet" => run_sheet(preview, &rest),
        "--bench-seek" => bench_seek(preview, &rest),
        _ => Err(format!("unknown preview mode '{mode}'\n{HELP}")),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("vo-preview: {e}");
            2
        }
    }
}

/// `--flag value` pairs and bare flags.
struct Args<'a> {
    rest: &'a [String],
}

impl Args<'_> {
    fn value(&self, flag: &str) -> Option<&str> {
        self.rest.iter().position(|a| a == flag).and_then(|i| self.rest.get(i + 1)).map(|s| s.as_str())
    }

    fn values(&self, flag: &str) -> Vec<&str> {
        self.rest.windows(2).filter(|w| w[0] == flag).map(|w| w[1].as_str()).collect()
    }

    fn has(&self, flag: &str) -> bool {
        self.rest.iter().any(|a| a == flag)
    }

    fn num(&self, flag: &str) -> Result<Option<f64>, String> {
        self.value(flag).map(|v| v.parse::<f64>().map_err(|e| format!("{flag} {v}: {e}"))).transpose()
    }
}

fn load_tokens(source: &mut dyn Source, path: Option<&PathBuf>) -> Result<(), String> {
    if let Some(p) = path {
        let text = std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        source.set_tokens(reload::parse_tokens(&text).map_err(|e| format!("{}: {e}", p.display()))?)?;
    }
    Ok(())
}

#[cfg(feature = "wayland")]
fn run_window(mut preview: Preview, rest: &[String]) -> Result<i32, String> {
    use vo_host::Host;
    let a = Args { rest };
    let path = |flag: &str, env: &str| a.value(flag).map(PathBuf::from).or_else(|| env_path(env));
    let tokens = path("--tokens", "VO_PREVIEW_TOKENS");
    load_tokens(preview.source.as_mut(), tokens.as_ref())?;
    let mut state = State::default();
    let resumed = match path("--resume", "VO_PREVIEW_RESUME").map(std::fs::read_to_string) {
        Some(Ok(text)) => {
            state = serde_json::from_str(&text).map_err(|e| format!("resume state: {e}"))?;
            true
        }
        _ => false,
    };
    // A resumed state wins over the starting options, which the launcher passes again.
    if !resumed {
        if let Some(t) = a.num("--at")?.or_else(|| std::env::var("VO_PREVIEW_AT").ok()?.parse().ok()) {
            state.t = t;
        }
        if a.has("--play") {
            state.playing = true;
        }
        if let Some(s) = a.num("--speed")? {
            state.speed = s;
        }
    }
    for o in a.values("--overlay").into_iter().filter(|_| !resumed) {
        let o = control::parse_overlay(o).ok_or_else(|| format!("no overlay '{o}'"))?;
        if !state.overlays.contains(&o) {
            state.overlays.push(o);
        }
    }
    state.t = state.t.clamp(0.0, preview.source.duration());
    let shaders = path("--shaders", "VO_SHADER_DIR");
    if let Some(dir) = &shaders {
        // vo-render reads shader files from here (`vo_render::hot`). Set before any thread starts.
        std::env::set_var("VO_SHADER_DIR", dir);
    }
    let setup = window::Setup {
        fps: preview.fps,
        markers: preview.markers,
        state,
        control: path("--control", "VO_PREVIEW_CONTROL"),
        state_path: path("--state", "VO_PREVIEW_STATE"),
        tokens,
        shaders,
        app_id: format!("vo-preview-{}", preview.name),
    };
    let mut win = window::Window::new(preview.source, setup);
    let host = vo_host_wayland::WaylandHost::connect().map_err(|e| e.to_string())?;
    let result = host.run(&mut win);
    match win.error.take().map(Err).unwrap_or(result) {
        Ok(()) => Ok(0),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(not(feature = "wayland"))]
fn run_window(_: Preview, _: &[String]) -> Result<i32, String> {
    Err("the preview window needs the `wayland` feature".into())
}

fn run_sheet(mut preview: Preview, rest: &[String]) -> Result<i32, String> {
    let a = Args { rest };
    let out = PathBuf::from(rest.first().filter(|s| !s.starts_with("--")).ok_or("--sheet needs an output path")?);
    load_tokens(preview.source.as_mut(), a.value("--tokens").map(PathBuf::from).as_ref())?;
    let duration = preview.source.duration();
    let spec = sheet::SheetSpec {
        from: a.num("--from")?.unwrap_or(0.0),
        to: a.num("--to")?.unwrap_or(duration),
        frames: a.num("--frames")?.unwrap_or(12.0) as usize,
        cols: a.num("--cols")?.unwrap_or(4.0) as usize,
        thumb_w: a.num("--thumb")?.unwrap_or(400.0) as u32,
        fps: preview.fps,
    };
    let started = Instant::now();
    let sheet = sheet::render_sheet(preview.source.as_mut(), &spec).map_err(|e| e.to_string())?;
    sheet::write_sheet(&sheet, &out).map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "vo-preview: sheet {} ({}x{}, {} frames from {:.3} to {:.3} s) in {:.2} s",
        out.display(),
        sheet.width,
        sheet.height,
        spec.frames,
        spec.from,
        spec.to,
        started.elapsed().as_secs_f64()
    );
    if let Some(golden) = a.value("--check") {
        let report = sheet::check_golden(&sheet, golden.as_ref(), &out)?;
        println!(
            "vo-preview: {} of {} pixels differ from {} (diff {})",
            report.differing,
            report.total,
            golden,
            report.diff.display()
        );
        return Ok(if report.passed() { 0 } else { 1 });
    }
    Ok(0)
}

fn bench_seek(mut preview: Preview, rest: &[String]) -> Result<i32, String> {
    let n = rest.first().and_then(|s| s.parse::<usize>().ok()).unwrap_or(500);
    let p = preview.source.as_mut();
    if p.seek_stats().is_none() {
        return Err("--bench-seek needs an app; a custom scene draws straight from t".into());
    }
    let fps = preview.fps;
    let duration = p.duration();
    let frames = (duration * fps).round() as u64;
    let timed = |p: &mut dyn Source, t: f64| {
        let started = Instant::now();
        p.seek(t);
        (started.elapsed().as_secs_f64() * 1e3, p.seek_stats().is_some_and(|s| s.replayed))
    };
    let pct = |v: &mut Vec<f64>, q: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * q).round() as usize]
    };
    let report = |name: &str, mut v: Vec<(f64, bool)>| {
        let replays = v.iter().filter(|x| x.1).count();
        let mut ms: Vec<f64> = v.drain(..).map(|x| x.0).collect();
        let mean = ms.iter().sum::<f64>() / ms.len() as f64;
        println!(
            "vo-preview: {name}: {} seeks, mean {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms, {replays} replays from 0",
            ms.len(),
            mean,
            pct(&mut ms, 0.5),
            pct(&mut ms, 0.95),
            pct(&mut ms, 1.0)
        );
    };
    let events = p.event_times().len();
    println!("vo-preview: {duration} s at {fps} fps ({frames} frames), {events} events");
    let forward: Vec<_> = (0..=frames).map(|i| timed(p, i as f64 / fps)).collect();
    report("play forward, one frame at a time", forward);
    let backward: Vec<_> = (0..=frames).rev().map(|i| timed(p, i as f64 / fps)).collect();
    report("scrub backwards, one frame at a time", backward);
    let random: Vec<_> = (0..n as u32)
        .map(|k| {
            let i = (vo_core::hash::rand_f64(11, k, 0, 0) * (frames + 1) as f64) as u64;
            timed(p, i.min(frames) as f64 / fps)
        })
        .collect();
    report("random frames", random);
    let ends: Vec<_> = (0..50).map(|k| timed(p, if k % 2 == 0 { duration } else { 0.0 })).collect();
    report("jump end to start and back", ends);
    Ok(0)
}
