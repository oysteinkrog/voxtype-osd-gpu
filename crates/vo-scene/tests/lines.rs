//! Lines by age (T14): breaks match the text layout, older lines keep their node, move up,
//! shrink and dim, and the oldest leaves.

use vo_core::words::Word;
use vo_scene::element::{Element, TextStyle};
use vo_scene::geom::Color;
use vo_scene::lines::{aged_lines, break_lines, Ages, Line};
use vo_scene::taffy::prelude::JustifyContent;
use vo_scene::text::{EstimateText, TextLayout};
use vo_scene::{NodeId, Presence, Scene};

const STYLE: TextStyle =
    TextStyle { font: 0, size: 20.0, line_height: 24.0, max_lines: 0, ellipsis: false, tracking: 0.0 };
/// Ten characters of 11 units, with spaces of 6: "aaaa bbbb" fits, a third word does not.
const WIDTH: f32 = 110.0;

fn words(n: usize) -> Vec<Word> {
    (0..n).map(|i| Word { id: i as u32 + 1, text: format!("w{i:02}") }).collect()
}

fn lines(n: usize) -> Vec<Line> {
    break_lines(&EstimateText::default(), &words(n), &STYLE, WIDTH)
}

fn view(n: usize) -> Element<()> {
    // Bottom-anchored, so the text rolls up.
    Element::column("root").size(200.0, 200.0).justify(JustifyContent::FLEX_END).child(aged_lines(
        "p",
        &lines(n),
        STYLE,
        Color::WHITE,
        &Ages::teleprompter(),
    ))
}

fn line(scene: &Scene, first_word: u32) -> Option<NodeId> {
    scene.find(&["root", "p", &format!("p-l{first_word}")])
}

#[test]
fn breaks_follow_the_text_layout() {
    let ls = lines(7);
    // "w00 w01 w02" is 3 * 3 * 11 + 2 * 6 = 111 > 110, so two words per line.
    assert_eq!(ls.iter().map(|l| l.words.len()).collect::<Vec<_>>(), vec![2, 2, 2, 1]);
    let joined = |l: &Line| l.words.iter().map(|w| w.text.clone()).collect::<Vec<_>>().join(" ");
    assert_eq!(joined(&ls[1]), "w02 w03");
    let w = EstimateText::default().layout("w02 w03", &STYLE, None).size.w;
    assert_eq!(ls[1].width, w);
}

#[test]
fn older_lines_move_up_shrink_dim_and_leave() {
    let mut scene = Scene::new();
    scene.apply(view(4), 0.0);
    // Lines start at words 1 and 3; the older one is at age 1.
    let (a, b) = (line(&scene, 1).unwrap(), line(&scene, 3).unwrap());
    let sa = scene.sampled(a, 2.0).unwrap();
    let sb = scene.sampled(b, 2.0).unwrap();
    assert!((sb.opacity - 1.0).abs() < 1e-3 && (sa.opacity - 0.45).abs() < 1e-3);
    assert!((sa.transform.a - 0.88).abs() < 1e-3, "age 1 scales to 0.88");
    let y_b = sb.transform.f;

    // A third line starts: b keeps its node, moves up a line and takes the age 1 look.
    scene.apply(view(5), 3.0);
    assert_eq!(line(&scene, 3), Some(b));
    let mid = scene.sampled(b, 3.1).unwrap();
    assert!(mid.opacity < 0.99 && mid.opacity > 0.45, "dimming, at {}", mid.opacity);
    let settled = scene.sampled(b, 5.0).unwrap();
    assert!((settled.opacity - 0.45).abs() < 1e-3);
    assert!(settled.transform.f < y_b - 20.0, "moved up from {y_b} to {}", settled.transform.f);
    // The left end stays put while the line shrinks.
    assert!((settled.transform.e - sb.transform.e).abs() < 0.01);
    // a is now age 2, which fades out in place.
    assert!(scene.sampled(a, 5.0).unwrap().opacity < 1e-3);

    // A fourth line: a is older than the last look and leaves.
    scene.apply(view(7), 6.0);
    assert!(matches!(scene.presence(a, 6.05), Presence::Leaving { .. }));
}
