//! Centred live text (FX-002) and inline objects in the flow (FX-063): lines re-centre on
//! a spring as they grow, and objects take room, wrap and spring like words.

use vo_core::words::Word;
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::inline::{Inline, OBJECT_GLYPH, PAUSE_MIN_EMS};
use vo_scene::text::{Align, ShapedLayout};
use vo_scene::Scene;

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn live(w: &[Word], style: TextStyle, width: f32, objects: Vec<(u32, Inline)>) -> Element<()> {
    Element::column("root").child(Element::live_text("live", w).text_style(style).width(width).inline_objects(objects))
}

fn first(list: &DisplayList, word: u32) -> f32 {
    list.glyphs.iter().find(|g| g.word == word).map(|g| g.pos.x).unwrap()
}

/// Each line's ink as `(left, right)`, by baseline.
fn line_spans(list: &DisplayList) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32, f32)> = Vec::new();
    for g in &list.glyphs {
        match out.iter_mut().find(|l| (l.0 - g.pos.y).abs() < 0.5) {
            Some(l) => (l.1, l.2) = (l.1.min(g.pos.x), l.2.max(g.pos.x)),
            None => out.push((g.pos.y, g.pos.x, g.pos.x)),
        }
    }
    out.into_iter().map(|(_, a, b)| (a, b)).collect()
}

const STYLE: TextStyle = TextStyle::new(0, 20.0, 26.0);

#[test]
fn a_centred_line_recentres_on_a_spring_as_it_grows() {
    let mut s = scene();
    let style = STYLE.centered();
    s.apply(live(&words(&[(1, "hello")]), style, 400.0, vec![]), 0.0);
    let before = s.sample(1.0);
    let x0 = first(&before, 1);
    // Centred in the 400 wide box.
    assert!(x0 > 150.0 && x0 < 200.0, "{x0}");
    s.apply(live(&words(&[(1, "hello"), (2, "there")]), style, 400.0, vec![]), 1.0);
    let settled = first(&s.sample(5.0), 1);
    assert!(settled < x0 - 20.0, "moves left to make room: {x0} to {settled}");
    // No jump at the change, and part of the way there mid spring.
    assert!((first(&s.sample(1.0), 1) - x0).abs() < 1e-3);
    let mid = first(&s.sample(1.05), 1);
    assert!(mid < x0 - 0.5 && mid > settled + 0.5, "{x0} {mid} {settled}");
}

#[test]
fn a_wrapped_centred_paragraph_has_each_line_centred_in_the_box() {
    let mut s = scene();
    let text = "the quick brown fox jumps over the lazy dog and keeps on running";
    let w: Vec<Word> = text.split(' ').enumerate().map(|(i, t)| Word { id: i as u32 + 1, text: t.into() }).collect();
    s.apply(live(&w, STYLE.centered().balanced(), 300.0, vec![]), 0.0);
    let list = s.sample(0.0);
    let spans = line_spans(&list);
    assert!(spans.len() >= 3, "{spans:?}");
    // Glyph pen positions, so the right end lacks the last advance: allow a glyph's width.
    for (a, b) in &spans {
        let mid = 0.5 * (a + b);
        assert!((mid - 150.0).abs() < 12.0, "line {a}..{b}");
    }
}

#[test]
fn an_object_takes_room_draws_its_mark_and_no_glyph() {
    let mut s = scene();
    let plain = words(&[(1, "hello"), (2, "world")]);
    s.apply(live(&plain, STYLE, 400.0, vec![]), 0.0);
    let without = first(&s.sample(0.0), 2);

    let mut s = scene();
    let w = vec![plain[0].clone(), Inline::word(100), plain[1].clone()];
    s.apply(live(&w, STYLE, 400.0, vec![(100, Inline::pause(0.0))]), 0.0);
    let list = s.sample(0.0);
    let with = first(&list, 2);
    // The pause's width and one more space.
    assert!(with - without > PAUSE_MIN_EMS * 20.0, "{without} {with}");
    assert!(list.glyphs.iter().all(|g| g.glyph != OBJECT_GLYPH));
    let dots = list.shapes.iter().filter(|sh| sh.kind == ShapeKind::Circle).count();
    assert_eq!(dots, 1);
}

#[test]
fn a_growing_pause_springs_and_the_words_after_it_slide() {
    let mut s = scene();
    let w = vec![Word { id: 1, text: "so".into() }, Inline::word(100), Word { id: 2, text: "then".into() }];
    s.apply(live(&w, STYLE, 400.0, vec![(100, Inline::pause(0.1))]), 0.0);
    let x0 = first(&s.sample(1.0), 2);
    s.apply(live(&w, STYLE, 400.0, vec![(100, Inline::pause(2.0))]), 1.0);
    let settled = s.sample(5.0);
    let x1 = first(&settled, 2);
    assert!(x1 > x0 + 20.0, "{x0} {x1}");
    let mid = first(&s.sample(1.05), 2);
    assert!(mid > x0 + 0.5 && mid < x1 - 0.5, "{x0} {mid} {x1}");
    let dots = |l: &DisplayList| l.shapes.iter().filter(|sh| sh.kind == ShapeKind::Circle).count();
    assert!(dots(&settled) > 3, "{}", dots(&settled));
}

#[test]
fn a_chip_boxes_the_words_between_its_ends() {
    let mut s = scene();
    let w = vec![
        Word { id: 1, text: "stop".into() },
        Inline::word(100),
        Word { id: 2, text: "end".into() },
        Word { id: 3, text: "sentence?".into() },
        Inline::word(101),
    ];
    s.apply(live(&w, STYLE, 400.0, vec![(100, Inline::chip_open()), (101, Inline::chip_close())]), 0.0);
    let list = s.sample(0.0);
    let boxes: Vec<_> = list.shapes.iter().filter(|sh| sh.kind == ShapeKind::RRect).collect();
    assert_eq!(boxes.len(), 1);
    let (x, w) = (boxes[0].geom[0], boxes[0].geom[2]);
    assert!(x < first(&list, 2) && x > first(&list, 1));
    let last = list.glyphs.iter().filter(|g| g.word == 3).map(|g| g.pos.x).fold(0.0, f32::max);
    assert!(x + w > last);
}

#[test]
fn objects_wrap_like_words() {
    let mut s = scene();
    let w = vec![Word { id: 1, text: "word".into() }, Inline::word(100), Word { id: 2, text: "next".into() }];
    // The gap slot is too wide to share the first line.
    s.apply(live(&w, STYLE.align(Align::Start), 120.0, vec![(100, Inline::gap(4.0, vec![0.5; 12]))]), 0.0);
    let list = s.sample(0.0);
    let bars: Vec<_> = list.shapes.iter().filter(|sh| sh.kind == ShapeKind::Capsule).collect();
    assert_eq!(bars.len(), 12);
    let first_y = list.glyphs.iter().find(|g| g.word == 1).unwrap().pos.y;
    assert!(bars.iter().all(|b| b.geom[1] > first_y), "the slot is on the second line");
}
