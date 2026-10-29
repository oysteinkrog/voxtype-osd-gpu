//! Materials that read the text field and the word table (EG-31).
//!
//! A full-size panel under one live word has a probe material that writes, as alpha, what a
//! text input reads at each pixel: `text_dist`, the distance `word_near` finds, or the
//! nearest word's age. Alpha is linear in the readback, so 64 px of distance is 255.
//!
//! GPU test: `just test-gpu text_field`.

mod support;

use vo_core::words::WordModel;
use vo_render::text_field::{TextField, WordTable, SETTLED_AGE};
use vo_render::Offscreen;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::material::{Material, CUSTOM_FIRST};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 240.0;
const H: f32 = 120.0;
const PROBE: u32 = CUSTOM_FIRST;
/// Pixels of distance per unit of alpha.
const RANGE: f32 = 64.0;

/// `p[0].x` picks what to write: 0 `text_dist`, 1 `word_near` distance, 2 its age (seconds,
/// 64 s full scale).
const PROBE_WGSL: &str = "
fn mat_probe (m: Material, i: MatIn) -> vec4f {
  var v = 0.0;
  let mode = m.p[0].x;
  if (mode < 0.5) {
    v = text_dist (i.px).x;
  } else if (mode < 1.5) {
    v = word_near (i.px).dist;
  } else {
    v = word_near (i.px).age;
  }
  return vec4f (0.0, 0.0, 0.0, clamp (v / 64.0, 0.0, 1.0) * i.cov);
}
";

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

/// One word, "I", at 60 px, on a panel with the probe in `mode`, or a clear panel with
/// `None`.
fn list(fonts: &FontRegistry, scale: f32, mode: Option<f32>, t: f64) -> DisplayList {
    let mut model = WordModel::new();
    model.update("I");
    let mut root = Element::panel("root").size(W, H).pad_edges(30.0, 0.0, 0.0, 100.0);
    root = match mode {
        Some(m) => root.fill(Color::WHITE).material(Material::custom(PROBE).reads_text().param(0, [m, 0.0, 0.0, 0.0])),
        None => root.fill(Color::TRANSPARENT),
    };
    let root = root.child(Element::live_text("live", model.words()).text_style(TextStyle::new(0, 60.0, 70.0)));
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), scale);
    scene.apply(root, 0.0);
    scene.sample(t)
}

struct Rig {
    off: Offscreen,
    field: TextField,
}

impl Rig {
    fn new(gpu: &support::Gpu, fonts: &FontRegistry, light: bool) -> Rig {
        let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
        off.renderer.add_material(PROBE, "mat_probe", PROBE_WGSL);
        off.renderer.set_light(light);
        let field = TextField::new(&gpu.device, &gpu.queue);
        Rig { off, field }
    }

