//! `vo-preview`: the launcher for the preview and scrub tool.
//!
//! `vo-preview run EXAMPLE` builds the example, runs it with `--preview` and watches the
//! workspace's Rust sources. After a change it rebuilds, asks the running window for its state
//! (time, play state, loop, overlays, size) over the control socket, closes it and starts the
//! new build at the same state, so a Rust edit replays to the same `t` (plan decision 4).
//! Token files and WGSL reload inside the running window without a restart.
//!
//! The example opts in with `vo::preview::wanted()` and `vo::preview::main` (see the
//! `vo::preview` docs). Set `VO_PREVIEW_BUILD` to wrap cargo, for example
//! `VO_PREVIEW_BUILD="heavy-build cargo"`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime};

use vo::preview::control;

const HELP: &str = "\
vo-preview: preview and scrub any vo example that opts in with vo::preview::main

USAGE:
  vo-preview run EXAMPLE [-p PACKAGE] [--release] [--tokens PATH] [--shaders DIR] [-- ARGS]
      Build EXAMPLE, open its preview window, rebuild and replay to the same time when a
      .rs or Cargo.toml file in the workspace changes. ARGS go to `EXAMPLE --preview`
      (for example `--at 2.5 --overlay bounds`).
  vo-preview sheet EXAMPLE OUT.png [-p PACKAGE] [SHEET OPTIONS]
      Contact sheet: --from A --to B --frames N --cols C --thumb W --check GOLDEN.png
  vo-preview bench EXAMPLE [N] [-p PACKAGE]
      Time seeks: forward play, backwards scrub, N random frames.
  vo-preview ctl COMMAND...
      Drive a running window: seek T, play, pause, step N, speed S, marker NAME,
      loop A B, loop off, overlay NAME on|off, fit, 1:1, size W H, shot PATH, state, quit.

ENVIRONMENT:
  VO_PREVIEW_BUILD     the cargo command, split on spaces (default `cargo`)
  VO_PREVIEW_CONTROL   control socket (default $XDG_RUNTIME_DIR/vo-preview.sock)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("sheet") => once(&args[1..], "--sheet"),
        Some("bench") => once(&args[1..], "--bench-seek"),
        Some("ctl") => ctl(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            print!("{HELP}");
            Ok(0)
        }
        Some(other) => Err(format!("unknown command '{other}'\n\n{HELP}")),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("vo-preview: {e}");
            ExitCode::from(2)
        }
    }
}

fn socket() -> PathBuf {
    std::env::var_os("VO_PREVIEW_CONTROL").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        dir.join("vo-preview.sock")
    })
}

fn ctl(words: &[String]) -> Result<u8, String> {
    if words.is_empty() || words[0] == "help" {
        print!("{HELP}");
        return Ok(0);
    }
    let path = socket();
    let reply = control::send(&path, &words.join(" ")).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("{reply}");
    Ok(if reply.starts_with("error") { 1 } else { 0 })
}

/// The example, the package and everything else.
struct Target {
    example: String,
    package: String,
    release: bool,
    rest: Vec<String>,
}

fn target(args: &[String]) -> Result<Target, String> {
    let example = args.first().filter(|a| !a.starts_with('-')).ok_or("which example? see --help")?.clone();
    let mut t = Target { example, package: "vo".into(), release: false, rest: Vec::new() };
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => t.package = it.next().ok_or("-p needs a package")?.clone(),
            "--release" => t.release = true,
            _ => t.rest.push(a.clone()),
        }
    }
    Ok(t)
}

fn workspace_root() -> Result<PathBuf, String> {
    let out = Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .output()
        .map_err(|e| format!("cargo: {e}"))?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Path::new(&path).parent().map(Path::to_path_buf).ok_or_else(|| "not inside a cargo workspace".into())
}

