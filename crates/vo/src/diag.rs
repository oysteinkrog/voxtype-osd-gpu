//! The diagnostics hook: startup stages since exec, memory, and frames, so a settled overlay
//! can be checked to draw nothing. [`LogDiagnostics`] prints them to stderr.

use vo_host::HostDiag;

/// Called by `run_overlay` as things happen. Every method has an empty default.
pub trait Diagnostics {
    /// Once, when the first frame reached the screen (or was drawn, when the host has no
    /// present feedback). `host` holds the startup stages.
    fn startup(&mut self, host: &HostDiag) {
        let _ = host;
    }

    /// A frame was drawn. `n` counts from 0; `t` is the time it was sampled at. `host` gives
    /// the loop wakeup count.
    fn frame(&mut self, n: u64, t: f64, host: &HostDiag) {
        let _ = (n, t, host);
    }

    /// Nothing moves any more at `t`, so no frames until the next event.
    fn settled(&mut self, t: f64, host: &HostDiag) {
        let _ = (t, host);
    }

    /// Frame `n`, the first after a settle, drew the same display list as the frame before
    /// the settle, and the host did not ask for it (no resize, no GPU hand-over). A frame
    /// drawn while idle: it costs power and shows nothing new.
    fn idle_frame(&mut self, n: u64, t: f64) {
        let _ = (n, t);
    }

    /// The run ended.
    fn exit(&mut self, host: &HostDiag) {
        let _ = host;
    }
}

/// Bytes on the heap, from an app's counting global allocator.
pub type HeapFn = fn() -> usize;

/// Prints the hook's events to stderr, with the own-heap figure when `heap` is set and the
/// process's PSS from `/proc/self/smaps_rollup`.
#[derive(Default)]
pub struct LogDiagnostics {
    pub heap: Option<HeapFn>,
    /// Also print a line per frame.
    pub every_frame: bool,
    /// `(t, loop wakeups)` per frame.
    frames: Vec<(f64, u64)>,
    /// `(t, frames before, loop wakeups)` per settle.
    settles: Vec<(f64, usize, u64)>,
    /// Frames drawn while idle. Should stay 0.
    idle_frames: u64,
}

impl LogDiagnostics {
    pub fn new(heap: Option<HeapFn>) -> LogDiagnostics {
        LogDiagnostics { heap, every_frame: std::env::var_os("VO_DIAG_FRAMES").is_some(), ..Default::default() }
    }

    fn memory(&self) -> String {
        let heap = self.heap.map_or(String::new(), |h| format!("heap={:.2}MB ", h() as f64 / 1048576.0));
        format!(
            "{heap}pss={:.1}MB pss_anon={:.1}MB rss={:.1}MB",
            smaps_kb("Pss:") / 1024.0,
            smaps_kb("Pss_Anon:") / 1024.0,
            smaps_kb("Rss:") / 1024.0
        )
    }
}

impl Diagnostics for LogDiagnostics {
    fn startup(&mut self, host: &HostDiag) {
        let stages: Vec<String> =
            host.stages.iter().map(|(name, t)| format!("{name}={:.1}ms", (host.exec_to_origin + t) * 1000.0)).collect();
        eprintln!("vo: startup since exec: {}", stages.join(" "));
        for n in &host.notes {
            eprintln!("vo: {n}");
        }
        eprintln!("vo: memory at first frame: {}", self.memory());
    }

    fn frame(&mut self, n: u64, t: f64, host: &HostDiag) {
        self.frames.push((t, host.loop_wakeups));
        if self.every_frame {
            eprintln!("vo: frame {n} t={t:.4}");
        }
    }

    fn settled(&mut self, t: f64, host: &HostDiag) {
        self.settles.push((t, self.frames.len(), host.loop_wakeups));
        eprintln!("vo: settled t={t:.3} frames={} {}", self.frames.len(), self.memory());
    }

    fn idle_frame(&mut self, n: u64, t: f64) {
        self.idle_frames += 1;
        eprintln!("vo: frame {n} at t={t:.4} was drawn while idle: same display list as before the settle");
    }

    fn exit(&mut self, host: &HostDiag) {
        for &(t, before, woke) in &self.settles {
            let in_5s = self.frames[before..].iter().filter(|f| f.0 > t && f.0 <= t + 5.0).count();
            let (next, woke_next) = self.frames.get(before).copied().unwrap_or((f64::INFINITY, host.loop_wakeups));
            eprintln!(
                "vo: idle after settle at t={t:.3}: next frame at t={next:.3} (gap {:.2}s), frames in the 5s after \
                 settle: {in_5s}, loop wakeups until the next frame: {}",
                next - t,
                woke_next - woke
            );
        }
        eprintln!("vo: idle frames: {} (drawn after a settle and the same as before it)", self.idle_frames);
        eprintln!("vo: exit: frames={} loop_wakeups={} {}", self.frames.len(), host.loop_wakeups, self.memory());
    }
}

/// A field of `/proc/self/smaps_rollup` in kB, 0 when it cannot be read.
pub fn smaps_kb(key: &str) -> f64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        })
        .unwrap_or(0.0)
}
