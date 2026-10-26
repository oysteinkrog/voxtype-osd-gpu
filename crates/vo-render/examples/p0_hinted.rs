//! P0-14: hinted small text from skrifa against KDE's own rendering.
//!
//! Input: a screenshot of the same string drawn by Qt Quick (NativeRendering)
//! at 9, 10, 11 and 12 px in a nested `kwin_wayland --virtual` session, made
//! with the files in /var/tmp/claude/vo-p0/kde/. Output: a contact sheet with
//! four rows per size, top to bottom: KDE, skrifa light autohint (FreeType's
//! "hintslight"), skrifa TrueType interpreter, unhinted. Grayscale coverage,
//! black on white, composited in sRGB space like FreeType and Qt do.
//!
//!   cargo run --release -p vo-render --example p0_hinted

mod common;

use common::*;
use skrifa::outline::{DrawSettings, Engine, HintingInstance, OutlinePen, SmoothMode, Target};
use skrifa::prelude::{LocationRef, Size};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use zeno::{Command, Mask};

const KDE_DIR: &str = "/var/tmp/claude/vo-p0/kde";
const SIZES: [f32; 4] = [9.0, 10.0, 11.0, 12.0];

/// Grayscale image, 0 = white paper, 1 = full ink.
struct Ink {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Ink {
    /// Crops to the ink bounding box.
    fn crop(&self) -> Ink {
        let (mut x0, mut y0, mut x1, mut y1) = (self.w, self.h, 0, 0);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.v[y * self.w + x] > 0.0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x1 <= x0 {
            return Ink { w: 0, h: 0, v: Vec::new() };
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let mut v = vec![0.0; w * h];
        for y in 0..h {
            v[y * w..(y + 1) * w].copy_from_slice(&self.v[(y + y0) * self.w + x0..(y + y0) * self.w + x1]);
        }
        Ink { w, h, v }
    }

    fn total(&self) -> f32 {
        self.v.iter().sum()
    }
}

/// Reads the KDE screenshot, finds the white window and splits it into one
/// ink band per text line.
fn kde_rows() -> Option<Vec<Ink>> {
    let file = std::fs::File::open(format!("{KDE_DIR}/kde-shot.png")).ok()?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file)).read_info().ok()?;
    let mut buf = vec![0u8; dec.output_buffer_size()?];
    let info = dec.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let ch = info.color_type.samples();
    let lum = |x: usize, y: usize| buf[(y * w + x) * ch] as f32 / 255.0;
    // The window is the white area on the black desktop.
    let (mut wx0, mut wy0, mut wx1, mut wy1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if lum(x, y) > 0.99 {
                wx0 = wx0.min(x);
                wy0 = wy0.min(y);
                wx1 = wx1.max(x + 1);
                wy1 = wy1.max(y + 1);
            }
        }
    }
    let win = Ink {
        w: wx1 - wx0,
        h: wy1 - wy0,
        v: (wy0..wy1).flat_map(|y| (wx0..wx1).map(move |x| (y, x))).map(|(y, x)| 1.0 - lum(x, y)).collect(),
    };
    let mut rows = Vec::new();
    let mut y = 0;
    while y < win.h {
        let blank = |y: usize| win.v[y * win.w..(y + 1) * win.w].iter().all(|&v| v == 0.0);
        if blank(y) {
            y += 1;
            continue;
        }
        let start = y;
        while y < win.h && !blank(y) {
            y += 1;
        }
        let band = Ink { w: win.w, h: y - start, v: win.v[start * win.w..y * win.w].to_vec() };
        rows.push(band.crop());
    }
    Some(rows)
}

struct Cmds {
    out: Vec<Command>,
    dx: f32,
    dy: f32,
}

