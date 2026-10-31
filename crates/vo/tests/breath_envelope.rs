//! The reading-line envelope (MO-08) holds while words breathe smoothly through a variable
//! font (EG-23): the breath demo's app, weight, width and slant ramps, through
//! `check_scenario` on the CPU. The weight is edge growth, the slant a lean of at most half
//! a degree past the instance, and the width moves the pens a share of a width step.

#[path = "../examples/breath_demo.rs"]
#[allow(dead_code)]
mod demo;

use vo::envelope::{check_scenario, Envelope};

#[test]
fn breath_demo_keeps_the_reading_line_envelope() {
    let (fonts, _) = demo::fonts();
    let app = demo::Demo::with_fonts(fonts.clone());
    let r = check_scenario(app, &demo::scenario(), 60.0, demo::options(fonts), &Envelope::default());
    r.assert_ok();
    assert!(r.glyphs_checked > 1000, "{r}");
}
