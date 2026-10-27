//! A reference image over the stage, to match a theme to its mock-up: drawn over or under the
//! stage at an opacity, as a split wipe, or as the difference between the two. The same
//! comparison runs headless (`--compare`) and prints how far the frame is from the mock-up.
//!
//! The reference is a PNG, usually a mock-up of the whole desktop, so a crop rectangle picks
//! the part that shows the popup. The crop is resampled to the stage's pixel size, so one
//! reference pixel lies under one stage pixel. Where the stage is transparent the comparison
//! needs a backdrop: the reference itself (the default: only what the stage draws counts) or
//! a flat colour.

use std::path::{Path, PathBuf};

/// How the reference shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefMode {
    /// Loaded, not shown.
    Off,
    /// Over the stage at `opacity`.
    #[default]
    Over,
    /// Under the stage at `opacity`, so the stage draws on top of the mock-up.
    Under,
    /// `|stage over backdrop - reference|` per channel, brightened.
    Diff,
    /// The stage left of `split`, the reference right of it.
    Split,
}

impl RefMode {
    pub const ALL: [RefMode; 5] = [RefMode::Off, RefMode::Over, RefMode::Under, RefMode::Diff, RefMode::Split];

    pub fn name(self) -> &'static str {
        match self {
            RefMode::Off => "off",
            RefMode::Over => "over",
            RefMode::Under => "under",
            RefMode::Diff => "diff",
            RefMode::Split => "split",
        }
    }

    pub fn parse(s: &str) -> Option<RefMode> {
        RefMode::ALL.into_iter().find(|m| m.name() == s)
    }

    /// The next mode for the window's button, skipping `Off` after the last.
    pub fn next(self) -> RefMode {
        match self {
            RefMode::Off => RefMode::Over,
            RefMode::Over => RefMode::Under,
            RefMode::Under => RefMode::Diff,
            RefMode::Diff => RefMode::Split,
            RefMode::Split => RefMode::Off,
        }
    }
}

/// What the stage is composited over before it is compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backdrop {
    /// The reference pixel under it.
    #[default]
    Reference,
    /// A flat sRGB colour.
    Color([u8; 3]),
}

impl Backdrop {
    /// `ref` or `#rrggbb`.
    pub fn parse(s: &str) -> Option<Backdrop> {
        if s == "ref" {
            return Some(Backdrop::Reference);
        }
        let hex = s.strip_prefix('#').filter(|h| h.len() == 6)?;
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(Backdrop::Color([(v >> 16) as u8, (v >> 8) as u8, v as u8]))
    }
}

/// The reference settings, kept in the window's state across restarts.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RefState {
    pub path: PathBuf,
    /// The part of the image to use, in its pixels: x, y, w, h. The whole image when unset.
    pub crop: Option<[f32; 4]>,
    pub mode: RefMode,
    /// For `Over` and `Under`.
    pub opacity: f32,
    /// For `Split`: where the wipe is, 0 to 1 across the stage.
    pub split: f32,
    pub backdrop: Backdrop,
}

impl Default for RefState {
    fn default() -> RefState {
        RefState {
            path: PathBuf::new(),
            crop: None,
            mode: RefMode::Over,
            opacity: 0.5,
            split: 0.5,
            backdrop: Backdrop::Reference,
        }
    }
}

