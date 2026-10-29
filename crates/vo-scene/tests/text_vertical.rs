//! Vertical text set with the font's vertical forms and metrics (EG-30).

use skrifa::instance::Size as FontSize;
use skrifa::MetadataProvider;
use vo_scene::element::TextStyle;
use vo_scene::font::{test_font_path, FontId, FontRegistry};
use vo_scene::text::{EstimateText, ShapedLayout, TextLayout};

fn jp(fonts: &FontRegistry) -> FontId {
    fonts.asset("jp-labels").unwrap()
}

const SAMPLE: &str = "音声入力。";

#[test]
fn a_vertical_label_runs_down_one_column() {
    let fonts = FontRegistry::new();
    let style = TextStyle::new(jp(&fonts), 16.0, 20.0).vertical();
    let t = ShapedLayout::new(fonts.clone()).layout(SAMPLE, &style, None);
    assert_eq!(t.glyphs.len(), 5);
    for pair in t.glyphs.windows(2) {
        assert!(pair[1].pos.y > pair[0].pos.y, "{:?} then {:?}", pair[0].pos, pair[1].pos);
    }
    // Every glyph is a full-width form, so their left edges line up.
    let x0 = t.glyphs[0].pos.x;
    assert!(t.glyphs.iter().all(|g| (g.pos.x - x0).abs() <= 0.5), "{:?}", t.glyphs);
}

#[test]
fn the_full_stop_takes_its_vertical_form_in_the_top_right() {
    let fonts = FontRegistry::new();
    let font = jp(&fonts);
    let layout = ShapedLayout::new(fonts.clone());
    let style = TextStyle::new(font, 16.0, 20.0);
    let across = layout.layout(SAMPLE, &style, None);
    let down = layout.layout(SAMPLE, &style.vertical(), None);
    let (h, v) = (across.glyphs[4], down.glyphs[4]);
    assert_ne!(h.glyph, v.glyph, "vert swaps in the vertical full stop");

    // Its em box: the column across, and from the pen to the next one down.
    let face = fonts.get(font).unwrap();
    let metrics = face.skrifa().glyph_metrics(FontSize::new(16.0), face.location());
    let ink = metrics.bounds(skrifa::GlyphId::new(v.glyph)).expect("ink");
    // The last cell's em box runs from one advance above the column's bottom.
    let cell_top = down.size.h - 16.0;
    let (ink_x0, ink_x1) = (v.pos.x + ink.x_min, v.pos.x + ink.x_max);
    let (ink_y0, ink_y1) = (v.pos.y - ink.y_max, v.pos.y - ink.y_min);
    let (mid_x, mid_y) = (0.5 * down.size.w, cell_top + 8.0);
    assert!(ink_x0 >= mid_x, "ink {ink_x0}..{ink_x1} right of {mid_x}");
    assert!(ink_y1 <= mid_y, "ink {ink_y0}..{ink_y1} above {mid_y}");
    assert!(ink_y0 >= cell_top - 0.5 && ink_x1 <= down.size.w + 0.5, "inside the em box");
}

#[test]
fn the_height_is_the_sum_of_the_vertical_advances() {
    let fonts = FontRegistry::new();
    let font = jp(&fonts);
    let style = TextStyle::new(font, 16.0, 20.0).vertical();
    let t = ShapedLayout::new(fonts.clone()).layout(SAMPLE, &style, None);
    let face = fonts.get(font).unwrap();
    let upem = face.units_per_em() as f32;
    let shaper = harfrust::ShaperFont::new(face.shaping());
    let sum: f32 =
        t.glyphs.iter().map(|g| -(shaper.glyph_v_advance(harfrust::GlyphId::new(g.glyph)) as f32) * 16.0 / upem).sum();
    assert!((t.size.h - sum).abs() < 1e-3, "{} against {sum}", t.size.h);
    // One em each in this face; the column is as wide as the widest glyph.
    assert!((t.size.h - 80.0).abs() < 1e-3);
    assert!((t.size.w - 16.0).abs() < 1e-3, "{}", t.size.w);

    // Tracking goes between characters, not after the last.
    let tracked = ShapedLayout::new(fonts.clone()).layout(SAMPLE, &TextStyle { tracking: 2.0, ..style }, None);
    assert!((tracked.size.h - 88.0).abs() < 1e-3, "{}", tracked.size.h);
}

#[test]
fn latin_stands_upright_in_a_column() {
    let fonts = FontRegistry::new();
    let font = fonts.add_file(&test_font_path()).unwrap();
    let style = TextStyle::new(font, 16.0, 20.0).vertical();
    let t = ShapedLayout::new(fonts.clone()).layout("REC 42", &style, None);
    assert_eq!(t.glyphs.len(), 5, "the space places no glyph");
    for pair in t.glyphs.windows(2) {
        assert!(pair[1].pos.y > pair[0].pos.y);
    }
    // Upright: each letter is centred on the column by its own width.
    for g in &t.glyphs {
        assert!((g.pos.x + 0.5 * g.advance - 0.5 * t.size.w).abs() < 0.5, "{g:?} in {}", t.size.w);
    }
    assert!(t.size.w > 0.0 && t.size.h > t.glyphs.last().unwrap().pos.y);

    // Japanese labels fall back to it for Latin letters without panicking.
    let jp = jp(&fonts);
    fonts.set_fallbacks(jp, &[font]);
    let mixed = ShapedLayout::new(fonts.clone()).layout("録音 REC", &TextStyle::new(jp, 16.0, 20.0).vertical(), None);
    assert_eq!(mixed.glyphs.len(), 5);
}

#[test]
fn estimate_text_sets_vertical_text_too() {
    let style = TextStyle::new(0, 10.0, 12.0).vertical();
    let est = EstimateText::default();
    let t = est.layout("ab c", &style, None);
    assert_eq!(t.glyphs.len(), 3);
    let ys: Vec<f32> = t.glyphs.iter().map(|g| g.pos.y).collect();
    assert_eq!(ys, vec![8.0, 18.0, 31.0]);
    assert!(t.glyphs.iter().all(|g| g.pos.x == 0.0));
    assert_eq!((t.size.w, t.size.h), (5.5, 33.0));
    let capped = est.layout("abcd", &TextStyle { max_lines: 2, ..style }, None);
    assert_eq!(capped.glyphs.len(), 2);
}