impl OutlinePen for Cmds {
    fn move_to(&mut self, x: f32, y: f32) {
        self.out.push(Command::MoveTo([x + self.dx, self.dy - y].into()));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.out.push(Command::LineTo([x + self.dx, self.dy - y].into()));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.out.push(Command::QuadTo([cx + self.dx, self.dy - cy].into(), [x + self.dx, self.dy - y].into()));
    }
    fn curve_to(&mut self, a: f32, b: f32, c: f32, d: f32, x: f32, y: f32) {
        self.out.push(Command::CurveTo(
            [a + self.dx, self.dy - b].into(),
            [c + self.dx, self.dy - d].into(),
            [x + self.dx, self.dy - y].into(),
        ));
    }
    fn close(&mut self) {
        self.out.push(Command::Close);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    LightAuto,
    Interpreter,
    Unhinted,
}

/// Shapes `text` with harfrust and rasterises it at `px` with zeno. Hinted
/// modes put each glyph origin on a whole pixel, as FreeType without subpixel
/// positioning does.
fn render(font: &FontRef, hb: &harfrust::ShaperFont, text: &str, px: f32, mode: Mode) -> Ink {
    let upem = font.head().unwrap().units_per_em() as f32;
    let mut buf = harfrust::Buffer::new();
    buf.push_str(text);
    buf.guess_segment_properties();
    harfrust::shape(hb, &mut buf, harfrust::ShapeOptions::new()).unwrap();
    let outlines = font.outline_glyphs();
    let target = |mode| Target::Smooth { mode, symmetric_rendering: true, preserve_linear_metrics: false };
    let hinting = match mode {
        Mode::LightAuto => Some(
            HintingInstance::new(&outlines, Size::new(px), LocationRef::default(), skrifa::outline::HintingOptions {
                engine: Engine::Auto(None),
                target: target(SmoothMode::Light),
            })
            .unwrap(),
        ),
        Mode::Interpreter => Some(
            HintingInstance::new(&outlines, Size::new(px), LocationRef::default(), skrifa::outline::HintingOptions {
                engine: Engine::Interpreter,
                target: target(SmoothMode::Normal),
            })
            .unwrap(),
        ),
        Mode::Unhinted => None,
    };
    let (w, h) = ((text.len() as f32 * px) as usize + 20, (px * 2.0) as usize + 8);
    let baseline = (px * 1.5).round();
    let mut pen = Cmds { out: Vec::new(), dx: 0.0, dy: baseline };
    let mut x = 4.0f32;
    for (info, pos) in buf.glyph_infos().iter().zip(buf.glyph_positions()) {
        let gid = GlyphId::new(info.glyph_id);
        pen.dx = if mode == Mode::Unhinted { x } else { x.round() };
        if let Some(g) = outlines.get(gid) {
            let settings = match &hinting {
                Some(hi) => DrawSettings::hinted(hi, false),
                None => DrawSettings::unhinted(Size::new(px), LocationRef::default()),
            };
            let _ = g.draw(settings, &mut pen);
        }
        x += pos.x_advance as f32 * px / upem;
    }
    let mut mask = vec![0u8; w * h];
    Mask::new(&pen.out[..]).size(w as u32, h as u32).render_into(&mut mask, None);
    Ink { w, h, v: mask.iter().map(|&m| m as f32 / 255.0).collect() }.crop()
}

/// Mean and max absolute difference over the union of both ink boxes, with
/// both images aligned at their top-left ink corner.
fn diff(a: &Ink, b: &Ink) -> (f32, f32) {
    let (w, h) = (a.w.max(b.w), a.h.max(b.h));
    let at = |i: &Ink, x: usize, y: usize| if x < i.w && y < i.h { i.v[y * i.w + x] } else { 0.0 };
    let (mut sum, mut max) = (0.0f32, 0.0f32);
    for y in 0..h {
        for x in 0..w {
            let d = (at(a, x, y) - at(b, x, y)).abs();
            sum += d;
            max = max.max(d);
        }
    }
    (sum / (w * h) as f32, max)
}

fn main() {
    let text = std::fs::read_to_string(format!("{KDE_DIR}/text.txt")).expect("run the nested KDE capture first");
    let path = ui_font_path();
    let data = std::fs::read(&path).unwrap();
    let font = FontRef::new(&data).unwrap();
    let hfont = harfrust::Font::new(data.clone(), 0).unwrap();
    let hb = harfrust::ShaperFont::new(&hfont).with_scale(font.head().unwrap().units_per_em() as i32);
    let kde = kde_rows().expect("KDE screenshot");
    assert_eq!(kde.len(), 4, "expected 4 text bands in the KDE shot, found {}", kde.len());
    println!("font {}", path.display());
    println!("size_px row ink_w ink_h ink_sum ink_sum_vs_kde mean_abs_diff_vs_kde max_abs_diff_vs_kde");

    let modes = [Mode::LightAuto, Mode::Interpreter, Mode::Unhinted];
    let names = ["kde", "skrifa_light_auto", "skrifa_interpreter", "unhinted"];
    let mut rows: Vec<Ink> = Vec::new();
    for (si, &px) in SIZES.iter().enumerate() {
        let k = &kde[si];
        let mut set = vec![Ink { w: k.w, h: k.h, v: k.v.clone() }];
        for m in modes {
            set.push(render(&font, &hb, &text, px, m));
        }
        for (name, ink) in names.iter().zip(&set) {
            let (mean, max) = diff(k, ink);
            println!(
                "{px:>4} {name:<20} {:>5} {:>3} {:>8.1} {:>6.3} {mean:>8.4} {max:>6.3}",
                ink.w,
                ink.h,
                ink.total(),
                ink.total() / k.total()
            );
        }
        rows.extend(set);
    }

    // Sheet: rows stacked with a gap; size groups separated by a wider gap.
    let gap = 4;
    let width = rows.iter().map(|r| r.w).max().unwrap() + 16;
    let height: usize = rows.iter().map(|r| r.h + gap).sum::<usize>() + SIZES.len() * 10 + 16;
    let mut img = vec![255u8; width * height * 3];
    let mut y = 8;
    for (i, r) in rows.iter().enumerate() {
        for ry in 0..r.h {
            for rx in 0..r.w {
                let v = (255.0 * (1.0 - r.v[ry * r.w + rx])).round() as u8;
                let o = ((y + ry) * width + 8 + rx) * 3;
                img[o..o + 3].copy_from_slice(&[v; 3]);
            }
        }
        y += r.h + gap;
        if i % 4 == 3 {
            y += 10;
        }
    }
    let out1 = format!("{OUT_DIR}/hinted-vs-kde-1x.png");
    write_png_rgb(&out1, width as u32, height as u32, &img);
    let z = 4;
    let half = width.min(260);
    let mut big = vec![0u8; half * z * height * z * 3];
    for yy in 0..height * z {
        for xx in 0..half * z {
            let o = ((yy / z) * width + xx / z) * 3;
            let d = (yy * half * z + xx) * 3;
            big[d..d + 3].copy_from_slice(&img[o..o + 3]);
        }
    }
    let out4 = format!("{OUT_DIR}/hinted-vs-kde-4x.png");
    write_png_rgb(&out4, (half * z) as u32, (height * z) as u32, &big);
    println!("sheets: {out1} {out4}");
}