/// `x,y,w,h`, or `x,y,w` with the height from `aspect` (width over height).
pub fn parse_crop(s: &str, aspect: f32) -> Result<[f32; 4], String> {
    let v: Vec<f32> = s
        .split([',', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<f32>().map_err(|e| format!("crop '{s}': {e}")))
        .collect::<Result<_, _>>()?;
    match v.as_slice() {
        [x, y, w, h] if *w > 0.0 && *h > 0.0 => Ok([*x, *y, *w, *h]),
        [x, y, w] if *w > 0.0 => Ok([*x, *y, *w, *w / aspect.max(1e-6)]),
        _ => Err(format!("crop '{s}': give x,y,w,h or x,y,w in image pixels")),
    }
}

/// An image in memory: RGBA8, straight alpha, sRGB encoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn load(path: &Path) -> Result<Image, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let mut dec = png::Decoder::new(std::io::BufReader::new(file));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().map_err(|e| format!("{}: {e}", path.display()))?;
        let mut buf = vec![0; reader.output_buffer_size().ok_or("image too large")?];
        let info = reader.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        let px = (info.width * info.height) as usize;
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf[..px * 4].to_vec(),
            png::ColorType::Rgb => buf[..px * 3].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
            png::ColorType::GrayscaleAlpha => buf[..px * 2].chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
            png::ColorType::Grayscale => buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => return Err(format!("{}: palette not expanded", path.display())),
        };
        Ok(Image { width: info.width, height: info.height, rgba })
    }

    /// The `crop` rectangle (the whole image when `None`) resampled to `w` x `h` with a box
    /// filter in linear light, as straight-alpha sRGB. Parts of the crop outside the image
    /// are transparent.
    pub fn resample(&self, crop: Option<[f32; 4]>, w: u32, h: u32) -> Image {
        let [cx, cy, cw, ch] = crop.unwrap_or([0.0, 0.0, self.width as f32, self.height as f32]);
        let (sx, sy) = (cw / w as f32, ch / h as f32);
        // Samples per output pixel and axis: enough to cover the source pixels under it.
        let (nx, ny) = (sx.ceil().clamp(1.0, 8.0) as u32, sy.ceil().clamp(1.0, 8.0) as u32);
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f32; 4];
                for j in 0..ny {
                    for i in 0..nx {
                        let u = cx + (x as f32 + (i as f32 + 0.5) / nx as f32) * sx;
                        let v = cy + (y as f32 + (j as f32 + 0.5) / ny as f32) * sy;
                        let p = self.bilinear(u, v);
                        for c in 0..4 {
                            acc[c] += p[c];
                        }
                    }
                }
                let n = (nx * ny) as f32;
                let a = acc[3] / n;
                let px = if a > 0.0 {
                    [
                        to_srgb(acc[0] / n / a),
                        to_srgb(acc[1] / n / a),
                        to_srgb(acc[2] / n / a),
                        (a * 255.0).round() as u8,
                    ]
                } else {
                    [0; 4]
                };
                out.extend_from_slice(&px);
            }
        }
        Image { width: w, height: h, rgba: out }
    }

    /// Premultiplied linear colour at image position `(u, v)`, pixel centres at `+0.5`.
    fn bilinear(&self, u: f32, v: f32) -> [f32; 4] {
        let (x, y) = (u - 0.5, v - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let mut acc = [0.0f32; 4];
        for (dx, dy, wgt) in
            [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)]
        {
            let p = self.premul_linear(x0 as i64 + dx, y0 as i64 + dy);
            for c in 0..4 {
                acc[c] += p[c] * wgt;
            }
        }
        acc
    }

    fn premul_linear(&self, x: i64, y: i64) -> [f32; 4] {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return [0.0; 4];
        }
        let i = ((y as u32 * self.width + x as u32) * 4) as usize;
        let a = self.rgba[i + 3] as f32 / 255.0;
        [lin(self.rgba[i]) * a, lin(self.rgba[i + 1]) * a, lin(self.rgba[i + 2]) * a, a]
    }

    /// Premultiplied sRGB-encoded bytes, as the GPU texture and `write_png` take them.
    pub fn premultiplied(&self) -> Vec<u8> {
        self.rgba
            .chunks(4)
            .flat_map(|p| {
                let a = p[3] as f32 / 255.0;
                [to_srgb(lin(p[0]) * a), to_srgb(lin(p[1]) * a), to_srgb(lin(p[2]) * a), p[3]]
            })
            .collect()
    }
}

fn lin(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let c = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round() as u8
}

/// How far a frame is from the reference, over all pixels, in sRGB-encoded values 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffStats {
    /// Mean over pixels of the largest channel difference.
    pub mean: f64,
    /// The 95th percentile of the same.
    pub p95: f64,
    /// Share of pixels whose largest channel difference is over 10%.
    pub over_10: f64,
}

/// The stage (premultiplied sRGB bytes, as read back) over the backdrop, as opaque sRGB.
pub fn flatten(stage: &[u8], reference: &Image, backdrop: Backdrop) -> Vec<u8> {
    stage
        .chunks(4)
        .zip(reference.rgba.chunks(4))
        .flat_map(|(s, r)| {
            let a = s[3] as f32 / 255.0;
            let bg = match backdrop {
                Backdrop::Reference => [r[0], r[1], r[2]],
                Backdrop::Color(c) => c,
            };
            let mut out = [0u8, 0, 0, 255];
            for c in 0..3 {
                out[c] = to_srgb(lin(s[c]) + (1.0 - a) * lin(bg[c]));
            }
            out
        })
        .collect()
}

/// Compares two opaque sRGB images of one size and returns the stats and a heat map: black
/// where they match, through red to yellow and white as the difference grows (4x gain).
pub fn diff(a: &[u8], b: &[u8]) -> (DiffStats, Vec<u8>) {
    let mut d: Vec<f64> = Vec::with_capacity(a.len() / 4);
    let mut heat = Vec::with_capacity(a.len());
    for (p, q) in a.chunks(4).zip(b.chunks(4)) {
        let m = (0..3).map(|c| (p[c] as i32 - q[c] as i32).unsigned_abs()).max().unwrap_or(0) as f64 / 255.0;
        d.push(m);
        let g = (m * 4.0).min(1.0) as f32;
        let ramp = |lo: f32| ((g - lo) / (1.0 / 3.0)).clamp(0.0, 1.0);
        heat.extend_from_slice(&[to_srgb(ramp(0.0)), to_srgb(ramp(1.0 / 3.0)), to_srgb(ramp(2.0 / 3.0)), 255]);
    }
    let n = d.len().max(1) as f64;
    let mean = d.iter().sum::<f64>() / n;
    let over_10 = d.iter().filter(|v| **v > 0.1).count() as f64 / n;
    d.sort_by(f64::total_cmp);
    let p95 = d.get(((d.len() as f64 * 0.95) as usize).min(d.len().saturating_sub(1))).copied().unwrap_or(0.0);
    (DiffStats { mean, p95, over_10 }, heat)
}

