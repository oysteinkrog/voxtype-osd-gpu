//! Contact sheets and golden checks from the command line: N frames across a range, each drawn
//! at thumbnail scale (vector content stays sharp; nothing is downsampled), in a grid with the
//! frame time under each one.

use std::path::{Path, PathBuf};

use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Offscreen, Target};
use vo_scene::element::{Element, TextStyle};
use vo_scene::geom::{Color, Size};
use vo_scene::{taffy, FontRegistry, Scene};

use super::composite::read_back;
use super::source::Source;
use crate::offline::{headless_gpu, OfflineError};

#[derive(Clone, Debug)]
pub struct SheetSpec {
    pub from: f64,
    pub to: f64,
    pub frames: usize,
    pub cols: usize,
    /// Width of one thumbnail in pixels.
    pub thumb_w: u32,
    pub fps: f64,
}

/// The frame times of a sheet: `frames` times from `from` to `to` inclusive, on the frame grid.
pub fn sheet_times(spec: &SheetSpec) -> Vec<f64> {
    let n = spec.frames.max(1);
    (0..n)
        .map(|i| {
            let t = if n == 1 { spec.from } else { spec.from + (spec.to - spec.from) * i as f64 / (n - 1) as f64 };
            (t * spec.fps).round() / spec.fps
        })
        .collect()
}

const GAP: u32 = 8;
const LABEL_H: u32 = 20;
/// Sheet background, sRGB.
const BG: [u8; 4] = [24, 26, 30, 255];

