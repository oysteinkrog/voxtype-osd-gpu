//! Images (EG-24): PNG and full-colour SVG files drawn in a box, such as an app's icon.
//!
//! `Element::image(key, source)` reserves a box like any element and shows the image in it,
//! centred and scaled to fit (its aspect kept). Under the hood it is a GPU element
//! (`crate::gpu`) whose source is an [`ImageSource`]; `vo_render` draws it with its built-in
//! image painter, so no app code is needed.
//!
//! **Rasters.** An image is decoded and scaled once per pixel size: the box's logical size
//! times the surface's pixels per unit. [`ImageCache`] keeps the results, keyed by
//! (source, pixel size), least recently used first out, at most [`CACHE_BYTES`] of pixels.
//! A raster is straight-alpha sRGB RGBA, 8 bits per channel; scaling a PNG averages in linear
//! light with premultiplied alpha, so edges do not darken. An SVG is drawn by resvg straight at
//! the pixel size. A source that fails to decode is remembered, so it fails once, not once a
//! frame; the caller logs it.
//!
//! **Where decoding happens.** The GPU painter decodes on first use, on the render thread.
//! A caller that wants the first frame to be quick calls [`ImageCache::raster`] from its own
//! thread ahead of time ([`ImageSource::warm`]). The CPU first frame (`vo_render::cpu`)
//! draws an image only when its raster is already in the cache, and skips it otherwise.
//!
//! The drawing never changes with time, so an image never keeps the scene awake.
//!
//! Not done: SVG text (no fonts are loaded) and raster images inside an SVG; other formats
//! than PNG and SVG.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::element::{Element, Key};
use crate::geom::Rect;
use crate::gpu::GpuSource;

/// The most pixel bytes [`ImageCache::global`] holds: 8 MB.
pub const CACHE_BYTES: usize = 8 << 20;

/// The largest raster side in pixels. Bigger requests are scaled down to fit.
pub const MAX_SIDE: u32 = 4096;

/// Where an image's bytes come from.
#[derive(Clone, Debug)]
pub enum ImageData {
    /// PNG or SVG file contents (told apart by the PNG signature).
    Bytes(Arc<[u8]>),
    /// A file, read when first drawn: `.svg` and `.svgz` as SVG, anything else by content.
    File(PathBuf),
    /// Straight-alpha sRGB RGBA pixels, `w` by `h`.
    Rgba { w: u32, h: u32, px: Arc<[u8]> },
}

/// An image to draw. Make one per image and keep the `Arc`: the element compares sources
/// by identity, and the cache by [`ImageSource::id`].
#[derive(Debug)]
pub struct ImageSource {
    id: u64,
    data: ImageData,
}

impl ImageSource {
    /// PNG or SVG file contents.
    pub fn bytes(bytes: impl Into<Arc<[u8]>>) -> Arc<ImageSource> {
        let bytes: Arc<[u8]> = bytes.into();
        Arc::new(ImageSource { id: fnv(&bytes, 1), data: ImageData::Bytes(bytes) })
    }

    /// A PNG or SVG file, read when first needed.
    pub fn file(path: impl Into<PathBuf>) -> Arc<ImageSource> {
        let path: PathBuf = path.into();
        let id = fnv(path.as_os_str().as_encoded_bytes(), 2);
        Arc::new(ImageSource { id, data: ImageData::File(path) })
    }

    /// Straight-alpha sRGB RGBA pixels, `w` by `h` (`px.len() == 4 * w * h`).
    pub fn rgba(w: u32, h: u32, px: impl Into<Arc<[u8]>>) -> Arc<ImageSource> {
        let px: Arc<[u8]> = px.into();
        let id = fnv(&px, 3) ^ ((w as u64) << 32 | h as u64);
        Arc::new(ImageSource { id, data: ImageData::Rgba { w, h, px } })
    }

    /// The cache key for this image's content (for a file, its path).
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn data(&self) -> &ImageData {
        &self.data
    }

    /// Decodes this image for a box of `size` logical units at `scale` pixels per unit into
    /// the global cache, so the first frame that shows it does not. For a worker thread.
    pub fn warm(&self, size: crate::geom::Size, scale: f32) -> Result<Arc<Raster>, ImageError> {
        let px = [(size.w * scale).round() as u32, (size.h * scale).round() as u32];
        ImageCache::global().lock().unwrap_or_else(|e| e.into_inner()).raster(self, px)
    }
}

