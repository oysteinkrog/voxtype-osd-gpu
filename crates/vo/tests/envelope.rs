//! The envelope check (MO-08) on a real app and scene: live text whose words arrive with
//! different enter transitions, run through a scenario on the CPU. The GPU budget test runs
//! the same scenario offline and is skipped on an adapter without timestamps.

use vo::envelope::{check_scenario, measure_budget, sample_scenario, Envelope};
use vo::scene::arrival::Arrival;
use vo::scene::display::NO_WORD;
use vo::scene::element::{Element, TextStyle};
use vo::scene::font::{test_font_path, FontRegistry};
use vo::scene::geom::{Color, Size};
use vo::scene::Transition;
use vo::{App, Cx, Input, Options, Scenario, Tokens};
use vo_core::words::Word;

struct Live {
    words: Vec<Word>,
    enter: Transition,
    tint: Option<Color>,
    arrival: Arrival,
}

impl App for Live {
    type Msg = Vec<Word>;

    fn update(&mut self, words: Vec<Word>, _: &mut Cx<Vec<Word>>) {
        self.words = words;
    }

    fn view(&self, _: &Tokens) -> Element<Vec<Word>> {
        let mut live = Element::live_text("live", &self.words)
            .text_style(TextStyle::new(0, 22.0, 28.0))
            .color(Color::hex(0xf2f3f5, 1.0))
            .word_enter(self.enter)
            .word_arrival(self.arrival)
            .width(328.0);
        if let Some(c) = self.tint {
            live = live.envelope_tint(c);
        }
        Element::panel("panel").pad(16.0).fill(Color::hex(0x1d1f24, 1.0)).width(360.0).height(120.0).child(live)
    }
}

fn scenario() -> Scenario<Vec<Word>> {
    let text = ["open", "the", "configuration", "file", "and", "set", "the", "theme"];
    let inputs = (1..=text.len())
        .map(|n| {
            let words = text[..n].iter().enumerate().map(|(i, w)| Word { id: i as u32, text: (*w).into() }).collect();
            (0.1 + n as f64 * 0.25, Input::Msg(words))
        })
        .collect();
    Scenario { size: Size::new(360.0, 120.0), scale: 1.0, duration: 3.0, inputs }
}

fn options() -> Options<Vec<Word>> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Options { fonts, reduced_motion: false, ..Options::default() }
}

/// With the enter transition as given (`Arrival::Free`), so the check sees it.
fn check(enter: Transition) -> vo::envelope::Report {
    check_with(enter, Arrival::Free)
}

fn check_with(enter: Transition, arrival: Arrival) -> vo::envelope::Report {
    let app = Live { words: Vec::new(), enter, tint: None, arrival };
    check_scenario(app, &scenario(), 60.0, options(), &Envelope::default())
}

#[test]
fn words_that_arrive_in_place_pass() {
    let r = check(Transition::NONE);
    r.assert_ok();
    assert!(r.glyphs_checked > 100, "{r}");
}

#[test]
fn a_fade_in_breaks_ink() {
    let r = check(Transition::fade());
    let kinds: Vec<_> = r.by_kind().iter().map(|s| s.kind).collect();
    assert_eq!(kinds, ["ink"], "{r}");
}

#[test]
fn a_rise_breaks_the_baseline_and_a_small_one_does_not() {
    let r = check(Transition::NONE.offset(0.0, 10.0));
    assert!(r.by_kind().iter().any(|s| s.kind == "off-row" || s.kind == "baseline"), "{r}");
    check(Transition::NONE.offset(0.0, 2.0)).assert_ok();
}

#[test]
fn blur_on_arrival_is_caught() {
    // Live words take no scale from `word_enter`; the unit tests cover scale.
    let r = check(Transition::NONE.blur(6.0));
    assert!(r.by_kind().iter().any(|s| s.kind == "blur"), "{r}");
}

#[test]
fn by_default_every_arrival_is_pulled_inside_the_envelope() {
    for enter in [
        Transition::fade(),
        Transition::fade().offset(0.0, 10.0),
        Transition::NONE.offset(0.0, -12.0),
        Transition::NONE.blur(6.0).dissolve(1.0).wipe(1.0),
    ] {
        let r = check_with(enter, Arrival::Readable);
        assert!(r.is_ok(), "{enter:?}\n{r}");
    }
}

#[test]
fn the_budget_is_measured_per_frame() {
    let app = Live { words: Vec::new(), enter: Transition::fade(), tint: None, arrival: Arrival::Free };
    let Some(b) = measure_budget(app, &scenario(), 30.0, options()).expect("render") else {
        eprintln!("skipped: this adapter has no GPU timestamps inside encoders");
        return;
    };
    assert_eq!(b.size, [360, 120]);
    assert!(b.frames > 10, "{b}");
    assert!(b.p50_ms <= b.p95_ms && b.p95_ms <= b.max_ms, "{b}");
    assert!(!b.sections.is_empty(), "{b}");
    eprintln!("{b}");
    b.within(50.0, 50.0).expect("a plain card fits a generous budget");
}

#[test]
fn the_tint_marks_arrivals_that_break_the_envelope() {
    let tint = Color::hex(0xff0000, 1.0);
    let red = |c: &Color| c.r > 0.99 && c.g < 0.01 && c.b < 0.01;
    let count = |enter: Transition| {
        let app = Live { words: Vec::new(), enter, tint: Some(tint), arrival: Arrival::Free };
        let (mut early, mut late) = (0, 0);
        sample_scenario(app, &scenario(), 60.0, options(), &mut |_, t, list| {
            for _ in list.glyphs.iter().filter(|g| g.word != NO_WORD && red(&g.color)) {
                if t < 2.7 {
                    early += 1;
                } else {
                    late += 1;
                }
            }
        });
        (early, late)
    };
    let (early, late) = count(Transition::fade());
    assert!(early > 0, "a fade-in is tinted while it is faint");
    assert_eq!(late, 0, "nothing is tinted at rest");
    assert_eq!(count(Transition::NONE), (0, 0));
}
