//! The reading-line envelope (MO-08) holds on the blur demo (EG-04): frosted depth-stack
//! sheets whose older lines are blurred, words arriving, run through `check_scenario` on the
//! CPU. No blur reaches the newest line: its layer has radius 0, so the scene emits none.

#[path = "../examples/blur_demo.rs"]
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
fn blur_demo_keeps_the_reading_line_envelope() {
    for reduced in [false, true] {
        let r = check_scenario(
            demo::Demo::new(3.0, reduced),
            &demo::scenario(),
            60.0,
            demo::options(fonts(), reduced),
            &Envelope::default(),
        );
        r.assert_ok();
        assert!(r.glyphs_checked > 100, "reduced {reduced}: {r}");
    }
}

/// Once three lines are up, every frame has frosted sheets and blurred older lines, and no
/// layer blur holds the newest word.
#[test]
fn older_lines_blur_and_the_newest_does_not() {
    let mut frames = 0;
    vo::envelope::sample_scenario(
        demo::Demo::new(3.0, false),
        &demo::scenario(),
        30.0,
        demo::options(fonts(), false),
        &mut |_, t, list| {
            if t < 3.5 {
                return;
            }
            assert!(list.layers.iter().any(|l| l.frost > 0.0), "no frost at {t}");
            assert!(list.layers.iter().any(|l| l.blur.is_some()), "no blurred line at {t}");
            assert!(
                list.layers.iter().filter_map(|l| l.blur).all(|b| b.keep.is_none()),
                "a blur holds the reading line at {t}"
            );
            frames += 1;
        },
    );
    assert!(frames > 30, "{frames}");
}
