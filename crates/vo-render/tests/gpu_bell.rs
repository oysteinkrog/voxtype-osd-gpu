//! The margin bell (MO-31): the newest line tightens as it nears the right edge and relaxes
//! when the text wraps; with a carriage return the new line arrives a few pixels right and
//! settles. The golden stacks frames of a line filling up and wrapping, top to bottom.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::bell::Bell;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 360.0;
const H: f32 = 84.0;

const ALL: [(u32, &str); 10] = [
    (1, "the"),
    (2, "line"),
    (3, "tightens"),
    (4, "as"),
    (5, "it"),
    (6, "nears"),
    (7, "its"),
    (8, "margin"),
    (9, "then"),
    (10, "relaxes"),
];

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn view(w: &[Word], bell: Option<Bell>) -> Element<()> {
    let mut live = Element::live_text("live", w)
        .text_style(TextStyle::new(0, 22.0, 28.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .width(W - 32.0);
    if let Some(b) = bell {
        live = live.margin_bell(b);
    }
    Element::panel("panel").pad(16.0).radius(12.0).fill(Color::hex(0x2a2d33, 1.0)).width(W).height(H).child(live)
}

fn scene(fonts: &FontRegistry) -> Scene<()> {
    let mut s = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    s.set_viewport(0.0, Size::new(W, H), 1.0);
    s
}

/// How many words fit on the first line.
fn first_line(fonts: &FontRegistry) -> usize {
    let mut probe = scene(fonts);
    probe.apply(view(&words(&ALL), None), 0.0);
    let list = probe.sample(5.0);
    let y = |id: u32| list.glyphs.iter().find(|g| g.word == id).map(|g| g.pos.y);
    let n = ALL.iter().take_while(|w| y(w.0) == y(1)).count();
    assert!((3..ALL.len()).contains(&n), "first line holds {n} words");
    n
}

/// The x of the last glyph of word `id`.
fn end_of(list: &DisplayList, id: u32) -> f32 {
    list.glyphs.iter().filter(|g| g.word == id).map(|g| g.pos.x).fold(f32::MIN, f32::max)
}

/// Words arrive one every 0.4 s, through the wrap: the frame at `t`, after every word due
/// by then.
struct Play {
    scene: Scene<()>,
    n: usize,
    next: usize,
    bell: Option<Bell>,
}

impl Play {
    fn new(fonts: &FontRegistry, n: usize, bell: Option<Bell>) -> Play {
        Play { scene: scene(fonts), n, next: 1, bell }
    }

    fn at(&mut self, t: f64) -> DisplayList {
        while self.next <= self.n + 1 && 0.4 * self.next as f64 <= t {
            let k = self.next;
            self.scene.apply(view(&words(&ALL[..k]), self.bell), 0.4 * k as f64);
            self.next += 1;
        }
        self.scene.sample(t)
    }
}

#[test]
fn bell_goldens() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let n = first_line(&fonts);
    let mut play = Play::new(&fonts, n, Some(Bell::DEFAULT.carriage(3.0)));
    let wrap = 0.4 * (n + 1) as f64;
    let times = [0.4 * n as f64 - 0.3, 0.4 * n as f64 + 0.35, wrap + 0.03, wrap + 0.1, wrap + 0.6];
    let mut px = Vec::new();
    for &t in &times {
        px.extend(off.render_to_rgba(&play.at(t), [W as u32, H as u32], 1.0));
    }
    let img = Image { name: "margin-bell".into(), w: W as u32, h: H as u32 * times.len() as u32, channels: 4, px };
    support::check(&img, Tolerance::default()).unwrap();
}

/// The full line ends further left with the bell, by no more than 5 % of its length, and
/// is back where the layout puts it once the wrap has relaxed it.
#[test]
fn full_line_tightens_then_relaxes() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let n = first_line(&fonts);
    let last = ALL[n - 1].0;
    let (mut plain, mut belled) = (Play::new(&fonts, n, None), Play::new(&fonts, n, Some(Bell::DEFAULT)));
    let full = 0.4 * n as f64 + 0.35;
    let (p, q) = (plain.at(full), belled.at(full));
    let start = q.glyphs.iter().filter(|g| g.word == 1).map(|g| g.pos.x).fold(f32::MAX, f32::min);
    let (a, b) = (end_of(&p, last), end_of(&q, last));
    assert!(b < a - 0.5, "the full line tightens: {b} against {a}");
    assert!(a - b <= 0.05 * (a - start), "at most 5 %: {b} against {a}");
    let after = 0.4 * (n + 1) as f64 + 1.0;
    let (a, b) = (end_of(&plain.at(after), last), end_of(&belled.at(after), last));
    assert!((a - b).abs() < 0.05, "relaxed after the wrap: {b} against {a}");
}