impl GpuSource for ImageSource {
    fn as_any(&self) -> &dyn Any {
        self
    }
    /// The drawing never changes.
    fn until(&self) -> f64 {
        0.0
    }
}

impl<M> Element<M> {
    /// An image (PNG or full-colour SVG) in the element's box, centred and scaled to fit with
    /// its aspect kept. Give the box a size. See [`crate::image`].
    pub fn image(key: impl Into<Key>, source: Arc<ImageSource>) -> Element<M> {
        Element::gpu(key, source)
    }
}

/// An image decoded at one pixel size: straight-alpha sRGB RGBA, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Raster {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Raster {
    pub fn bytes(&self) -> usize {
        self.px.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// The file could not be read; the text is the reason.
    Read(String),
    /// Not a PNG or SVG this decoder reads; the text is the reason.
    Decode(String),
    /// Zero pixels wide or high, asked or decoded.
    Empty,
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Read(e) => write!(f, "cannot read image: {e}"),
            ImageError::Decode(e) => write!(f, "cannot decode image: {e}"),
            ImageError::Empty => write!(f, "image has no pixels"),
        }
    }
}

impl std::error::Error for ImageError {}

/// The part of `rect` an image of `w` by `h` covers when centred and scaled to fit.
pub fn fit(rect: Rect, w: u32, h: u32) -> Rect {
    if w == 0 || h == 0 {
        return Rect::new(rect.x, rect.y, 0.0, 0.0);
    }
    let k = (rect.w / w as f32).min(rect.h / h as f32);
    let (fw, fh) = (w as f32 * k, h as f32 * k);
    Rect::new(rect.x + 0.5 * (rect.w - fw), rect.y + 0.5 * (rect.h - fh), fw, fh)
}

/// A cache key: source id and the asked pixel size.
pub type CacheKey = (u64, [u32; 2]);

/// Decoded rasters, least recently used out first, bounded by bytes. See the module docs.
#[derive(Debug)]
pub struct ImageCache {
    max: usize,
    bytes: usize,
    tick: u64,
    /// (source id, asked pixel size), last use, raster.
    entries: Vec<(CacheKey, u64, Arc<Raster>)>,
    /// Sources that failed, with why.
    failed: Vec<(u64, ImageError)>,
}

impl ImageCache {
    pub fn new(max_bytes: usize) -> ImageCache {
        ImageCache { max: max_bytes, bytes: 0, tick: 0, entries: Vec::new(), failed: Vec::new() }
    }

    /// The cache the renderers share, [`CACHE_BYTES`] big.
    pub fn global() -> &'static Mutex<ImageCache> {
        static CACHE: OnceLock<Mutex<ImageCache>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(ImageCache::new(CACHE_BYTES)))
    }

    /// Pixel bytes held now.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The raster for `src` in a box of `px` pixels, if it is already decoded. Never decodes.
    pub fn peek(&mut self, src: &ImageSource, px: [u32; 2]) -> Option<Arc<Raster>> {
        self.tick += 1;
        let tick = self.tick;
        let e = self.entries.iter_mut().find(|e| e.0 == (src.id, px))?;
        e.1 = tick;
        Some(e.2.clone())
    }

    /// The error `src` failed with, if it did.
    pub fn failure(&self, src: &ImageSource) -> Option<&ImageError> {
        self.failed.iter().find(|f| f.0 == src.id).map(|f| &f.1)
    }

    /// The raster for `src` fitted into a box of `px` pixels, decoded now if it is not in the
    /// cache. A source that failed before fails again at once.
    pub fn raster(&mut self, src: &ImageSource, px: [u32; 2]) -> Result<Arc<Raster>, ImageError> {
        if let Some(r) = self.peek(src, px) {
            return Ok(r);
        }
        if let Some(e) = self.failure(src) {
            return Err(e.clone());
        }
        match decode(&src.data, px) {
            Ok(r) => {
                let r = Arc::new(r);
                self.insert((src.id, px), r.clone());
                Ok(r)
            }
            // A box with no pixels is not the image's fault.
            Err(ImageError::Empty) if px[0] == 0 || px[1] == 0 => Err(ImageError::Empty),
            Err(e) => {
                self.failed.push((src.id, e.clone()));
                Err(e)
            }
        }
    }

    /// Adds `raster` under `key`, dropping the least recently used until it fits. A raster
    /// bigger than the whole cache is not kept.
    pub fn insert(&mut self, key: CacheKey, raster: Arc<Raster>) {
        let n = raster.bytes();
        if n > self.max {
            return;
        }
        if let Some(i) = self.entries.iter().position(|e| e.0 == key) {
            self.bytes -= self.entries.swap_remove(i).2.bytes();
        }
        while self.bytes + n > self.max {
            let Some(oldest) = self.entries.iter().enumerate().min_by_key(|(_, e)| e.1).map(|(i, _)| i) else {
                break;
            };
            self.bytes -= self.entries.swap_remove(oldest).2.bytes();
        }
        self.tick += 1;
        self.entries.push((key, self.tick, raster));
        self.bytes += n;
    }
}

