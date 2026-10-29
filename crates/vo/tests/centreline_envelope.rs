//! The reading-line envelope (MO-08) holds while words are written along their own
//! centre lines (EG-29): the centreline demo's app, three words, through `check_scenario`
//! on the CPU in both faces. The type sits at 0.7 ink from its first frame and the pen only
//! draws over it.

#[path = "../examples/centreline_demo.rs"]
#[allow(dead_code)]
mod demo;

use vo::envelope::{check_scenario, Envelope};

#[test]
fn centreline_demo_keeps_the_reading_line_envelope() {
    for font in ["grotesk", "extra-condensed-bold"] {
        let r = check_scenario(
            demo::Demo::new(font),
            &demo::scenario(),
            60.0,
            demo::options(demo::fonts(font).0),
            &Envelope::default(),
        );
        r.assert_ok();
        assert!(r.glyphs_checked > 100, "{font}: {r}");
    }
}
