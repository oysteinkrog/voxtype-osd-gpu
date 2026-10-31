//! Drawings that follow live text glyphs (`Element::glyph_follow`): the follower gets the
//! glyphs as they are drawn in each frame, in the live text's box, also while a centred line
//! slides to its new middle; the drawing under the glyphs comes before them in the display
//! list and the one over them after.

use std::any::Any;
use std::sync::{Arc, Mutex};

use vo_core::words::Word;
use vo_scene::display::{GlyphInstance, ItemKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::gpu::{GlyphFollow, GpuSource};
use vo_scene::text::{Align, ShapedLayout};
use vo_scene::{taffy, Scene};

/// What a follower hands the renderer; no painter knows it.
#[derive(Debug)]
struct Drawn;

impl GpuSource for Drawn {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn until(&self) -> f64 {
        f64::NEG_INFINITY
    }
}

/// Paints nothing; remembers the glyphs it was last given.
#[derive(Debug, Default)]
struct Spy(Mutex<Vec<GlyphInstance>>);

impl GlyphFollow for Spy {
    fn follow(&self, glyphs: &[GlyphInstance], _t: f64) -> Option<Arc<dyn GpuSource>> {
        *self.0.lock().unwrap() = glyphs.to_vec();
        Some(Arc::new(Drawn))
    }

    fn until(&self) -> f64 {
        f64::NEG_INFINITY
    }
}

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

fn view(text: &str, under: &Arc<Spy>, over: &Arc<Spy>) -> Element<()> {
    let w: Vec<Word> = text.split(' ').enumerate().map(|(i, t)| Word { id: i as u32 + 1, text: t.into() }).collect();
    let live = Element::live_text("live", &w)
        .text_style(TextStyle::new(0, 20.0, 26.0).align(Align::Center))
        .width(400.0)
        .glyph_follow(under.clone(), false)
        .glyph_follow(over.clone(), true);
    Element::column("root").child(live.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(30.0);
        s.inset.top = taffy::style::LengthPercentageAuto::length(50.0);
    }))
}

#[test]
fn followers_get_the_drawn_glyphs_in_every_frame_in_order() {
    let (under, over) = (Arc::new(Spy::default()), Arc::new(Spy::default()));
    let mut s = scene();
    s.apply(view("the", &under, &over), 0.0);
    s.sample(1.0);
    s.apply(view("the configuration", &under, &over), 1.0);
    let mut moved = false;
    let mut first = None;
    for n in 0..30 {
        let t = 1.0 + n as f64 / 120.0;
        let list = s.sample(t);
        let got = under.0.lock().unwrap().clone();
        let drawn: Vec<_> = list.glyphs.iter().filter(|g| g.word != u32::MAX).collect();
        assert_eq!(got.len(), drawn.len(), "frame {n}");
        for (g, d) in got.iter().zip(&drawn) {
            assert!((g.pos.x + 30.0 - d.pos.x).abs() < 1e-3 && (g.pos.y + 50.0 - d.pos.y).abs() < 1e-3, "frame {n}");
        }
        assert_eq!(*over.0.lock().unwrap(), got, "over and under see the same glyphs");
        let x = got[0].pos.x;
        moved |= first.is_some_and(|f: f32| (f - x).abs() > 1.0);
        first.get_or_insert(x);
        // Under, glyphs, over: the follower items sit on either side of the glyphs.
        let kinds: Vec<ItemKind> = list.items.iter().map(|i| i.kind).collect();
        let g = kinds.iter().position(|k| *k == ItemKind::Glyphs).unwrap();
        assert_eq!(kinds[g - 1], ItemKind::Gpu, "frame {n}: {kinds:?}");
        assert_eq!(kinds[g + 1], ItemKind::Gpu, "frame {n}: {kinds:?}");
    }
    assert!(moved, "the centred line slides, and the follower sees it slide");
}