/// Decodes `data` fitted into a box of `px` pixels.
pub fn decode(data: &ImageData, px: [u32; 2]) -> Result<Raster, ImageError> {
    if px[0] == 0 || px[1] == 0 {
        return Err(ImageError::Empty);
    }
    let px = [px[0].min(MAX_SIDE), px[1].min(MAX_SIDE)];
    match data {
        ImageData::Bytes(b) => decode_bytes(b, px),
        ImageData::File(path) => {
            let b = std::fs::read(path).map_err(|e| ImageError::Read(e.to_string()))?;
            if is_svg_path(path) {
                svg(&b, px)
            } else {
                decode_bytes(&b, px)
            }
        }
        ImageData::Rgba { w, h, px: src } => {
            let (w, h) = (*w, *h);
            if src.len() != (4 * w * h) as usize {
                return Err(ImageError::Decode(format!("{} bytes for {w} x {h} pixels", src.len())));
            }
            scaled(w, h, src, px)
        }
    }
}

fn is_svg_path(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

fn decode_bytes(b: &[u8], px: [u32; 2]) -> Result<Raster, ImageError> {
    if b.starts_with(&PNG_SIGNATURE) {
        let (w, h, rgba) = png_rgba(b)?;
        scaled(w, h, &rgba, px)
    } else {
        svg(b, px)
    }
}

/// A PNG as straight-alpha 8-bit RGBA.
fn png_rgba(b: &[u8]) -> Result<(u32, u32, Vec<u8>), ImageError> {
    let err = |e: png::DecodingError| ImageError::Decode(e.to_string());
    let mut dec = png::Decoder::new(std::io::Cursor::new(b));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(err)?;
    let size = reader.output_buffer_size().ok_or(ImageError::Decode("too large".into()))?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(err)?;
    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 {
        return Err(ImageError::Empty);
    }
    let n = (w * h) as usize;
    let src = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => src.to_vec(),
        png::ColorType::Rgb => src.as_chunks::<3>().0.iter().take(n).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => src.as_chunks::<2>().0.iter().take(n).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => src.iter().take(n).flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err(ImageError::Decode("palette not expanded".into())),
    };
    if rgba.len() != 4 * n {
        return Err(ImageError::Decode("short pixel data".into()));
    }
    Ok((w, h, rgba))
}

/// An SVG drawn at its fitted size in a box of `px` pixels.
fn svg(b: &[u8], px: [u32; 2]) -> Result<Raster, ImageError> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(b, &usvg::Options::default()).map_err(|e| ImageError::Decode(e.to_string()))?;
    let size = tree.size();
    let (iw, ih) = (size.width(), size.height());
    if !(iw > 0.0 && ih > 0.0) {
        return Err(ImageError::Empty);
    }
    let [w, h] = fitted_px([iw, ih], px);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or(ImageError::Empty)?;
    let t = tiny_skia::Transform::from_scale(w as f32 / iw, h as f32 / ih);
    resvg::render(&tree, t, &mut pixmap.as_mut());
    // tiny-skia holds premultiplied colour; rasters are straight.
    let mut out = pixmap.take();
    for p in out.as_chunks_mut::<4>().0 {
        let a = p[3];
        if a != 0 && a != 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
            }
        }
    }
    Ok(Raster { w, h, px: out })
}

/// The pixel size of an image `[iw, ih]` fitted into `px`, at least 1 by 1.
fn fitted_px([iw, ih]: [f32; 2], px: [u32; 2]) -> [u32; 2] {
    let k = (px[0] as f32 / iw).min(px[1] as f32 / ih);
    [((iw * k).round() as u32).clamp(1, px[0]), ((ih * k).round() as u32).clamp(1, px[1])]
}

