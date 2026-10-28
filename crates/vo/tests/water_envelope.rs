//! The reading-line envelope (MO-08) holds over a water pool (EG-10): the water pool demo's
//! app, a drop per arriving word, run through `check_scenario` on the CPU. The pool only reads
//! what is drawn above it, so the glyphs of the newest line never move.

#[path = "../examples/water_pool_demo.rs"]
#[allow(dead_code)]
mod demo;

use vo::envelope::{check_scenario, Envelope};
use vo::scene::font::{test_font_path, FontRegistry};

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

#[test]
fn water_pool_demo_keeps_the_reading_line_envelope() {
    let r = check_scenario(demo::Demo::new(), &demo::scenario(), 60.0, demo::options(fonts()), &Envelope::default());
    r.assert_ok();
    assert!(r.glyphs_checked > 100, "{r}");
}

/// Every frame from the first word on draws the pool with its material, below the text.
#[test]
fn every_frame_has_the_pool_below_the_text() {
    let mut frames = 0;
    vo::envelope::sample_scenario(
        demo::Demo::new(),
        &demo::scenario(),
        30.0,
        demo::options(fonts()),
        &mut |_, t, list| {
            if t < 0.3 {
                return;
            }
            assert!(list.gpu.len() == 1, "no water drive at {t}");
            assert!(list.layers.iter().any(|l| l.backdrop.is_some()), "no backdrop at {t}");
            frames += 1;
        },
    );
    assert!(frames > 100, "{frames} frames");
}