/// Three panels side by side with a gap: frame, reference, heat map. All opaque, one size.
pub fn triptych(panels: [&[u8]; 3], w: u32, h: u32) -> Image {
    const GAP: u32 = 8;
    let tw = 3 * w + 2 * GAP;
    let mut rgba: Vec<u8> = [24u8, 26, 30, 255].iter().copied().cycle().take((tw * h * 4) as usize).collect();
    for (k, p) in panels.iter().enumerate() {
        let ox = k as u32 * (w + GAP);
        for y in 0..h {
            let src = &p[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
            let at = ((y * tw + ox) * 4) as usize;
            rgba[at..at + src.len()].copy_from_slice(src);
        }
    }
    Image { width: tw, height: h, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> Image {
        Image { width: w, height: h, rgba: c.iter().copied().cycle().take((w * h * 4) as usize).collect() }
    }

    #[test]
    fn modes_backdrops_and_crops_parse() {
        assert_eq!(RefMode::parse("diff"), Some(RefMode::Diff));
        assert_eq!(RefMode::parse("nope"), None);
        let mut m = RefMode::Off;
        for _ in 0..RefMode::ALL.len() {
            m = m.next();
        }
        assert_eq!(m, RefMode::Off, "the button cycles through every mode");
        assert_eq!(Backdrop::parse("ref"), Some(Backdrop::Reference));
        assert_eq!(Backdrop::parse("#102030"), Some(Backdrop::Color([0x10, 0x20, 0x30])));
        assert_eq!(Backdrop::parse("#12"), None);
        assert_eq!(parse_crop("10,20,300,150", 2.0), Ok([10.0, 20.0, 300.0, 150.0]));
        assert_eq!(parse_crop("10 20 300", 2.0), Ok([10.0, 20.0, 300.0, 150.0]));
        assert!(parse_crop("1,2", 2.0).is_err());
    }

    #[test]
    fn resampling_keeps_flat_colour_and_halves_by_averaging() {
        let flat = solid(40, 20, [200, 100, 50, 255]);
        let r = flat.resample(Some([4.0, 2.0, 20.0, 10.0]), 7, 3);
        assert!(r.rgba.chunks(4).all(|p| p == [200, 100, 50, 255]), "{:?}", &r.rgba[..8]);
        // Black and white columns average to mid grey in linear light, not 128.
        let mut stripes = solid(8, 2, [0, 0, 0, 255]);
        for (i, p) in stripes.rgba.chunks_mut(4).enumerate() {
            if i % 2 == 1 {
                p[..3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let half = stripes.resample(None, 4, 1);
        assert!(half.rgba.chunks(4).all(|p| (186..=189).contains(&p[0])), "{:?}", half.rgba);
        // Outside the image is transparent.
        let out = flat.resample(Some([-40.0, 0.0, 40.0, 20.0]), 4, 2);
        assert!(out.rgba.chunks(4).all(|p| p[3] == 0));
    }

    #[test]
    fn identical_frames_score_zero_and_a_changed_patch_shows() {
        let reference = solid(10, 10, [30, 60, 90, 255]);
        // A transparent stage over the reference is the reference.
        let clear = vec![0u8; 400];
        let flat = flatten(&clear, &reference, Backdrop::Reference);
        let (s, heat) = diff(&flat, &reference.rgba);
        assert_eq!(s.mean, 0.0);
        assert!(heat.chunks(4).all(|p| p == [0, 0, 0, 255]));
        // Over black, the same transparent stage is far off everywhere.
        let (s, _) = diff(&flatten(&clear, &reference, Backdrop::Color([0, 0, 0])), &reference.rgba);
        assert!((s.mean - 90.0 / 255.0).abs() < 1e-9 && s.over_10 == 1.0);
        // A white opaque 2x5 patch: 10% of the pixels, each off by (255 - 30) / 255.
        let mut stage = vec![0u8; 400];
        for y in 0..5 {
            for x in 0..2 {
                let i = (y * 10 + x) * 4;
                stage[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let (s, heat) = diff(&flatten(&stage, &reference, Backdrop::Reference), &reference.rgba);
        assert!((s.over_10 - 0.1).abs() < 1e-9 && (s.mean - 0.1 * 225.0 / 255.0).abs() < 1e-9);
        assert_eq!(&heat[..4], &[255, 255, 255, 255]);
        assert_eq!(&heat[8 * 4..9 * 4], &[0, 0, 0, 255]);
    }

    #[test]
    fn triptych_lays_panels_side_by_side() {
        let (a, b, c) = (vec![10u8; 16], vec![20u8; 16], vec![30u8; 16]);
        let t = triptych([&a, &b, &c], 2, 2);
        assert_eq!((t.width, t.height), (22, 2));
        assert_eq!(t.rgba[0], 10);
        assert_eq!(t.rgba[(10 * 4) as usize], 20);
        assert_eq!(t.rgba[(20 * 4) as usize], 30);
        assert_eq!(t.rgba[(3 * 4) as usize], 24, "the gap");
    }
}