/// `src` (straight sRGB RGBA, `w` by `h`) scaled to fit `px`, filtered in linear light with
/// premultiplied alpha: a tent as wide as the scale step when shrinking, bilinear when growing.
fn scaled(w: u32, h: u32, src: &[u8], px: [u32; 2]) -> Result<Raster, ImageError> {
    if w == 0 || h == 0 {
        return Err(ImageError::Empty);
    }
    let [dw, dh] = fitted_px([w as f32, h as f32], px);
    if [dw, dh] == [w, h] {
        return Ok(Raster { w, h, px: src.to_vec() });
    }
    let lin = lut();
    let premul: Vec<[f32; 4]> = src
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [lin[p[0] as usize] * a, lin[p[1] as usize] * a, lin[p[2] as usize] * a, a]
        })
        .collect();
    let xs = weights(w, dw);
    let ys = weights(h, dh);
    // Rows first, then columns.
    let mut mid = vec![[0.0f32; 4]; (dw * h) as usize];
    for y in 0..h as usize {
        for (x, (start, ws)) in xs.iter().enumerate() {
            let mut acc = [0.0; 4];
            for (i, k) in ws.iter().enumerate() {
                let s = premul[y * w as usize + start + i];
                (0..4).for_each(|c| acc[c] += s[c] * k);
            }
            mid[y * dw as usize + x] = acc;
        }
    }
    let mut out = vec![0u8; (4 * dw * dh) as usize];
    for (y, (start, ws)) in ys.iter().enumerate() {
        for x in 0..dw as usize {
            let mut acc = [0.0; 4];
            for (i, k) in ws.iter().enumerate() {
                let s = mid[(start + i) * dw as usize + x];
                (0..4).for_each(|c| acc[c] += s[c] * k);
            }
            let a = acc[3].clamp(0.0, 1.0);
            let o = &mut out[4 * (y * dw as usize + x)..][..4];
            for c in 0..3 {
                let v = if a > 0.0 { (acc[c] / a).clamp(0.0, 1.0) } else { 0.0 };
                o[c] = (srgb_encode(v) * 255.0 + 0.5) as u8;
            }
            o[3] = (a * 255.0 + 0.5) as u8;
        }
    }
    Ok(Raster { w: dw, h: dh, px: out })
}

/// Per output sample: the first source index and the normalised weights from there.
fn weights(n: u32, m: u32) -> Vec<(usize, Vec<f32>)> {
    let step = n as f32 / m as f32;
    let radius = step.max(1.0);
    (0..m)
        .map(|i| {
            let centre = (i as f32 + 0.5) * step - 0.5;
            let lo = ((centre - radius).ceil().max(0.0)) as usize;
            let hi = ((centre + radius).floor() as usize).min(n as usize - 1);
            let mut ws: Vec<f32> =
                (lo..=hi.max(lo)).map(|j| (1.0 - (j as f32 - centre).abs() / radius).max(0.0)).collect();
            let sum: f32 = ws.iter().sum();
            if sum > 0.0 {
                ws.iter_mut().for_each(|k| *k /= sum);
            } else {
                // Only when the tent falls between samples: take the nearest.
                ws = vec![1.0];
                return ((centre.round().clamp(0.0, n as f32 - 1.0)) as usize, ws);
            }
            (lo, ws)
        })
        .collect()
}

/// sRGB byte to linear.
fn lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| std::array::from_fn(|i| srgb_decode(i as f32 / 255.0)))
}

