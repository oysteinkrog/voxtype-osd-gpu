//! Per-letter motions (EG-16), each a closed form in `t` and the letter's arrival time, so a
//! seek, a replay or a render at another frame rate gives the same frames. They move one
//! glyph at a time through its transform (`GlyphXform`, EG-15).
//!
//! - Tumble (`LetterMotions::tumble`): a copy of each new letter, drawn as decoration (word
//!   id `NO_WORD`, so the reading-line envelope ignores it), flies in from up to
//!   [`TUMBLE_FROM`] px away turning up to [`TUMBLE_TURN`] degrees and lands on the real
//!   glyph within [`TUMBLE_SECS`]. The real glyph stands in place with the ink the text
//!   would give it anyway: the copy never dims it, so it is as readable as without the
//!   tumble from its first frame.
//! - Italic arrival (`LetterMotions::italic`): a new word arrives leaning up to
//!   [`ITALIC_SKEW`] degrees and straightens over [`ITALIC_SECS`].
//! - Wave (`LetterMotions::wave`): glyphs of the older lines, never the newest, ride
//!   `y = A sin(kx - wt)` with `A` at most [`WAVE_AMP`] px, turned to follow the slope. A
//!   line eases into the wave over [`WAVE_EASE_SECS`], [`WAVE_HOLD_SECS`] after it stops
//!   being the newest line.
//! - Lean ([`digit_lean`]): digits of a counter label lean forward with the words per
//!   minute, up to [`LEAN_MAX`] degrees, and spring back upright in a pause. For labels, not
//!   live text.
//!
//! Under reduced motion there is no tumbling copy, no lean, a straight arrival and still
//! lines.

use std::f64::consts::TAU;

use vo_core::hash::rand_f32;

use crate::geom::Point;

/// How long a tumbling copy flies, in seconds.
pub const TUMBLE_SECS: f64 = 0.35;
/// The farthest a tumbling copy starts from its letter, in px.
pub const TUMBLE_FROM: f32 = 40.0;
/// The most a tumbling copy turns on its way in, in degrees.
pub const TUMBLE_TURN: f32 = 360.0;
/// How far a new word leans as it arrives, in degrees.
pub const ITALIC_SKEW: f32 = 5.0;
/// How long a new word takes to straighten, in seconds.
pub const ITALIC_SECS: f64 = 0.2;
/// The wave's height, in px.
pub const WAVE_AMP: f32 = 3.0;
/// The wave's length along the line, in px.
pub const WAVE_LENGTH: f32 = 140.0;
/// Seconds for the wave to pass one wavelength.
pub const WAVE_PERIOD: f64 = 3.2;
/// How long after a newer line's first letter arrives a line waits before it eases into
/// the wave, in seconds: a look may hold the new line back (a drop hold, a line wait), and
/// until it shows the older line is still the one read.
pub const WAVE_HOLD_SECS: f64 = 0.4;
/// How long a line takes to join the wave once it starts, in seconds.
pub const WAVE_EASE_SECS: f64 = 0.8;
/// The most a digit leans, in degrees.
pub const LEAN_MAX: f32 = 12.0;
/// Words per minute at which digits lean all the way.
pub const LEAN_FULL_WPM: f32 = 200.0;
/// How quickly digits stand back up in a pause: the lean halves every this many seconds.
pub const LEAN_HALF_SECS: f64 = 0.25;

/// Which per-letter motions live text plays (`Element::letter_motion`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LetterMotions {
    pub tumble: bool,
    pub italic: bool,
    pub wave: bool,
}

impl LetterMotions {
    pub const NONE: LetterMotions = LetterMotions { tumble: false, italic: false, wave: false };

    pub fn any(&self) -> bool {
        self.tumble || self.italic || self.wave
    }
}

/// Where a tumbling copy is: its move from the real glyph, its turn and its opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TumblePose {
    pub offset: Point,
    pub rot_deg: f32,
    pub alpha: f32,
}

/// Eases out: fast at first, landing softly.
fn ease_out(u: f32) -> f32 {
    let v = 1.0 - u.clamp(0.0, 1.0);
    1.0 - v * v * v
}

/// The tumbling copy of a letter that arrived at `arrived`, at `t`; `seed` picks its
/// direction and turn. `None` before it arrives and from [`TUMBLE_SECS`] after.
pub fn tumble(arrived: f64, t: f64, seed: u32) -> Option<TumblePose> {
    let age = t - arrived;
    if !(0.0..TUMBLE_SECS).contains(&age) {
        return None;
    }
    let u = (age / TUMBLE_SECS) as f32;
    let left = 1.0 - ease_out(u);
    let angle = rand_f32(seed, 1, 0, 0) * std::f32::consts::TAU;
    let from = TUMBLE_FROM * (0.6 + 0.4 * rand_f32(seed, 2, 0, 0));
    let turn =
        TUMBLE_TURN * (0.5 + 0.5 * rand_f32(seed, 3, 0, 0)) * if rand_f32(seed, 4, 0, 0) < 0.5 { -1.0 } else { 1.0 };
    Some(TumblePose {
        offset: Point::new(angle.cos() * from * left, angle.sin() * from * left),
        rot_deg: turn * left,
        alpha: 0.85 * (1.0 - u * u),
    })
}

/// A new word's lean `t - arrived` seconds after it came, in degrees: [`ITALIC_SKEW`],
/// straightening over [`ITALIC_SECS`].
pub fn italic_skew(arrived: f64, t: f64) -> f32 {
    let u = ((t - arrived) / ITALIC_SECS).clamp(0.0, 1.0) as f32;
    ITALIC_SKEW * (1.0 - ease_out(u))
}

/// How far into the wave a line is at `t` when it stopped being the newest line at `next`: 0
/// up to [`WAVE_HOLD_SECS`] later, then easing to 1 over [`WAVE_EASE_SECS`].
pub fn wave_share(next: f64, t: f64) -> f32 {
    let u = ((t - next - WAVE_HOLD_SECS) / WAVE_EASE_SECS).clamp(0.0, 1.0) as f32;
    u * u * (3.0 - 2.0 * u)
}

/// A glyph of an older line at pen `x` at `t`: how far the wave moves it down, in px, and
/// the turn that follows the wave's slope, in degrees (clockwise on screen).
pub fn wave(x: f32, t: f64) -> (f32, f32) {
    let k = std::f64::consts::TAU / WAVE_LENGTH as f64;
    let phase = k * x as f64 - TAU / WAVE_PERIOD * t;
    let dy = WAVE_AMP as f64 * phase.sin();
    let slope = WAVE_AMP as f64 * k * phase.cos();
    (dy as f32, slope.atan().to_degrees() as f32)
}

/// How far a counter's digits lean forward, in degrees: up to [`LEAN_MAX`] with the words
/// per minute, standing back up in a pause of `pause` seconds. Upright under reduced motion.
pub fn digit_lean(wpm: f32, pause: f64, reduced: bool) -> f32 {
    if reduced {
        return 0.0;
    }
    let lean = LEAN_MAX * (wpm / LEAN_FULL_WPM).clamp(0.0, 1.0);
    lean * 0.5f32.powf((pause.max(0.0) / LEAN_HALF_SECS) as f32)
}