/// A rendered sheet: RGBA8, premultiplied, sRGB encoded, as `write_png` takes it.
pub struct Sheet {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Renders the sheet, with the frame time under each thumbnail when a monospace font is found.
pub(crate) fn render_sheet(source: &mut dyn Source, spec: &SheetSpec) -> Result<Sheet, OfflineError> {
    let (device, queue) = headless_gpu()?;
    let fonts: FontRegistry = source.fonts();
    let label_font = fonts.add_system("monospace").ok();
    let mut off = Offscreen::with_fonts(&device, &queue, fonts.clone());
    source.configure(&mut off.renderer);
    let size = source.size();
    let scale = spec.thumb_w as f32 / size.w;
    let (tw, th) = (spec.thumb_w, (size.h * scale).round().max(1.0) as u32);
    let times = sheet_times(spec);
    let cols = spec.cols.clamp(1, times.len()) as u32;
    let rows = (times.len() as u32).div_ceil(cols);
    let (w, h) = (cols * tw + (cols + 1) * GAP, rows * (th + LABEL_H) + (rows + 1) * GAP);
    let mut rgba: Vec<u8> = BG.iter().copied().cycle().take((w * h * 4) as usize).collect();
    let mut target = Target::new(&device, tw, th, FORMAT);
    for (i, t) in times.iter().enumerate() {
        source.draw(*t, scale, &mut off.renderer, &mut target);
        let thumb = read_back(&device, &queue, target.texture());
        let (ox, oy) = (GAP + (i as u32 % cols) * (tw + GAP), GAP + (i as u32 / cols) * (th + LABEL_H + GAP));
        over(&mut rgba, w, &thumb, tw, th, ox, oy);
    }
    if let Some(font) = label_font {
        // Frame times under each thumbnail, drawn by the framework in one list.
        let mut scene: Scene<()> = Scene::new().with_text(Box::new(vo_scene::text::ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, Size::new(w as f32, h as f32), 1.0);
        let labels = times.iter().enumerate().map(|(i, t)| {
            let (ox, oy) = (GAP + (i as u32 % cols) * (tw + GAP), GAP + (i as u32 / cols) * (th + LABEL_H + GAP) + th);
            let frame = (t * spec.fps).round() as i64;
            Element::label(format!("l{i}"), format!("{t:.3} s  f {frame}"))
                .text_style(TextStyle::new(font, 12.0, 16.0))
                .color(Color::hex(0xc8ccd4, 1.0))
                .layout(|s| {
                    use taffy::style::{LengthPercentageAuto as L, Position};
                    s.position = Position::Absolute;
                    s.inset = taffy::Rect {
                        left: L::length(ox as f32 + 2.0),
                        top: L::length(oy as f32 + 3.0),
                        right: L::auto(),
                        bottom: L::auto(),
                    };
                })
        });
        scene.apply(Element::panel("root").size(w as f32, h as f32).children(labels), 0.0);
        let labels = off.render_to_rgba(&scene.sample(0.0), [w, h], 1.0);
        over(&mut rgba, w, &labels, w, h, 0, 0);
    }
    Ok(Sheet { width: w, height: h, rgba })
}

/// Premultiplied over in sRGB-encoded values: close enough for opaque thumbnails and text on
/// the flat background.
fn over(dst: &mut [u8], dst_w: u32, src: &[u8], w: u32, h: u32, ox: u32, oy: u32) {
    for y in 0..h {
        for x in 0..w {
            let s = ((y * w + x) * 4) as usize;
            let d = (((oy + y) * dst_w + ox + x) * 4) as usize;
            let a = src[s + 3] as u32;
            for c in 0..4 {
                dst[d + c] = (src[s + c] as u32 + dst[d + c] as u32 * (255 - a) / 255).min(255) as u8;
            }
        }
    }
}

pub fn write_sheet(sheet: &Sheet, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    write_png(path, sheet.width, sheet.height, &sheet.rgba)
}

/// The result of a golden check.
#[derive(Debug)]
pub struct GoldenReport {
    pub differing: usize,
    pub total: usize,
    pub diff: PathBuf,
}

/// Compares `sheet` with the PNG at `golden`: a pixel differs when a channel is more than 2
/// levels off, and the check fails when more than 0.05 % of pixels differ (the rule the GPU
/// golden tests use). Writes a diff image next to `out`: differences in red, 8 times brighter.
pub fn check_golden(sheet: &Sheet, golden: &Path, out: &Path) -> Result<GoldenReport, String> {
    let file = std::fs::File::open(golden).map_err(|e| format!("cannot open {}: {e}", golden.display()))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("golden too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if (info.width, info.height) != (sheet.width, sheet.height) || info.color_type != png::ColorType::Rgba {
        return Err(format!(
            "golden is {}x{} {:?}, the sheet is {}x{} RGBA",
            info.width, info.height, info.color_type, sheet.width, sheet.height
        ));
    }
    // Compare as straight alpha, as both were written.
    let ours: Vec<u8> =
        sheet.rgba.as_chunks::<4>().0.iter().flat_map(|&p| vo_render::color::unpremultiply_srgb8(p)).collect();
    let mut diff = vec![0u8; ours.len()];
    let mut differing = 0;
    for (i, (a, b)) in ours.as_chunks::<4>().0.iter().zip(buf.as_chunks::<4>().0).enumerate() {
        let d = (0..4).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
        if d > 2 {
            differing += 1;
        }
        diff[i * 4] = d.saturating_mul(8);
        diff[i * 4 + 3] = 255;
    }
    let diff_path = out.with_extension("diff.png");
    write_png(&diff_path, sheet.width, sheet.height, &diff).map_err(|e| e.to_string())?;
    Ok(GoldenReport { differing, total: ours.len() / 4, diff: diff_path })
}

impl GoldenReport {
    pub fn passed(&self) -> bool {
        self.differing as f64 <= self.total as f64 * 0.0005
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_times_cover_the_range_on_the_frame_grid() {
        let spec = SheetSpec { from: 0.0, to: 10.0, frames: 7, cols: 4, thumb_w: 320, fps: 60.0 };
        let t = sheet_times(&spec);
        assert_eq!(t.len(), 7);
        assert_eq!(t[0], 0.0);
        assert_eq!(t[6], 10.0);
        assert!(t.iter().all(|t| ((t * 60.0).round() - t * 60.0).abs() < 1e-9));
        let one = sheet_times(&SheetSpec { frames: 1, from: 2.5, ..spec });
        assert_eq!(one, vec![2.5]);
    }
}