pub fn srgb_decode(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn srgb_encode(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// 64-bit FNV-1a, salted by kind so a path and a byte string never share an id.
fn fnv(bytes: &[u8], salt: u64) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ salt;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w` by `h` PNG, red on the left half and transparent blue on the right.
    pub fn test_png(w: u32, h: u32) -> Vec<u8> {
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                px.extend_from_slice(if x < w / 2 { &[255, 0, 0, 255] } else { &[0, 0, 255, 128] });
            }
        }
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        out
    }

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10" width="20" height="10">
<rect width="10" height="10" fill="#00ff00"/><circle cx="15" cy="5" r="5" fill="#ffffff"/></svg>"##;

    #[test]
    fn a_png_decodes_at_its_own_size() {
        let r = decode(&ImageData::Bytes(test_png(4, 2).into()), [4, 2]).unwrap();
        assert_eq!((r.w, r.h), (4, 2));
        assert_eq!(&r.px[..4], &[255, 0, 0, 255]);
        assert_eq!(&r.px[12..16], &[0, 0, 255, 128]);
    }

    #[test]
    fn a_png_scales_to_fit_its_box_keeping_aspect() {
        let r = decode(&ImageData::Bytes(test_png(64, 32).into()), [16, 16]).unwrap();
        assert_eq!((r.w, r.h), (16, 8));
        // Far left stays red, far right stays half-transparent blue: no dark fringe.
        assert_eq!(&r.px[..4], &[255, 0, 0, 255]);
        let last = &r.px[4 * 15..4 * 16];
        assert_eq!(&last[..3], &[0, 0, 255]);
        assert!((last[3] as i32 - 128).abs() <= 1, "{last:?}");
        // Upscaling works too.
        let r = decode(&ImageData::Bytes(test_png(4, 2).into()), [40, 40]).unwrap();
        assert_eq!((r.w, r.h), (40, 20));
    }

    #[test]
    fn an_svg_draws_at_its_fitted_size_with_colour() {
        let r = decode(&ImageData::Bytes(SVG.as_bytes().into()), [40, 40]).unwrap();
        assert_eq!((r.w, r.h), (40, 20));
        let at = |x: u32, y: u32| &r.px[(4 * (y * r.w + x)) as usize..][..4];
        assert_eq!(at(5, 10), &[0, 255, 0, 255]);
        assert_eq!(at(30, 10), &[255, 255, 255, 255]);
        assert_eq!(at(39, 0)[3], 0, "outside the circle is clear");
    }

    #[test]
    fn decode_errors_are_reported_and_remembered() {
        let mut cache = ImageCache::new(CACHE_BYTES);
        let junk = ImageSource::bytes(b"not an image".to_vec());
        assert!(matches!(cache.raster(&junk, [8, 8]), Err(ImageError::Decode(_))));
        assert!(cache.failure(&junk).is_some());
        let mut bad_png = test_png(4, 4);
        bad_png.truncate(40);
        assert!(matches!(cache.raster(&ImageSource::bytes(bad_png), [8, 8]), Err(ImageError::Decode(_))));
        let missing = ImageSource::file("/nonexistent/vo-image-test.png");
        assert!(matches!(cache.raster(&missing, [8, 8]), Err(ImageError::Read(_))));
        let short = ImageSource::rgba(2, 2, vec![0u8; 3]);
        assert!(matches!(cache.raster(&short, [8, 8]), Err(ImageError::Decode(_))));
        // An empty box is not a failure of the image.
        let ok = ImageSource::bytes(test_png(4, 4));
        assert_eq!(cache.raster(&ok, [0, 8]), Err(ImageError::Empty));
        assert!(cache.failure(&ok).is_none());
        assert!(cache.raster(&ok, [8, 8]).is_ok());
    }

    #[test]
    fn the_cache_stays_within_its_bytes_and_drops_the_oldest() {
        let mut cache = ImageCache::new(CACHE_BYTES);
        // 100 images of 1 MB each (512 x 512 x 4).
        let one = Arc::new(Raster { w: 512, h: 512, px: vec![0; 1 << 20] });
        for i in 0..100u64 {
            cache.insert((i, [512, 512]), one.clone());
            assert!(cache.bytes() <= CACHE_BYTES, "{} bytes after {i}", cache.bytes());
        }
        assert_eq!(cache.len(), 8);
        // The newest eight are kept.
        assert!(cache.entries.iter().all(|e| e.0 .0 >= 92));
        // Using one keeps it past the next insert.
        let src = ImageSource { id: 92, data: ImageData::Rgba { w: 0, h: 0, px: Arc::from(vec![]) } };
        assert!(cache.peek(&src, [512, 512]).is_some());
        cache.insert((100, [512, 512]), one.clone());
        assert!(cache.peek(&src, [512, 512]).is_some());
        assert!(cache.entries.iter().all(|e| e.0 .0 != 93));
        // Too big for the cache: not kept.
        cache.insert((200, [0, 0]), Arc::new(Raster { w: 0, h: 0, px: vec![0; CACHE_BYTES + 1] }));
        assert!(cache.bytes() <= CACHE_BYTES);
    }

    #[test]
    fn fit_centres_and_keeps_aspect() {
        let r = fit(Rect::new(10.0, 20.0, 40.0, 20.0), 10, 10);
        assert_eq!(r, Rect::new(20.0, 20.0, 20.0, 20.0));
    }
}
