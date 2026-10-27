//! The signal bus: audio features as a timestamped input that any prop, shader or emitter can
//! read (SIG01).
//!
//! The daemon sends one [`SignalFrame`] every 10 ms: level, peak, pitch, voicing, spectral
//! centroid and flatness, an onset strength and 24 mel bands. [`Signals`] keeps the last few
//! seconds and answers "what was the value at `t`" as a pure function, so a scene sampled at
//! `t` gives the same frame live and in export.
//!
//! Rules the store follows:
//! - Frames are placed by the daemon's audio clock, not by when they arrived, so socket jitter
//!   never moves the picture. The offset between the two clocks is the smallest one seen, and
//!   it may grow by 1 ms per second to follow clock drift.
//! - Reads happen `playout` seconds (50 ms by default) behind `t`, so the frame for `t` has
//!   almost always arrived. A gap of more than 30 ms between frames holds the last value for
//!   30 ms, then reads as rest (silence).
//! - Values come out normalised to 0..1 (see [`Sig`]). [`Sig::Raw`] gives the physical value.
//! - Pitch is gated by voicing: it holds the last voiced value and never interpolates across
//!   a voiced edge.
//! - [`Signals::envelope`] smooths with separate attack and release, stepped on a fixed
//!   100 Hz grid of absolute time, so the result does not depend on the frame rate.
//! - [`Signals::presence`] fades from 1 to 0 when no frame has arrived for 300 to 600 ms.
//!   A [`Bind`] composes towards neutral by it, so a stalled stream eases out.
//!
//! A [`Bind`] maps a signal onto a prop: input range, curve, output range, attack and release,
//! and how it combines with the prop's own spring value ([`Compose`]). The spring stays the
//! base; the signal multiplies, adds or mixes on top.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// The number of mel bands in a frame.
pub const MEL_BANDS: usize = 24;
/// Seconds between frames.
pub const FRAME_SECS: f64 = 0.01;
/// The default read delay behind `t`.
pub const DEFAULT_PLAYOUT: f64 = 0.05;
/// Seconds of history the live store keeps.
pub const DEFAULT_CAP_SECS: f64 = 4.0;
/// A gap between frames longer than this reads as rest after holding the last value this long.
pub const GAP_SECS: f64 = 0.03;
/// Presence is 1 up to this many seconds after the newest frame...
pub const STALE_SECS: f64 = 0.3;
/// ...and 0 from this many.
pub const GONE_SECS: f64 = 0.6;
/// A frame whose onset strength reaches this counts as an onset.
pub const ONSET_THRESHOLD: f32 = 0.5;
/// The onset age reported when no onset has happened.
pub const NO_ONSET: f32 = 1000.0;

/// Level normalisation: dBFS from this floor up to 0.
const LEVEL_FLOOR_DB: f32 = -60.0;
/// Pitch normalisation: log scale over this range in Hz.
const PITCH_LO: f32 = 60.0;
const PITCH_HI: f32 = 1000.0;
/// Centroid normalisation: log scale over this range in Hz.
const CENTROID_LO: f32 = 100.0;
const CENTROID_HI: f32 = 8000.0;

/// One 10 ms frame of audio features, in physical units except the mel bands.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalFrame {
    /// The daemon's audio sample clock in seconds (sample index over sample rate).
    pub clock: f64,
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    /// Fundamental frequency in Hz; only meaningful when `voiced`.
    pub f0_hz: f32,
    pub voiced: bool,
    pub centroid_hz: f32,
    /// Spectral flatness, 0 (tonal) to 1 (noise).
    pub flatness: f32,
    /// Onset strength, 0 to 1.
    pub onset: f32,
    /// Mel band energies, 0 (-100 dB) to 1 (0 dB).
    pub mel: [f32; MEL_BANDS],
}

impl SignalFrame {
    /// A silent frame at `clock`.
    pub fn silent(clock: f64) -> SignalFrame {
        SignalFrame {
            clock,
            rms_dbfs: -100.0,
            peak_dbfs: -100.0,
            f0_hz: 0.0,
            voiced: false,
            centroid_hz: 0.0,
            flatness: 0.0,
            onset: 0.0,
            mel: [0.0; MEL_BANDS],
        }
    }
}

