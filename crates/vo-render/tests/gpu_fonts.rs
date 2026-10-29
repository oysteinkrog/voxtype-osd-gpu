//! Built-in fonts and font fallback (WP7, FX-003, FX-004).
//!
//! `fonts-specimen`: one line in every built-in font, loaded by name. `fonts-fallback`: a
//! mecha-style header where a condensed Latin face falls back to the Japanese label face for
//! the kanji, a Norwegian line, and the same header with no fallback chain, where the kanji
//! are the primary face's missing-glyph boxes.
//!
//! GPU test: `just test-gpu fonts`.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const SPECIMEN: [(&str, &str); 9] = [
    ("sans", "sans: Noto Sans, the quick brown fox"),
    ("serif", "serif: Noto Serif, the quick brown fox"),
    ("mono", "mono: JetBrains Mono 0123456789 {}[]"),
    ("grotesk", "grotesk: Inter, the quick brown fox"),
    ("condensed", "CONDENSED: BARLOW CONDENSED MEDIUM"),
    ("condensed-bold", "CONDENSED-BOLD: SO THE PLAN FOR TOMORROW"),
    ("thin", "thin: Barlow Thin, the quick brown fox"),
    ("jp-labels", "jp-labels: 音声入力 テキストに変換中"),
    ("jp-labels-bold", "jp-labels-bold: 作戦継続中 未来の都市"),
];

fn render(fonts: &FontRegistry, view: Element<()>, size: Size) -> (Image, vo_scene::DisplayList) {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view, 0.0);
    let list = scene.sample(10.0);
    assert!(list.glyphs.iter().all(|g| fonts.get(g.font).is_some()));
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let (w, h) = (size.w as u32, size.h as u32);
    let px = off.render_to_rgba(&list, [w, h], 1.0);
    (Image { name: String::new(), w, h, channels: 4, px }, list)
}

fn ink() -> Color {
    Color::hex(0xf2f4f8, 1.0)
}

#[test]
fn specimen_of_every_built_in_font() {
    let fonts = FontRegistry::new();
    // The Japanese faces have no Latin letters: their captions fall back to Noto Sans.
    fonts.set_default_fallbacks(&[fonts.asset("sans").unwrap()]);
    let mut col = Element::column("col").pad(12.0).gap(4.0);
    for (name, line) in SPECIMEN {
        let id = fonts.asset(name).unwrap_or_else(|e| panic!("{e}"));
        col = col.child(Element::label(name, line).text_style(TextStyle::new(id, 20.0, 26.0)).color(ink()));
    }
    let size = Size::new(520.0, 290.0);
    let view = Element::panel("bg").size(size.w, size.h).fill(Color::hex(0x101216, 1.0)).child(col);
    let (img, list) = render(&fonts, view, size);
    assert!(list.glyphs.iter().all(|g| g.glyph != 0), "a missing-glyph box");
    let img = Image { name: "fonts-specimen".into(), ..img };
    support::check(&img, Tolerance::default()).unwrap();
}

#[test]
fn condensed_header_falls_back_to_japanese() {
    let fonts = FontRegistry::new();
    let head = fonts.asset("condensed-bold").unwrap();
    let jp = fonts.asset("jp-labels-bold").unwrap();
    let body = fonts.asset("condensed").unwrap();
    // A second handle to the same face with no chain, to show what the fallback fixes.
    let bare = fonts.add(fonts.get(head).unwrap().data().to_vec()).unwrap();
    fonts.set_fallbacks(head, &[jp]);
    fonts.set_fallbacks(body, &[jp]);
    let orange = Color::hex(0xff8a1f, 1.0);
    let line = |key: &str, text: &str, font, size: f32| {
        Element::label(key.to_string(), text).text_style(TextStyle::new(font, size, size * 1.25)).color(orange)
    };
    let col = Element::column("col")
        .pad(14.0)
        .gap(6.0)
        .child(line("head", "音声入力 VOICE DICTATION", head, 34.0))
        .child(line("body", "録音中 LISTENING · Blåbærsyltetøy på Ærø", body, 22.0))
        .child(line("bare", "音声入力 VOICE DICTATION", bare, 34.0));
    let size = Size::new(460.0, 170.0);
    let view = Element::panel("bg").size(size.w, size.h).fill(Color::hex(0x0b0806, 1.0)).child(col);
    let (img, list) = render(&fonts, view, size);
    // The bare line draws the primary's missing glyph for each of its four kanji; the
    // others have none.
    assert_eq!(list.glyphs.iter().filter(|g| g.glyph == 0).count(), 4);
    assert!(list.glyphs.iter().filter(|g| g.glyph == 0).all(|g| g.font == bare));
    assert!(list.glyphs.iter().any(|g| g.font == jp));
    let img = Image { name: "fonts-fallback".into(), ..img };
    support::check(&img, Tolerance::default()).unwrap();
}

#[test]
fn specimen_of_the_eg28_faces() {
    let fonts = FontRegistry::new();
    fonts.set_default_fallbacks(&[fonts.asset("sans").unwrap()]);
    let lines = [
        ("typewriter", "typewriter: Hamburgefonstiv 0123 ÆØÅ"),
        ("display-wide-italic", "WIDE ITALIC: Hamburgefonstiv 0123"),
        ("ultra-condensed-heavy", "ULTRA CONDENSED HEAVY 0123 ÆØÅ"),
        ("script", "Script: Hamburgefonstiv 0123 ÆØÅ"),
        // Staff lines, G clef, flat, sharp, natural, whole, half and black note heads, quarter rest.
        ("music", "\u{E014}\u{E050}\u{E260}\u{E262}\u{E261}\u{E0A2}\u{E0A3}\u{E0A4}\u{E4E5}\u{E4E3}"),
    ];
    let mut col = Element::column("col").pad(12.0).gap(4.0);
    for (name, line) in lines {
        let id = fonts.asset(name).unwrap_or_else(|e| panic!("{e}"));
        col = col.child(Element::label(name, line).text_style(TextStyle::new(id, 24.0, 32.0)).color(ink()));
    }
    let size = Size::new(640.0, 250.0);
    let view = Element::panel("bg").size(size.w, size.h).fill(Color::hex(0x101216, 1.0)).child(col);
    let (img, list) = render(&fonts, view, size);
    assert!(list.glyphs.iter().all(|g| g.glyph != 0), "a missing-glyph box");
    let img = Image { name: "fonts-specimen-eg28".into(), ..img };
    support::check(&img, Tolerance::default()).unwrap();
}
