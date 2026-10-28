//! The reading-line envelope (MO-08) holds under a displacement (EG-03): the displace demo's
//! app, words arriving with a ripple ring each, run through `check_scenario` on the CPU.
//! The displacement only moves pixels of a captured backdrop, never glyphs, and the reading
//! line is kept out of it; this checks the scene stays inside the envelope with it on.

#[path = "../examples/displace_demo.rs"]
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
fn displace_demo_keeps_the_reading_line_envelope() {
    for source in [demo::Source::Ripples, demo::Source::Noise, demo::Source::Curl, demo::Source::Page] {
        let r = check_scenario(
            demo::Demo::new(source),
            &demo::scenario(),
            60.0,
            demo::options(fonts()),
            &Envelope::default(),
        );
        r.assert_ok();
        assert!(r.glyphs_checked > 100, "{source:?}: {r}");
    }
}

/// Every frame with a word on screen carries a displacement whose kept box covers the newest
/// line.
#[test]
fn every_frame_keeps_the_reading_line() {
    let mut frames = 0;
    vo::envelope::sample_scenario(
        demo::Demo::new(demo::Source::Ripples),
        &demo::scenario(),
        30.0,
        demo::options(fonts()),
        &mut |_, t, list| {
            if t < 0.3 {
                return;
            }
            let d = list.layers.iter().find_map(|l| l.displace).expect("a displacement");
            assert!(d.keep.is_some(), "no kept box at {t}");
            frames += 1;
        },
    );
    assert!(frames > 60, "{frames}");
}
