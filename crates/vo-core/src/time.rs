//! Time as an input.
//!
//! Only the host reads a clock. Everything below it gets the time passed in: events carry
//! their timestamp, and a frame is sampled at the time it will be shown. Times are seconds
//! as `f64` from an origin the host picks (the event log's start, or the export's frame 0).

/// The time a frame is sampled at, and its number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameTime {
    /// Seconds. For a live frame, the time it is expected to reach the screen.
    pub t: f64,
    /// Counts presented frames. Only for logs and frame-pacing stats: no motion may depend
    /// on it, because live and export frame numbers differ for the same `t`.
    pub frame: u64,
}

impl FrameTime {
    pub const fn new(t: f64, frame: u64) -> FrameTime {
        FrameTime { t, frame }
    }

    /// Frame `frame` of an export at `fps` frames per second. `frame / fps` with no running
    /// sum, so frame 3600 at 60 fps is exactly 60 s.
    pub fn at_rate(frame: u64, fps: f64) -> FrameTime {
        FrameTime { t: frame as f64 / fps, frame }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_frames_land_on_exact_times() {
        assert_eq!(FrameTime::at_rate(3600, 60.0).t, 60.0);
        assert_eq!(FrameTime::at_rate(24, 144.0).t, FrameTime::at_rate(5, 30.0).t);
        assert_eq!(FrameTime::at_rate(7, 30.0), FrameTime::new(7.0 / 30.0, 7));
    }
}
