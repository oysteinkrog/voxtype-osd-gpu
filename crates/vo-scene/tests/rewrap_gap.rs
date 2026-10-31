//! Words keep their gap while a line re-wraps (s8my): when the final pass lands and words
//! move to other lines, no two neighbouring words on one row come closer than half a space
//! in any frame at 30 fps, counting the old copy of a word that faded off its line and the
//! new copy that fades in at its place.

use std::collections::HashMap;

use vo_core::words::Word;
use vo_scene::display::{DisplayList, NO_WORD};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::Revise;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::{Align, ShapedLayout, TextLayout};
use vo_scene::Scene;

const SIZE: f32 = 20.0;
const LINE: f32 = 26.0;
/// Below this ink a copy is not seen.
const SEEN: f32 = 0.05;

const SAID: &str = "with the waveform above them then press the key again to stop and let the final pass \
                    fix the casing and the punctuation before the text is typed into the editor";
const FINAL: &str = "with the waveform above them. Then press the key again to stop, and let the final pass \
                     fix the casing and the punctuation before the text is typed into the editor.";

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn style(align: Align) -> TextStyle {
    TextStyle::new(0, SIZE, LINE).align(align)
}

fn words(text: &str) -> Vec<Word> {
    text.split(' ').enumerate().map(|(i, w)| Word { id: i as u32 + 1, text: w.into() }).collect()
}

fn view(w: &[Word], align: Align, revise: Revise, width: f32) -> Element<()> {
    let live = Element::live_text("live", w).text_style(style(align)).width(width).word_revise(revise);
    Element::column("root").child(live)
}

/// How far the pen moves after each glyph, from the words shaped alone.
fn advances(layout: &ShapedLayout, align: Align) -> HashMap<u32, f32> {
    let mut map = HashMap::new();
    for w in SAID.split(' ').chain(FINAL.split(' ')) {
        for g in layout.layout(w, &style(align), None).glyphs {
            map.insert(g.glyph, g.advance);
        }
    }
    map
}

/// Every drawn copy of a word on a row, `(row, left, right, word)`, left to right per row.
fn spans(list: &DisplayList, adv: &HashMap<u32, f32>) -> Vec<(i32, f32, f32, u32)> {
    let mut by: HashMap<(u32, i32), (f32, f32)> = HashMap::new();
    for g in list.glyphs.iter().filter(|g| g.word != NO_WORD && g.color.a > SEEN) {
        let row = (g.pos.y / LINE).round() as i32;
        // A glyph drawn smaller (a pop) is that much narrower.
        let right = g.pos.x + adv.get(&g.glyph).copied().unwrap_or(0.0) * g.size / SIZE;
        let e = by.entry((g.word, row)).or_insert((g.pos.x, right));
        e.0 = e.0.min(g.pos.x);
        e.1 = e.1.max(right);
    }
    let mut out: Vec<_> = by.into_iter().map(|((w, row), (a, b))| (row, a, b, w)).collect();
    out.sort_by(|p, q| (p.0, p.1).partial_cmp(&(q.0, q.1)).unwrap());
    out
}

/// The narrowest gap between neighbouring words on one row.
fn narrowest(list: &DisplayList, adv: &HashMap<u32, f32>) -> Option<(f32, u32, u32)> {
    let s = spans(list, adv);
    s.windows(2)
        .filter(|p| p[0].0 == p[1].0 && p[0].3 != p[1].3)
        .map(|p| (p[1].1 - p[0].2, p[0].3, p[1].3))
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// Whether the final pass re-wrapped a line at `width`.
fn check_at(align: Align, revise: Revise, width: f32) -> bool {
    let layout = ShapedLayout::new(fonts());
    let adv = advances(&layout, align);
    let space = layout.layout("a b", &style(align), None).size.w - layout.layout("ab", &style(align), None).size.w;
    assert!(space > 1.0);
    let mut s = Scene::new().with_text(Box::new(ShapedLayout::new(fonts())));
    s.apply(view(&words(SAID), align, revise, width), 0.0);
    let at_rest = narrowest(&s.sample(2.0), &adv).unwrap().0;
    assert!(at_rest >= 0.9 * space, "at rest the words sit a space apart ({at_rest} of {space})");
    let rows = |l: &DisplayList| -> HashMap<u32, i32> { spans(l, &adv).into_iter().map(|s| (s.3, s.0)).collect() };
    let before = rows(&s.sample(2.0));
    s.apply(view(&words(FINAL), align, revise, width), 2.0);
    let after = rows(&s.sample(4.0));
    let rewrapped = before.iter().any(|(w, r)| after.get(w) != Some(r));
    for n in 0..45 {
        let t = 2.0 + n as f64 / 30.0;
        if let Some((gap, a, b)) = narrowest(&s.sample(t), &adv) {
            assert!(
                gap >= 0.5 * space - 0.01,
                "{align:?} {revise:?} at {width} px: words {a} and {b} {gap:.2} px apart at frame {n} (half a space is {:.2})",
                0.5 * space
            );
        }
    }
    rewrapped
}

fn check(align: Align, revise: Revise) {
    let rewraps = (0..19).filter(|k| check_at(align, revise, 220.0 + 10.0 * *k as f32)).count();
    assert!(rewraps >= 3, "the final pass re-wraps lines at {rewraps} of 19 widths");
}

#[test]
fn centred_lines_keep_their_gaps_on_the_final_pass() {
    check(Align::Center, Revise::Palimpsest);
}

#[test]
fn centred_particle_text_keeps_its_gaps_on_the_final_pass() {
    check(Align::Center, Revise::Swap);
}

#[test]
fn left_aligned_lines_keep_their_gaps_on_the_final_pass() {
    check(Align::Start, Revise::Palimpsest);
}
