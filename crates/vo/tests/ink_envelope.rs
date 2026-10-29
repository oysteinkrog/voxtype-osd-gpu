//! The reading-line envelope (MO-08) holds over the ink layer (EG-12): the ink demo's app, six
//! dictated words seeding the fluid, run through `check_scenario` on the CPU in each style. The
//! dye is drawn behind the text and the smear keeps the newest line, so its glyphs never move.

#[path = "../examples/ink_demo.rs"]
#[allow(dead_code)]
mod demo;

use vo::envelope::{check_scenario, Envelope};
use vo::scene::fluid::InkStyle;

#[test]
fn ink_demo_keeps_the_reading_line_envelope() {
    for style in [InkStyle::Ink, InkStyle::Smoke, InkStyle::Watercolour] {
        let r = check_scenario(
            demo::Demo::new(style),
            &demo::scenario(),
            60.0,
            demo::options(demo::fonts(), style),
            &Envelope::default(),
        );
        r.assert_ok();
        assert!(r.glyphs_checked > 100, "{style:?}: {r}");
    }
}

/// Every frame from the first word on drives the fluid, and the older line has a smear once
/// the second line starts.
#[test]
fn every_frame_drives_the_fluid() {
    let mut frames = 0;
    vo::envelope::sample_scenario(
        demo::Demo::new(InkStyle::Ink),
        &demo::scenario(),
        30.0,
        demo::options(demo::fonts(), InkStyle::Ink),
        &mut |_, t, list| {
            if t < 0.3 {
                return;
            }
            assert!(list.gpu.len() == 1, "no fluid drive at {t}");
            if t > demo::ONSETS[3] + 0.05 {
                assert!(list.layers.iter().any(|l| l.displace.is_some()), "no smear at {t}");
            }
            frames += 1;
        },
    );
    assert!(frames > 100, "{frames} frames");
}
