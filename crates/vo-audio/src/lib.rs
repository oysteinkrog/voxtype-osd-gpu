//! Audio charts drawn from the signal bus (`vo_core::signals`), WP17.
//!
//! - `mel`: [`MelTexture`], a scrolling mel spectrogram drawn as a smooth texture through a
//!   colour map, with grain and dark seams between words (FX-114). It is a GPU element:
//!   add [`MelPainter`] to the renderer (`install`) and show it with `Element::gpu`.
//! - `mic`: [`mic_meter`], a mic health meter with a target zone, noise floor, peak hold and
//!   clip latch, and [`MicStats`] with the noise floor, speech level, SNR and a verdict
//!   (FX-118).
//!
//! Every chart reads a `SignalsCell` and is a pure function of the frame time, so live and
//! export draw the same frames.

pub mod mel;
pub mod mic;

pub use mel::{ColorMap, MelPainter, MelTexture};
pub use mic::{mic_meter, MicLook, MicStats, Verdict};

/// Adds this crate's painters to `renderer`. Call it once per renderer, for example from
/// `vo::Options::renderer_setup`.
pub fn install(renderer: &mut vo_render::Renderer) {
    renderer.add_painter(Box::new(MelPainter::default()));
}