/// Builds the example and returns its executable. Diagnostics go to stderr as cargo renders
/// them.
fn build(t: &Target) -> Result<PathBuf, String> {
    let cmd = std::env::var("VO_PREVIEW_BUILD").unwrap_or_else(|_| "cargo".into());
    let mut words = cmd.split_whitespace();
    let program = words.next().ok_or("VO_PREVIEW_BUILD is empty")?;
    let mut c = Command::new(program);
    c.args(words)
        .args(["build", "-p", &t.package, "--example", &t.example, "--message-format=json-render-diagnostics"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if t.release {
        c.arg("--release");
    }
    let out = c.output().map_err(|e| format!("{program}: {e}"))?;
    let mut exe = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["reason"] == "compiler-message" {
            if let Some(r) = v["message"]["rendered"].as_str() {
                eprint!("{r}");
            }
        }
        if v["reason"] == "compiler-artifact" && v["target"]["name"] == t.example.as_str() {
            exe = v["executable"].as_str().map(PathBuf::from);
        }
    }
    if !out.status.success() {
        return Err(format!("build failed ({})", out.status));
    }
    exe.ok_or_else(|| format!("cargo built no executable for example '{}'", t.example))
}

/// `sheet` and `bench`: build, run once, pass the exit code on.
fn once(args: &[String], mode: &str) -> Result<u8, String> {
    let t = target(args)?;
    let exe = build(&t)?;
    let status = Command::new(exe).arg(mode).args(&t.rest).status().map_err(|e| e.to_string())?;
    Ok(status.code().unwrap_or(1) as u8)
}

/// Every `.rs` and `Cargo.toml` under `dir`, skipping `target` and hidden directories.
fn sources(dir: &Path, out: &mut Vec<(PathBuf, Option<SystemTime>)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if p.is_dir() {
            if name != "target" && !name.starts_with('.') {
                sources(&p, out);
            }
        } else if name.ends_with(".rs") || name == "Cargo.toml" {
            let m = e.metadata().and_then(|m| m.modified()).ok();
            out.push((p, m));
        }
    }
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut v = Vec::new();
    sources(root, &mut v);
    v.sort();
    v
}

struct Running {
    child: Child,
    started: Instant,
}

fn spawn(exe: &Path, t: &Target, sock: &Path, state: &Path, resume: bool) -> Result<Running, String> {
    let mut c = Command::new(exe);
    c.arg("--preview").args(&t.rest).env("VO_PREVIEW_CONTROL", sock).env("VO_PREVIEW_STATE", state);
    if resume {
        c.env("VO_PREVIEW_RESUME", state);
    }
    let child = c.spawn().map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(Running { child, started: Instant::now() })
}

/// Waits until the window answers on its socket, which it does once it has a surface and a
/// GPU device. Returns the time that took.
fn wait_ready(sock: &Path, r: &mut Running) -> Option<Duration> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if r.child.try_wait().ok().flatten().is_some() {
            return None;
        }
        if control::send(sock, "state").is_ok() {
            return Some(r.started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// Asks the window to save its state and exit; stops it if it does not.
fn stop(sock: &Path, r: &mut Running) {
    let _ = control::send(sock, "quit");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if r.child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Only ever our own child.
    let _ = r.child.kill();
    let _ = r.child.wait();
}

fn run(args: &[String]) -> Result<u8, String> {
    let t = target(args)?;
    let root = workspace_root()?;
    let sock = socket();
    let state = sock.with_extension("state.json");
    let _ = std::fs::remove_file(&state);
    let started = Instant::now();
    let exe = build(&t)?;
    eprintln!("vo-preview: built {} in {:.2} s", t.example, started.elapsed().as_secs_f64());
    let mut win = Some(spawn(&exe, &t, &sock, &state, false)?);
    if let Some(d) = win.as_mut().and_then(|w| wait_ready(&sock, w)) {
        eprintln!("vo-preview: window up {:.2} s after start; control socket {}", d.as_secs_f64(), sock.display());
    }
    let mut seen = snapshot(&root);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(w) = win.as_mut() {
            if let Some(status) = w.child.try_wait().map_err(|e| e.to_string())? {
                if status.success() {
                    eprintln!("vo-preview: window closed");
                    return Ok(0);
                }
                eprintln!("vo-preview: the window stopped ({status}); fix and save to try again");
                win = None;
            }
        }
        let now = snapshot(&root);
        if now == seen {
            continue;
        }
        let changed: Vec<String> = now
            .iter()
            .filter(|f| !seen.contains(f))
            .filter_map(|(p, _)| p.strip_prefix(&root).ok().map(|p| p.display().to_string()))
            .collect();
        // Edits made during the build differ from this snapshot and trigger the next pass.
        seen = now;
        let edit = Instant::now();
        eprintln!("vo-preview: changed {}; rebuilding", changed.join(", "));
        match build(&t) {
            Ok(new_exe) => {
                let built = edit.elapsed();
                if let Some(mut w) = win.take() {
                    stop(&sock, &mut w);
                }
                let mut w = spawn(&new_exe, &t, &sock, &state, true)?;
                match wait_ready(&sock, &mut w) {
                    Some(_) => eprintln!(
                        "vo-preview: edit to window {:.2} s (build {:.2} s, restart {:.2} s), back at the same t",
                        edit.elapsed().as_secs_f64(),
                        built.as_secs_f64(),
                        (edit.elapsed() - built).as_secs_f64()
                    ),
                    None => eprintln!("vo-preview: the new build did not come up"),
                }
                win = Some(w);
            }
            Err(e) => eprintln!("vo-preview: {e}; the window keeps the last good build"),
        }
    }
}