    /// Builds the field if it must, binds it with `table` (or the field's own words), and
    /// draws `list`. Returns the pixels and whether the field was built.
    fn frame(&mut self, list: &DisplayList, scale: f32, t: f64, reduced: bool) -> (Vec<u8>, bool) {
        let size = [(list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32];
        let device = self.off.renderer.device().clone();
        let mut enc = device.create_command_encoder(&Default::default());
        let built = self.field.update_words(&mut self.off.renderer, list, size, scale, t, &mut enc);
        self.off.renderer.queue().submit([enc.finish()]);
        let table = self.field.word_table(t, reduced);
        self.field.bind(&mut self.off.renderer, &table);
        (self.off.render_to_rgba(list, size, scale), built)
    }
}

fn alpha(px: &[u8], w: u32, x: u32, y: u32) -> f32 {
    px[((y * w + x) * 4 + 3) as usize] as f32
}

/// The x where the ink's alpha falls through half, scanning right along row `y`.
fn ink_right_edge(px: &[u8], w: u32, y: u32) -> f32 {
    let mut last = None;
    for x in 1..w {
        let (a, b) = (alpha(px, w, x - 1, y), alpha(px, w, x, y));
        if a >= 127.5 && b < 127.5 {
            last = Some(x as f32 - 0.5 + (a - 127.5) / (a - b));
        }
    }
    last.expect("ink on the row")
}

/// `text_dist` 10 px right of the ink's right edge reads 10 px, within 1.5.
fn check_dist(scale: f32, light: bool) {
    let gpu = support::gpu();
    let fonts = fonts();
    let mut rig = Rig::new(&gpu, &fonts, light);
    let w = (W * scale).ceil() as u32;
    let plain = rig.off.render_to_rgba(&list(&fonts, scale, None, 2.0), [w, (H * scale).ceil() as u32], scale);
    // Mid height of the "I": its cap height is about 0.7 em below the line top.
    let y = ((30.0 + 35.0) * scale) as u32;
    let edge = ink_right_edge(&plain, w, y);
    let (px, built) = rig.frame(&list(&fonts, scale, Some(0.0), 2.0), scale, 2.0, false);
    assert!(built, "the first frame builds the field");
    for d in [10.0f32, 20.0] {
        // Pixel centres: the probe at pixel x reads x + 0.5.
        let x = (edge + d - 0.5).round() as u32;
        let want = x as f32 + 0.5 - edge;
        let got = alpha(&px, w, x, y) / 255.0 * RANGE;
        assert!(
            (got - want).abs() <= 1.5,
            "scale {scale} light {light}: text_dist {d} px right of the ink reads {got:.2}, want {want:.2}"
        );
    }
}

#[test]
fn text_dist_reads_pixels_from_the_ink() {
    check_dist(1.0, false);
    check_dist(1.5, false);
    check_dist(1.0, true);
}

/// The GPU's `word_near` distance matches the table's, which the field's word boxes fill.
#[test]
fn word_near_matches_the_table() {
    let gpu = support::gpu();
    let fonts = fonts();
    for scale in [1.0f32, 1.5] {
        let mut rig = Rig::new(&gpu, &fonts, false);
        let l = list(&fonts, scale, Some(1.0), 2.0);
        let (px, _) = rig.frame(&l, scale, 2.0, false);
        let table: WordTable = rig.field.word_table(2.0, false);
        assert_eq!(table.len(), 1, "one word in the table");
        let w = (W * scale).ceil() as u32;
        for (x, y) in [(20u32, 20u32), (150, 60), (200, 100), (100, 5)] {
            let (x, y) = ((x as f32 * scale) as u32, (y as f32 * scale) as u32);
            let want = table.word_near([x as f32 + 0.5, y as f32 + 0.5]).unwrap().dist.min(RANGE);
            let got = alpha(&px, w, x, y) / 255.0 * RANGE;
            assert!((got - want).abs() <= 0.5, "scale {scale} at ({x}, {y}): word_near {got:.2}, table {want:.2}");
        }
    }
}

/// A word's age counts from the frame it first showed; under reduced motion it is settled.
#[test]
fn ages_count_from_first_sight_and_settle_under_reduced_motion() {
    let gpu = support::gpu();
    let fonts = fonts();
    let mut rig = Rig::new(&gpu, &fonts, false);
    rig.frame(&list(&fonts, 1.0, Some(2.0), 2.0), 1.0, 2.0, false);
    let (px, _) = rig.frame(&list(&fonts, 1.0, Some(2.0), 2.0), 1.0, 34.0, false);
    let got = alpha(&px, 240, 20, 20) / 255.0 * RANGE;
    assert!((got - 32.0).abs() <= 0.5, "age 32 s reads {got:.2}");
    assert_eq!(rig.field.word_table(34.0, true).words()[0].age, SETTLED_AGE);
    let (px, _) = rig.frame(&list(&fonts, 1.0, Some(2.0), 2.0), 1.0, 34.0, true);
    assert_eq!(alpha(&px, 240, 20, 20), 255.0, "settled reads as long ago");
}

/// Settled text builds the field once in 60 frames; a list whose materials read no text
/// never builds it.
#[test]
fn settled_text_builds_the_field_once() {
    let gpu = support::gpu();
    let fonts = fonts();
    let mut rig = Rig::new(&gpu, &fonts, false);
    for i in 0..60 {
        let t = 2.0 + i as f64 / 60.0;
        rig.frame(&list(&fonts, 1.0, Some(0.0), t), 1.0, t, false);
    }
    assert_eq!(rig.field.builds(), 1, "built once over 60 settled frames");
    let mut plain = Rig::new(&gpu, &fonts, false);
    for i in 0..5 {
        let (_, built) = plain.frame(&list(&fonts, 1.0, None, 2.0 + i as f64), 1.0, 2.0, false);
        assert!(!built);
    }
    assert_eq!(plain.field.builds(), 0, "no material reads text, no field");
    assert_eq!(plain.field.memory_bytes(), 0);
    // 600x300 at scale 1: a 300x150 field.
    let mut big = TextField::new(&gpu.device, &gpu.queue);
    let mut l = list(&fonts, 1.0, Some(0.0), 2.0);
    l.size = Size::new(600.0, 300.0);
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    assert!(big.update_words(&mut rig.off.renderer, &l, [600, 300], 1.0, 2.0, &mut enc));
    gpu.queue.submit([enc.finish()]);
    assert_eq!(big.size(), Some([300, 150]));
    println!("text field memory at 600x300: {} KiB", big.memory_bytes() / 1024);
}