/// A physical value, for [`Sig::Raw`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Field {
    RmsDb,
    PeakDb,
    F0Hz,
    CentroidHz,
}

/// What a read returns. Everything except `Raw` is normalised to 0..1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Sig {
    /// RMS level, dBFS from -60 to 0.
    Level,
    /// Peak level, dBFS from -60 to 0.
    Peak,
    /// Pitch on a log scale from 60 to 1000 Hz, holding the last voiced value.
    Pitch,
    /// 1 when voiced, else 0.
    Voiced,
    /// Spectral centroid on a log scale from 100 to 8000 Hz.
    Centroid,
    Flatness,
    /// Onset strength of the frame.
    Onset,
    /// `exp(-age / tau)` where age is the seconds since the last onset: 1 at an onset,
    /// decaying after. Not smoothed further by an envelope's attack.
    OnsetPulse {
        tau: f32,
    },
    /// One mel band, 0 lowest.
    Band(u8),
    /// The mean of mel bands `lo..=hi`.
    Bands(u8, u8),
    /// The value in its own unit, with rest values of -100 dB and 0 Hz.
    Raw(Field),
}

impl Sig {
    fn read(self, f: &SignalFrame) -> f32 {
        match self {
            Sig::Level => level(f.rms_dbfs),
            Sig::Peak => level(f.peak_dbfs),
            Sig::Pitch => log_norm(f.f0_hz, PITCH_LO, PITCH_HI),
            Sig::Voiced => f.voiced as u8 as f32,
            Sig::Centroid => log_norm(f.centroid_hz, CENTROID_LO, CENTROID_HI),
            Sig::Flatness => f.flatness.clamp(0.0, 1.0),
            Sig::Onset => f.onset.clamp(0.0, 1.0),
            Sig::OnsetPulse { .. } => 0.0,
            Sig::Band(i) => f.mel[(i as usize).min(MEL_BANDS - 1)],
            Sig::Bands(lo, hi) => {
                let lo = (lo as usize).min(MEL_BANDS - 1);
                let hi = (hi as usize).clamp(lo, MEL_BANDS - 1);
                f.mel[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32
            }
            Sig::Raw(Field::RmsDb) => f.rms_dbfs,
            Sig::Raw(Field::PeakDb) => f.peak_dbfs,
            Sig::Raw(Field::F0Hz) => f.f0_hz,
            Sig::Raw(Field::CentroidHz) => f.centroid_hz,
        }
    }

    /// The value when nothing is sounding.
    fn rest(self) -> f32 {
        match self {
            Sig::Raw(Field::RmsDb) | Sig::Raw(Field::PeakDb) => -100.0,
            _ => 0.0,
        }
    }
}

fn level(db: f32) -> f32 {
    ((db - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0)
}

fn log_norm(hz: f32, lo: f32, hi: f32) -> f32 {
    if hz <= 0.0 {
        return 0.0;
    }
    ((hz / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Placed {
    /// Scene time of the frame.
    at: f64,
    frame: SignalFrame,
}

/// The audio feature history, as scene-time samples. Cheap to read; the host keeps it in an
/// `Arc` and hands snapshots to the view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    frames: VecDeque<Placed>,
    playout: f64,
    cap: f64,
    /// Scene time minus audio clock, once the first frame arrived.
    offset: Option<f64>,
    last_arrival: f64,
}

impl Default for Signals {
    fn default() -> Signals {
        Signals::new()
    }
}

impl Signals {
    /// An empty live store: 50 ms playout, 4 s of history.
    pub fn new() -> Signals {
        Signals {
            frames: VecDeque::new(),
            playout: DEFAULT_PLAYOUT,
            cap: DEFAULT_CAP_SECS,
            offset: None,
            last_arrival: 0.0,
        }
    }

    /// A recording for replay or export: frame `i` sits at `start + frames[i].clock -
    /// frames[0].clock`, with no history cap.
    pub fn recorded(frames: impl IntoIterator<Item = SignalFrame>, start: f64) -> Signals {
        let mut s = Signals { cap: f64::MAX, ..Signals::new() };
        for frame in frames {
            let offset = *s.offset.get_or_insert(start - frame.clock);
            s.insert(Placed { at: frame.clock + offset, frame });
        }
        s
    }

    pub fn with_playout(mut self, playout: f64) -> Signals {
        self.playout = playout.max(0.0);
        self
    }

    pub fn with_cap(mut self, secs: f64) -> Signals {
        self.cap = secs.max(GONE_SECS);
        self
    }

    pub fn playout(&self) -> f64 {
        self.playout
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// The scene time of the newest frame.
    pub fn newest_at(&self) -> Option<f64> {
        self.frames.back().map(|p| p.at)
    }

    /// The frames with their scene times, oldest first. For recording.
    pub fn frames(&self) -> impl Iterator<Item = (f64, &SignalFrame)> + '_ {
        self.frames.iter().map(|p| (p.at, &p.frame))
    }

    /// A frame that arrived at scene time `arrived`. Frames that land at or before the newest
    /// one are dropped. A clock that runs backwards or jumps by more than a second means the
    /// daemon restarted: the history starts over.
    pub fn push(&mut self, frame: SignalFrame, arrived: f64) {
        let seen = arrived - frame.clock;
        let offset = match (self.offset, self.frames.back()) {
            (Some(offset), Some(last)) if frame.clock > last.frame.clock && frame.clock - last.frame.clock < 1.0 => {
                let drift = 0.001 * (arrived - self.last_arrival).max(0.0);
                seen.min(offset + drift)
            }
            _ => {
                self.frames.clear();
                seen
            }
        };
        self.offset = Some(offset);
        self.last_arrival = arrived;
        self.insert(Placed { at: frame.clock + offset, frame });
    }

    fn insert(&mut self, p: Placed) {
        if self.frames.back().is_some_and(|last| p.at <= last.at) {
            return;
        }
        self.frames.push_back(p);
        while self.frames.front().is_some_and(|f| f.at < p.at - self.cap) {
            self.frames.pop_front();
        }
    }

    /// The read time for scene time `t`.
    fn read_at(&self, t: f64) -> f64 {
        t - self.playout
    }

    /// The index of the last frame at or before `s`.
    fn index(&self, s: f64) -> Option<usize> {
        let n = self.frames.partition_point(|p| p.at <= s);
        n.checked_sub(1)
    }

    /// The value of `sig` at scene time `t`, unsmoothed.
    pub fn sample(&self, sig: Sig, t: f64) -> f32 {
        self.sample_read(sig, self.read_at(t))
    }

    fn sample_read(&self, sig: Sig, s: f64) -> f32 {
        if let Sig::OnsetPulse { tau } = sig {
            let age = self.onset_age_read(s);
            return if tau > 0.0 { (-age / tau).exp() } else { (age <= 0.0) as u8 as f32 };
        }
        let Some(i) = self.index(s) else { return sig.rest() };
        if sig == Sig::Pitch {
            return self.pitch_read(i, s);
        }
        let a = &self.frames[i];
        match self.frames.get(i + 1) {
            Some(b) if b.at - a.at <= GAP_SECS => {
                let k = ((s - a.at) / (b.at - a.at)) as f32;
                let (x, y) = (sig.read(&a.frame), sig.read(&b.frame));
                x + (y - x) * k
            }
            _ if s - a.at <= GAP_SECS => sig.read(&a.frame),
            _ => sig.rest(),
        }
    }

    /// Pitch at read time `s`, frame `i` being the last at or before it.
    fn pitch_read(&self, i: usize, s: f64) -> f32 {
        let a = &self.frames[i];
        if let Some(b) = self.frames.get(i + 1) {
            if a.frame.voiced && b.frame.voiced && b.at - a.at <= GAP_SECS {
                let k = ((s - a.at) / (b.at - a.at)) as f32;
                return Sig::Pitch.read(&a.frame) + (Sig::Pitch.read(&b.frame) - Sig::Pitch.read(&a.frame)) * k;
            }
        }
        self.frames.range(..=i).rev().find(|p| p.frame.voiced).map_or(0.0, |p| Sig::Pitch.read(&p.frame))
    }

    /// `sig` smoothed with `attack` and `release` time constants in seconds: a one-pole filter
    /// stepped on the absolute 100 Hz grid from 6 time constants back.
    pub fn envelope(&self, sig: Sig, attack: f32, release: f32, t: f64) -> f32 {
        let s = self.read_at(t);
        if attack <= 0.0 && release <= 0.0 || self.frames.is_empty() {
            return self.sample_read(sig, s);
        }
        let span = 6.0 * attack.max(release) as f64;
        let first = ((s - span) / FRAME_SECS).floor() as i64;
        let last = (s / FRAME_SECS).floor() as i64;
        let mut y = self.sample_read(sig, first as f64 * FRAME_SECS);
        let step = |y: f32, x: f32, dt: f64| {
            let tc = if x > y { attack } else { release };
            if tc <= 0.0 {
                x
            } else {
                y + (x - y) * (1.0 - (-dt as f32 / tc).exp())
            }
        };
        for k in first + 1..=last {
            y = step(y, self.sample_read(sig, k as f64 * FRAME_SECS), FRAME_SECS);
        }
        let rest = s - last as f64 * FRAME_SECS;
        if rest > 0.0 {
            y = step(y, self.sample_read(sig, s), rest);
        }
        y
    }

    /// 1 while frames are arriving, fading to 0 between 300 and 600 ms after the newest one.
    pub fn presence(&self, t: f64) -> f32 {
        let Some(newest) = self.newest_at() else { return 0.0 };
        let s = self.read_at(t);
        if s < self.frames[0].at {
            return 0.0;
        }
        let age = s - newest;
        (1.0 - (age - STALE_SECS) / (GONE_SECS - STALE_SECS)).clamp(0.0, 1.0) as f32
    }

    /// Seconds since the newest frame, at the read time for `t`. Negative while that frame is
    /// still ahead of the read time.
    pub fn signal_age(&self, t: f64) -> f32 {
        self.newest_at().map_or(NO_ONSET, |n| (self.read_at(t) - n) as f32)
    }

    /// Seconds since the last onset at or before the read time for `t`, or [`NO_ONSET`].
    pub fn onset_age(&self, t: f64) -> f32 {
        self.onset_age_read(self.read_at(t))
    }

    fn onset_age_read(&self, s: f64) -> f32 {
        let Some(i) = self.index(s) else { return NO_ONSET };
        self.frames
            .range(..=i)
            .rev()
            .take_while(|p| s - p.at < NO_ONSET as f64)
            .find(|p| p.frame.onset >= ONSET_THRESHOLD)
            .map_or(NO_ONSET, |p| (s - p.at) as f32)
    }

    /// The scene time after which nothing that reads this store with smoothing `tail` seconds
    /// changes. A scene stays awake until then.
    pub fn active_until(&self, tail: f64) -> Option<f64> {
        self.newest_at().map(|n| n + self.playout + GAP_SECS + tail.max(GONE_SECS))
    }

    /// The uniform block for the shaders at scene time `t`.
    pub fn block(&self, t: f64) -> SignalBlock {
        let s = self.read_at(t);
        let head = [
            t as f32,
            self.presence(t),
            self.signal_age(t),
            self.onset_age(t),
            self.sample_read(Sig::Level, s),
            self.sample_read(Sig::Peak, s),
            self.sample_read(Sig::Pitch, s),
            self.sample_read(Sig::Voiced, s),
            self.sample_read(Sig::Centroid, s),
            self.sample_read(Sig::Flatness, s),
            self.sample_read(Sig::OnsetPulse { tau: 0.15 }, s),
            self.envelope(Sig::Level, 0.01, 0.15, t),
            0.0,
            0.0,
            0.0,
            0.0,
        ];
        let mut block = SignalBlock { head, ..SignalBlock::default() };
        for (b, v) in block.bands.iter_mut().enumerate() {
            *v = self.envelope(Sig::Band(b as u8), 0.0, 0.08, t);
        }
        let newest = (s / FRAME_SECS).floor() as i64;
        for (i, v) in block.ring.iter_mut().enumerate() {
            *v = self.sample_read(Sig::Level, (newest - i as i64) as f64 * FRAME_SECS);
        }
        block
    }
}

/// The live store, shared between the host that pushes frames and the parts of a scene that
/// read them (dot sources, emitters, canvases). Each read takes a snapshot, so a frame being
/// sampled never sees a half-pushed store.
#[derive(Clone, Debug, Default)]
pub struct SignalsCell(Arc<RwLock<Arc<Signals>>>);

/// Two cells are equal when they share one store.
impl PartialEq for SignalsCell {
    fn eq(&self, other: &SignalsCell) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl SignalsCell {
    pub fn new(signals: Signals) -> SignalsCell {
        SignalsCell(Arc::new(RwLock::new(Arc::new(signals))))
    }

    /// The current snapshot.
    pub fn get(&self) -> Arc<Signals> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replaces the store, for a recording.
    pub fn set(&self, signals: Arc<Signals>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = signals;
    }

    /// Adds a frame that arrived at scene time `arrived` (see [`Signals::push`]) and returns
    /// the new snapshot.
    pub fn push(&self, frame: SignalFrame, arrived: f64) -> Arc<Signals> {
        let mut guard = self.0.write().unwrap_or_else(|e| e.into_inner());
        Arc::make_mut(&mut guard).push(frame, arrived);
        guard.clone()
    }
}

/// History entries in [`SignalBlock::ring`].
pub const SIGNAL_RING: usize = 64;

/// What every shader can read, uploaded once per frame. Layout matches [`SIGNALS_WGSL`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SignalBlock {
    /// t, presence, signal age, onset age; level, peak, pitch, voiced; centroid, flatness,
    /// onset pulse (tau 150 ms), smoothed level (10 ms attack, 150 ms release); 4 spare.
    pub head: [f32; 16],
    /// Mel bands with an 80 ms release.
    pub bands: [f32; MEL_BANDS],
    /// Level on the 100 Hz grid, newest first.
    pub ring: [f32; SIGNAL_RING],
}

impl Default for SignalBlock {
    fn default() -> SignalBlock {
        let mut head = [0.0; 16];
        head[2] = NO_ONSET;
        head[3] = NO_ONSET;
        SignalBlock { head, bands: [0.0; MEL_BANDS], ring: [0.0; SIGNAL_RING] }
    }
}

/// Floats in a [`SignalBlock`] upload.
pub const SIGNAL_BLOCK_FLOATS: usize = 16 + MEL_BANDS + SIGNAL_RING;

impl SignalBlock {
    pub fn as_floats(&self) -> [f32; SIGNAL_BLOCK_FLOATS] {
        let mut out = [0.0; SIGNAL_BLOCK_FLOATS];
        out[..16].copy_from_slice(&self.head);
        out[16..16 + MEL_BANDS].copy_from_slice(&self.bands);
        out[16 + MEL_BANDS..].copy_from_slice(&self.ring);
        out
    }
}

/// The WGSL declaration of [`SignalBlock`] and its readers. The renderer binds it as
/// `@group(2) @binding(0) var<uniform> signals: Signals;`.
pub const SIGNALS_WGSL: &str = r#"
struct Signals {
    head: array<vec4<f32>, 4>,
    bands: array<vec4<f32>, 6>,
    ring: array<vec4<f32>, 16>,
};
fn sig_presence() -> f32 { return signals.head[0].y; }
fn sig_age() -> f32 { return signals.head[0].z; }
fn sig_onset_age() -> f32 { return signals.head[0].w; }
fn sig_level() -> f32 { return signals.head[1].x; }
fn sig_peak() -> f32 { return signals.head[1].y; }
fn sig_pitch() -> f32 { return signals.head[1].z; }
fn sig_voiced() -> f32 { return signals.head[1].w; }
fn sig_centroid() -> f32 { return signals.head[2].x; }
fn sig_flatness() -> f32 { return signals.head[2].y; }
fn sig_onset() -> f32 { return signals.head[2].z; }
fn sig_level_smooth() -> f32 { return signals.head[2].w; }
fn sig_band(i: u32) -> f32 { let b = min(i, 23u); return signals.bands[b / 4u][b % 4u]; }
fn sig_ring(i: u32) -> f32 { let r = min(i, 63u); return signals.ring[r / 4u][r % 4u]; }
"#;

/// How a bound signal combines with the prop's own (spring) value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compose {
    /// `base * v`: scale, opacity.
    Mul,
    /// `base + v`: offsets, glow radius.
    Add,
    /// The signal replaces the base: `v`. Colours mix towards a target by `v`.
    Mix,
}

/// A signal bound to a prop: `v = output.lerp(curve(input.unlerp(envelope(sig))))`, composed
/// with the base value and faded to neutral by [`Signals::presence`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bind {
    pub sig: Sig,
    pub input: (f32, f32),
    pub output: (f32, f32),
    /// Exponent on the normalised input: above 1 favours loud parts, below 1 quiet ones.
    pub curve: f32,
    pub attack: f32,
    pub release: f32,
    pub op: Compose,
}

impl Bind {
    /// `sig` over 0..1 to 0..1, 10 ms attack, 150 ms release, multiplied onto the base.
    pub fn new(sig: Sig) -> Bind {
        Bind { sig, input: (0.0, 1.0), output: (0.0, 1.0), curve: 1.0, attack: 0.01, release: 0.15, op: Compose::Mul }
    }

    pub fn input(mut self, lo: f32, hi: f32) -> Bind {
        self.input = (lo, hi);
        self
    }

    pub fn output(mut self, lo: f32, hi: f32) -> Bind {
        self.output = (lo, hi);
        self
    }

    pub fn curve(mut self, exponent: f32) -> Bind {
        self.curve = exponent.max(0.01);
        self
    }

    pub fn smooth(mut self, attack: f32, release: f32) -> Bind {
        self.attack = attack.max(0.0);
        self.release = release.max(0.0);
        self
    }

    pub fn mul(mut self) -> Bind {
        self.op = Compose::Mul;
        self
    }

    pub fn add(mut self) -> Bind {
        self.op = Compose::Add;
        self
    }

    pub fn mix(mut self) -> Bind {
        self.op = Compose::Mix;
        self
    }

    /// The mapped value at `t`, before composing.
    pub fn value(&self, signals: &Signals, t: f64) -> f32 {
        let x = match self.sig {
            Sig::OnsetPulse { .. } => signals.sample(self.sig, t),
            sig => signals.envelope(sig, self.attack, self.release, t),
        };
        let (lo, hi) = self.input;
        let n = if hi == lo { (x >= hi) as u8 as f32 } else { ((x - lo) / (hi - lo)).clamp(0.0, 1.0) };
        let n = n.powf(self.curve);
        self.output.0 + (self.output.1 - self.output.0) * n
    }

    /// `base` with the signal composed on, eased to `base` as the stream goes stale.
    pub fn apply(&self, base: f64, signals: &Signals, t: f64) -> f64 {
        let p = signals.presence(t) as f64;
        if p <= 0.0 {
            return base;
        }
        let v = self.value(signals, t) as f64;
        match self.op {
            Compose::Mul => base * (1.0 + (v - 1.0) * p),
            Compose::Add => base + v * p,
            Compose::Mix => base + (v - base) * p,
        }
    }

    /// The mix factor for a colour bound with [`Compose::Mix`]: the value times presence.
    pub fn mix_factor(&self, signals: &Signals, t: f64) -> f32 {
        (self.value(signals, t) * signals.presence(t)).clamp(0.0, 1.0)
    }

    /// Seconds this binding keeps moving after the input stops.
    pub fn tail(&self) -> f64 {
        let tau = match self.sig {
            Sig::OnsetPulse { tau } => tau,
            _ => self.release,
        };
        6.0 * tau as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(clock: f64, db: f32) -> SignalFrame {
        SignalFrame { rms_dbfs: db, peak_dbfs: db, ..SignalFrame::silent(clock) }
    }

    /// `n` frames from clock 0, level from `db(i)`.
    fn recorded(n: usize, db: impl Fn(usize) -> f32) -> Signals {
        Signals::recorded((0..n).map(|i| frame(i as f64 * FRAME_SECS, db(i))), 0.0)
    }

    #[test]
    fn normalises_level_pitch_and_centroid() {
        let f = SignalFrame { rms_dbfs: -30.0, f0_hz: 245.0, centroid_hz: 894.4, ..SignalFrame::silent(0.0) };
        assert!((Sig::Level.read(&f) - 0.5).abs() < 1e-6);
        assert!((Sig::Pitch.read(&f) - 0.5).abs() < 0.01);
        assert!((Sig::Centroid.read(&f) - 0.5).abs() < 0.01);
        assert_eq!(Sig::Level.read(&frame(0.0, -80.0)), 0.0);
        assert_eq!(Sig::Raw(Field::RmsDb).read(&f), -30.0);
    }

    #[test]
    fn samples_with_playout_and_interpolates() {
        let s = recorded(10, |i| -60.0 + 6.0 * i as f32);
        // Read time 0.025 sits halfway between frames 2 and 3.
        let v = s.sample(Sig::Level, 0.075);
        assert!((v - 0.25).abs() < 1e-5, "{v}");
        // Before the first frame: rest.
        assert_eq!(s.sample(Sig::Level, 0.0), 0.0);
    }

    #[test]
    fn a_gap_holds_then_rests() {
        let s = Signals::recorded([frame(0.0, 0.0), frame(0.2, 0.0)], 0.0).with_playout(0.0);
        assert_eq!(s.sample(Sig::Level, 0.02), 1.0);
        assert_eq!(s.sample(Sig::Level, 0.1), 0.0);
        assert_eq!(s.sample(Sig::Level, 0.2), 1.0);
        // After the newest frame too.
        assert_eq!(s.sample(Sig::Level, 0.21), 1.0);
        assert_eq!(s.sample(Sig::Level, 0.3), 0.0);
    }

    #[test]
    fn pitch_holds_the_last_voiced_value() {
        let f = |c: f64, hz: f32, voiced: bool| SignalFrame { f0_hz: hz, voiced, ..SignalFrame::silent(c) };
        let s = Signals::recorded(
            [f(0.0, 200.0, true), f(0.01, 400.0, true), f(0.02, 0.0, false), f(0.03, 100.0, true)],
            0.0,
        )
        .with_playout(0.0);
        let p = |hz: f32| log_norm(hz, PITCH_LO, PITCH_HI);
        assert!((s.sample(Sig::Pitch, 0.005) - (p(200.0) + p(400.0)) / 2.0).abs() < 1e-5);
        // Across the voiced edge: hold 400 Hz, no slide towards 0 or 100 Hz.
        assert_eq!(s.sample(Sig::Pitch, 0.015), p(400.0));
        assert_eq!(s.sample(Sig::Pitch, 0.025), p(400.0));
        assert_eq!(s.sample(Sig::Pitch, 0.03), p(100.0));
    }

    #[test]
    fn envelope_attacks_fast_and_releases_slow() {
        // 0.5 s of full level, then silence frames.
        let s = recorded(100, |i| if i < 50 { 0.0 } else { -100.0 }).with_playout(0.0);
        let up = s.envelope(Sig::Level, 0.01, 0.2, 0.05);
        assert!(up > 0.98, "{up}");
        let down = s.envelope(Sig::Level, 0.01, 0.2, 0.69);
        assert!((down - (-0.2f32 / 0.2).exp()).abs() < 0.03, "{down}");
    }

    #[test]
    fn envelope_does_not_depend_on_frame_rate() {
        let s = recorded(200, |i| -60.0 + 60.0 * ((i as f32 * 0.37).sin() * 0.5 + 0.5));
        // The same t read alone or after many other reads gives the same value.
        let a = s.envelope(Sig::Level, 0.02, 0.3, 1.2345);
        for k in 0..50 {
            s.envelope(Sig::Level, 0.02, 0.3, k as f64 * 0.016);
        }
        assert_eq!(a, s.envelope(Sig::Level, 0.02, 0.3, 1.2345));
    }

    #[test]
    fn presence_fades_after_the_stream_stops() {
        let s = recorded(10, |_| 0.0).with_playout(0.0);
        assert_eq!(s.presence(0.05), 1.0);
        assert_eq!(s.presence(0.09 + 0.3), 1.0);
        assert!((s.presence(0.09 + 0.45) - 0.5).abs() < 1e-4);
        assert_eq!(s.presence(0.09 + 0.6), 0.0);
        assert_eq!(Signals::new().presence(1.0), 0.0);
    }

    #[test]
    fn onset_age_and_pulse() {
        let mut frames: Vec<_> = (0..20).map(|i| frame(i as f64 * FRAME_SECS, -20.0)).collect();
        frames[5].onset = 0.9;
        let s = Signals::recorded(frames, 0.0).with_playout(0.0);
        assert_eq!(s.onset_age(0.04), NO_ONSET);
        assert!((s.onset_age(0.15) - 0.1).abs() < 1e-5);
        let pulse = s.sample(Sig::OnsetPulse { tau: 0.1 }, 0.15);
        assert!((pulse - (-1.0f32).exp()).abs() < 1e-4);
    }

    #[test]
    fn push_places_by_audio_clock_not_arrival() {
        let mut s = Signals::new().with_playout(0.0);
        // Frames every 10 ms of audio clock, arriving with up to 20 ms of jitter.
        let jitter = [0.0, 0.02, 0.005, 0.015, 0.0, 0.01];
        for (i, j) in jitter.iter().enumerate() {
            s.push(frame(5.0 + i as f64 * FRAME_SECS, -20.0), 1.0 + i as f64 * FRAME_SECS + j);
        }
        let at: Vec<f64> = s.frames().map(|(at, _)| at).collect();
        // Spacing stays on the audio clock, give or take the 1 ms per second drift allowance.
        for w in at.windows(2) {
            assert!((w[1] - w[0] - FRAME_SECS).abs() < 1e-4, "{at:?}");
        }
        assert!((at[0] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn push_restarts_on_a_clock_reset_and_caps_history() {
        let mut s = Signals::new().with_cap(1.0);
        for i in 0..300 {
            s.push(frame(i as f64 * FRAME_SECS, -20.0), 10.0 + i as f64 * FRAME_SECS);
        }
        assert!(s.len() <= 101, "{}", s.len());
        s.push(frame(0.0, -20.0), 20.0);
        assert_eq!(s.len(), 1);
        assert_eq!(s.newest_at(), Some(20.0));
    }

    #[test]
    fn bind_maps_composes_and_fades() {
        let s = recorded(100, |_| -30.0).with_playout(0.0);
        let b = Bind::new(Sig::Level).input(0.0, 1.0).output(1.0, 2.0).smooth(0.0, 0.0);
        assert!((b.value(&s, 0.5) - 1.5).abs() < 1e-5);
        assert!((b.apply(10.0, &s, 0.5) - 15.0).abs() < 1e-4);
        assert!((b.add().apply(10.0, &s, 0.5) - 11.5).abs() < 1e-4);
        assert!((b.mix().apply(10.0, &s, 0.5) - 1.5).abs() < 1e-4);
        // Long after the stream stopped: the base, untouched.
        assert_eq!(b.apply(10.0, &s, 5.0), 10.0);
        assert!((b.curve(2.0).output(0.0, 1.0).value(&s, 0.5) - 0.25).abs() < 1e-5);
    }

    #[test]
    fn block_layout_and_ring() {
        let s = recorded(100, |i| -60.0 + 0.6 * i as f32).with_playout(0.0);
        let b = s.block(0.505);
        assert_eq!(b.head[0], 0.505);
        assert_eq!(b.head[1], 1.0);
        assert!((b.ring[0] - Sig::Level.read(&frame(0.0, -60.0 + 0.6 * 50.0))).abs() < 1e-5);
        assert!(b.ring[0] > b.ring[10]);
        let f = b.as_floats();
        assert_eq!(f.len(), SIGNAL_BLOCK_FLOATS);
        assert_eq!(f[16 + MEL_BANDS], b.ring[0]);
        assert_eq!(SignalBlock::default().head[3], NO_ONSET);
    }

    #[test]
    fn cell_snapshots_do_not_change_under_a_reader() {
        let cell = SignalsCell::default();
        let before = cell.get();
        let after = cell.push(frame(0.0, -20.0), 1.0);
        assert!(before.is_empty());
        assert_eq!(after.len(), 1);
        assert_eq!(cell.get().len(), 1);
    }

    #[test]
    fn recording_round_trips_through_json() {
        let s = recorded(5, |i| -10.0 * i as f32);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Signals>(&json).unwrap(), s);
    }
}
