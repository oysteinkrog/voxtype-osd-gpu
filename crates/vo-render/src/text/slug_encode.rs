//! Glyph blob encoder for the Slug coverage shader.
//!
//! A line-by-line Rust port of the encoder in HarfBuzz 14.6.0
//! (`src/hb-gpu-draw.cc`, `src/hb-gpu-cu2qu.hh` and the quicksort in
//! `src/hb-algs.hh`, upstream commit c7a7457b). The output is byte for byte
//! what `hb_gpu_draw_encode` produces, so the copied WGSL in `wgsl/` reads it
//! unchanged. `tests/encode_vs_harfbuzz.rs` checks that against the C library.
//!
//! Original code:
//!
//! Copyright (C) 2026  Behdad Esfahbod
//!
//!  This is part of HarfBuzz, a text shaping library.
//!
//! Permission is hereby granted, without written agreement and without
//! license or royalty fees, to use, copy, modify, and distribute this
//! software and its documentation for any purpose, provided that the
//! above copyright notice and the following two paragraphs appear in
//! all copies of this software.
//!
//! IN NO EVENT SHALL THE COPYRIGHT HOLDER BE LIABLE TO ANY PARTY FOR
//! DIRECT, INDIRECT, SPECIAL, INCIDENTAL, OR CONSEQUENTIAL DAMAGES
//! ARISING OUT OF THE USE OF THIS SOFTWARE AND ITS DOCUMENTATION, EVEN
//! IF THE COPYRIGHT HOLDER HAS BEEN ADVISED OF THE POSSIBILITY OF SUCH
//! DAMAGE.
//!
//! THE COPYRIGHT HOLDER SPECIFICALLY DISCLAIMS ANY WARRANTIES, INCLUDING,
//! BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
//! FITNESS FOR A PARTICULAR PURPOSE.  THE SOFTWARE PROVIDED HEREUNDER IS
//! ON AN "AS IS" BASIS, AND THE COPYRIGHT HOLDER HAS NO OBLIGATION TO
//! PROVIDE MAINTENANCE, SUPPORT, UPDATES, ENHANCEMENTS, OR MODIFICATIONS.

use skrifa::outline::{pen::PathStyle, DrawSettings, OutlinePen};
use skrifa::prelude::{LocationRef, Size};
use skrifa::raw::TableProvider;
use skrifa::{GlyphId, MetadataProvider};

/// Sub-unit quantisation: blob coordinates are font units times this.
pub const UNITS_PER_EM: f64 = 4.0;
const MAX_CURVES: usize = 65536;
const CU2QU_TOLERANCE: f64 = 0.5;
const CU2QU_MAX_DEPTH: u32 = 10;
const BAND_EPSILON: f64 = 1.0 / 1024.0;

/// One RGBA16I texel of the blob.
pub type Texel = [i16; 4];

#[derive(Clone, Copy)]
struct Curve {
    p1: [f64; 2],
    p2: [f64; 2],
    p3: [f64; 2],
    contour_start: bool,
}

#[derive(Clone, Copy, Default)]
struct CurveInfo {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    is_horizontal: bool,
    is_vertical: bool,
    hband_lo: i32,
    hband_hi: i32,
    vband_lo: i32,
    vband_hi: i32,
}

/// An encoded glyph.
#[derive(Clone, Debug, Default)]
pub struct EncodedGlyph {
    /// Empty for glyphs with no ink (space).
    pub texels: Vec<Texel>,
    /// Ink box in font units, floored and ceiled: min x, min y, max x, max y.
    pub extents: [f32; 4],
}

/// Collects one glyph outline and encodes it. Implements [`OutlinePen`].
pub struct Encoder {
    start: [f64; 2],
    current: [f64; 2],
    need_moveto: bool,
    success: bool,
    curves: Vec<Curve>,
    ext_min: [f64; 2],
    ext_max: [f64; 2],
    x_scale: i32,
    y_scale: i32,
}

impl Default for Encoder {
    fn default() -> Self {
        Self {
            start: [0.0; 2],
            current: [0.0; 2],
            need_moveto: true,
            success: true,
            curves: Vec::new(),
            ext_min: [f64::INFINITY; 2],
            ext_max: [f64::NEG_INFINITY; 2],
            x_scale: 0,
            y_scale: 0,
        }
    }
}

fn quantize(v: f64) -> i16 {
    (v * UNITS_PER_EM).round() as i16
}

fn dequantize(v: i32) -> f64 {
    v as f64 / UNITS_PER_EM
}

fn fits_i16(q: f64) -> bool {
    q >= i16::MIN as f64 && q <= i16::MAX as f64
}

fn encode_offset(offset: usize) -> i16 {
    (offset as u32).wrapping_sub(32768) as u16 as i16
}

fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Encodes glyph `gid` of `font` at its unscaled size (font units), with
    /// the font's units per em as the blob scale, like `hb_gpu_draw_glyph`
    /// on a default `hb_font_t`.
    pub fn encode_glyph(&mut self, font: &skrifa::FontRef, gid: GlyphId) -> Option<EncodedGlyph> {
        self.encode_glyph_at(font, gid, LocationRef::default())
    }

    /// Same as [`Self::encode_glyph`] at a variation location.
    pub fn encode_glyph_at(
        &mut self,
        font: &skrifa::FontRef,
        gid: GlyphId,
        location: LocationRef,
    ) -> Option<EncodedGlyph> {
        let upem = font.head().ok()?.units_per_em() as i32;
        self.clear();
        self.x_scale = upem;
        self.y_scale = upem;
        if let Some(glyph) = font.outline_glyphs().get(gid) {
            let settings = DrawSettings::unhinted(Size::unscaled(), location).with_path_style(PathStyle::HarfBuzz);
            glyph.draw(settings, self).ok()?;
        }
        self.encode()
    }

    /// Encodes whatever `draw` feeds through the [`OutlinePen`] methods, in font units with
    /// `scale` units per em. For paths that are not glyphs.
    pub fn encode_with(&mut self, scale: i32, draw: impl FnOnce(&mut Self)) -> Option<EncodedGlyph> {
        self.x_scale = scale;
        self.y_scale = scale;
        self.clear();
        draw(self);
        self.acc_close_path();
        self.encode()
    }

    fn clear(&mut self) {
        let (xs, ys) = (self.x_scale, self.y_scale);
        *self = Self { x_scale: xs, y_scale: ys, curves: std::mem::take(&mut self.curves), ..Self::default() };
        self.curves.clear();
    }

    fn update_extents(&mut self, p: [f64; 2]) {
        self.ext_min[0] = self.ext_min[0].min(p[0]);
        self.ext_min[1] = self.ext_min[1].min(p[1]);
        self.ext_max[0] = self.ext_max[0].max(p[0]);
        self.ext_max[1] = self.ext_max[1].max(p[1]);
    }

    fn emit(&mut self, contour_start: bool, p1: [f64; 2], p2: [f64; 2], p3: [f64; 2]) {
        if self.curves.len() >= MAX_CURVES {
            self.success = false;
            return;
        }
        self.curves.push(Curve { p1, p2, p3, contour_start });
        self.current = p3;
        self.update_extents(p1);
        self.update_extents(p2);
        self.update_extents(p3);
    }

    fn emit_conic(&mut self, c: [f64; 2], p: [f64; 2]) {
        if self.current == p {
            return;
        }
        let contour_start = self.need_moveto;
        if self.need_moveto {
            self.start = self.current;
            self.need_moveto = false;
        }
        self.emit(contour_start, self.current, c, p);
    }

    fn acc_move_to(&mut self, p: [f64; 2]) {
        if !self.success {
            return;
        }
        self.need_moveto = true;
        self.current = p;
    }

    fn acc_line_to(&mut self, p: [f64; 2]) {
        if !self.success {
            return;
        }
        self.emit_conic(self.current, p);
    }

    fn acc_conic_to(&mut self, c: [f64; 2], p: [f64; 2]) {
        if !self.success {
            return;
        }
        self.emit_conic(c, p);
    }

    fn acc_close_path(&mut self) {
        if !self.success {
            return;
        }
        if !self.need_moveto && self.current != self.start {
            self.acc_line_to(self.start);
        }
    }

    fn acc_cubic_to(&mut self, c1: [f64; 2], c2: [f64; 2], p: [f64; 2]) {
        if !self.success {
            return;
        }
        let c0 = self.current;
        if c0 == p && c1 == p && c2 == p {
            return;
        }
        // Collinear control points: emit a line instead of subdividing.
        let (dx, dy) = (p[0] - c0[0], p[1] - c0[1]);
        let len_sq = dx * dx + dy * dy;
        if len_sq > 0.0 {
            let inv = 1.0 / len_sq;
            let cross1 = (c1[0] - c0[0]) * dy - (c1[1] - c0[1]) * dx;
            let cross2 = (c2[0] - c0[0]) * dy - (c2[1] - c0[1]) * dx;
            let tol2 = CU2QU_TOLERANCE * CU2QU_TOLERANCE;
            if cross1 * cross1 * inv <= tol2 && cross2 * cross2 * inv <= tol2 {
                self.acc_line_to(p);
                return;
            }
        }
        self.cubic_to_quadratics(c0, c1, c2, p, 0);
    }

    fn cubic_to_quadratics(&mut self, c0: [f64; 2], c1: [f64; 2], c2: [f64; 2], c3: [f64; 2], depth: u32) {
        if let Some(q1) = cubic_approx_quadratic(c0, c1, c2, c3, CU2QU_TOLERANCE) {
            self.acc_conic_to(q1, c3);
            return;
        }
        if depth >= CU2QU_MAX_DEPTH {
            self.acc_line_to(c3);
            return;
        }
        let m01 = lerp(c0, c1, 0.5);
        let m12 = lerp(c1, c2, 0.5);
        let m23 = lerp(c2, c3, 0.5);
        let m012 = lerp(m01, m12, 0.5);
        let m123 = lerp(m12, m23, 0.5);
        let mid = lerp(m012, m123, 0.5);
        self.cubic_to_quadratics(c0, m01, m012, mid, depth + 1);
        self.cubic_to_quadratics(mid, m123, m23, c3, depth + 1);
    }

    /// Ink extents as `_hb_gpu_draw_get_extents` reports them, Y up.
    fn extents(&self) -> [f32; 4] {
        if !self.success || self.curves.is_empty() || self.ext_min[0] == f64::INFINITY {
            return [0.0; 4];
        }
        [
            self.ext_min[0].floor() as f32,
            self.ext_min[1].floor() as f32,
            self.ext_max[0].ceil() as f32,
            self.ext_max[1].ceil() as f32,
        ]
    }

    /// `hb_gpu_draw_encode`. Returns `None` where HarfBuzz returns NULL.
    pub fn encode(&mut self) -> Option<EncodedGlyph> {
        let extents = self.extents();
        let out = self.encode_inner().map(|texels| EncodedGlyph { texels, extents });
        self.clear();
        out
    }

    fn encode_inner(&self) -> Option<Vec<Texel>> {
        if !self.success {
            return None;
        }
        let curves = &self.curves;
        let num_curves = curves.len();
        if num_curves == 0 {
            return Some(Vec::new());
        }
        let u = UNITS_PER_EM;
        if !fits_i16((self.ext_min[0] * u).floor())
            || !fits_i16((self.ext_min[1] * u).floor())
            || !fits_i16((self.ext_max[0] * u).ceil())
            || !fits_i16((self.ext_max[1] * u).ceil())
        {
            return None;
        }
        let min_x_q = (self.ext_min[0] * u).floor() as i16;
        let min_y_q = (self.ext_min[1] * u).floor() as i16;
        let max_x_q = (self.ext_max[0] * u).ceil() as i16;
        let max_y_q = (self.ext_max[1] * u).ceil() as i16;
        let (min_x, min_y) = (dequantize(min_x_q as i32), dequantize(min_y_q as i32));
        let (max_x, max_y) = (dequantize(max_x_q as i32), dequantize(max_y_q as i32));

        let mut infos: Vec<CurveInfo> = curves.iter().map(curve_info).collect();

        let mut num_hbands = num_curves.clamp(1, 16);
        let mut num_vbands = num_curves.clamp(1, 16);
        let height = max_y - min_y;
        let width = max_x - min_x;
        if height <= 0.0 {
            num_hbands = 1;
        }
        if width <= 0.0 {
            num_vbands = 1;
        }
        let hband_size = height / num_hbands as f64;
        let vband_size = width / num_vbands as f64;

        let mut hband_counts = vec![0usize; num_hbands];
        let mut vband_counts = vec![0usize; num_vbands];
        for info in &mut infos {
            if !info.is_horizontal {
                if height > 0.0 {
                    info.hband_lo = ((info.min_y - min_y) / hband_size - BAND_EPSILON).floor() as i32;
                    info.hband_hi = ((info.max_y - min_y) / hband_size + BAND_EPSILON).floor() as i32;
                    info.hband_lo = info.hband_lo.max(0);
                    info.hband_hi = info.hband_hi.min(num_hbands as i32 - 1);
                    for b in info.hband_lo..=info.hband_hi {
                        hband_counts[b as usize] += 1;
                    }
                } else {
                    info.hband_lo = 0;
                    info.hband_hi = 0;
                    hband_counts[0] += 1;
                }
            }
            if !info.is_vertical {
                if width > 0.0 {
                    info.vband_lo = ((info.min_x - min_x) / vband_size - BAND_EPSILON).floor() as i32;
                    info.vband_hi = ((info.max_x - min_x) / vband_size + BAND_EPSILON).floor() as i32;
                    info.vband_lo = info.vband_lo.max(0);
                    info.vband_hi = info.vband_hi.min(num_vbands as i32 - 1);
                    for b in info.vband_lo..=info.vband_hi {
                        vband_counts[b as usize] += 1;
                    }
                } else {
                    info.vband_lo = 0;
                    info.vband_hi = 0;
                    vband_counts[0] += 1;
                }
            }
        }

        let prefix = |counts: &[usize]| {
            let mut acc = 0;
            counts
                .iter()
                .map(|c| {
                    let o = acc;
                    acc += c;
                    o
                })
                .collect::<Vec<_>>()
        };
        let hband_offsets = prefix(&hband_counts);
        let vband_offsets = prefix(&vband_counts);
        let total_h: usize = hband_counts.iter().sum();
        let total_v: usize = vband_counts.iter().sum();

        let mut hband_curves = vec![0u32; total_h];
        let mut hband_curves_asc = vec![0u32; total_h];
        let mut vband_curves = vec![0u32; total_v];
        let mut vband_curves_asc = vec![0u32; total_v];
        let mut hcur = hband_offsets.clone();
        let mut vcur = vband_offsets.clone();
        for (i, info) in infos.iter().enumerate() {
            for b in info.hband_lo..=info.hband_hi {
                let idx = hcur[b as usize];
                hcur[b as usize] += 1;
                hband_curves[idx] = i as u32;
                hband_curves_asc[idx] = i as u32;
            }
            for b in info.vband_lo..=info.vband_hi {
                let idx = vcur[b as usize];
                vcur[b as usize] += 1;
                vband_curves[idx] = i as u32;
                vband_curves_asc[idx] = i as u32;
            }
        }

        // Sort: descending by max, ascending by min.
        let infos = &infos[..];
        let desc = |key: fn(&CurveInfo) -> f64| {
            move |a: u32, b: u32| {
                let (av, bv) = (key(&infos[a as usize]), key(&infos[b as usize]));
                (av < bv) as i32 - (av > bv) as i32
            }
        };
        let asc = |key: fn(&CurveInfo) -> f64| {
            move |a: u32, b: u32| {
                let (av, bv) = (key(&infos[a as usize]), key(&infos[b as usize]));
                (av > bv) as i32 - (av < bv) as i32
            }
        };
        for b in 0..num_hbands {
            let r = hband_offsets[b]..hband_offsets[b] + hband_counts[b];
            hb_qsort(&mut hband_curves[r.clone()], desc(|i| i.max_x));
            hb_qsort(&mut hband_curves_asc[r], asc(|i| i.min_x));
        }
        for b in 0..num_vbands {
            let r = vband_offsets[b]..vband_offsets[b] + vband_counts[b];
            hb_qsort(&mut vband_curves[r.clone()], desc(|i| i.max_y));
            hb_qsort(&mut vband_curves_asc[r], asc(|i| i.min_y));
        }

        let total_curve_indices = (total_h + total_v) * 2;
        let header_len = 2;
        let contour_breaks = (0..num_curves.saturating_sub(1)).filter(|&i| curves[i + 1].contour_start).count();
        let curve_data_len = num_curves + contour_breaks + 1;
        let band_headers_len = num_hbands + num_vbands;
        let total_len = header_len + band_headers_len + total_curve_indices + curve_data_len;
        if total_len > u16::MAX as usize + 1 {
            return None;
        }
        let mut buf: Vec<Texel> = vec![[0; 4]; total_len];
        let curve_data_offset = header_len + band_headers_len + total_curve_indices;

        buf[0] = [min_x_q, min_y_q, max_x_q, max_y_q];
        buf[1] = [
            num_hbands as i16,
            num_vbands as i16,
            self.x_scale.clamp(-32768, 32767) as i16,
            self.y_scale.clamp(-32768, 32767) as i16,
        ];

        // Curve data with shared endpoints.
        let mut curve_texel = vec![0usize; num_curves];
        let mut texel = curve_data_offset;
        for i in 0..num_curves {
            let c = &curves[i];
            if c.contour_start {
                curve_texel[i] = texel;
                buf[texel] = [quantize(c.p1[0]), quantize(c.p1[1]), quantize(c.p2[0]), quantize(c.p2[1])];
                texel += 1;
            } else {
                curve_texel[i] = texel - 1;
            }
            let has_next = i + 1 < num_curves && !curves[i + 1].contour_start;
            let (nx, ny) =
                if has_next { (quantize(curves[i + 1].p2[0]), quantize(curves[i + 1].p2[1])) } else { (0, 0) };
            buf[texel] = [quantize(c.p3[0]), quantize(c.p3[1]), nx, ny];
            texel += 1;
        }

        // Band headers and curve indices.
        let mut index_offset = header_len + band_headers_len;
        let mut pack_bands = |num_bands: usize,
                              offsets: &[usize],
                              counts: &[usize],
                              desc_list: &[u32],
                              asc_list: &[u32],
                              hdr_base: usize,
                              max_key: fn(&CurveInfo) -> f64,
                              min_key: fn(&CurveInfo) -> f64,
                              default_split: f64,
                              buf: &mut Vec<Texel>| {
            for b in 0..num_bands {
                let (off, n) = (offsets[b], counts[b]);
                let mut best_worst = n;
                let mut best_split = default_split;
                let mut left_count = n;
                for ci in 0..n {
                    let split = max_key(&infos[desc_list[off + ci] as usize]);
                    let right_count = ci + 1;
                    while left_count > 0 && min_key(&infos[asc_list[off + left_count - 1] as usize]) > split {
                        left_count -= 1;
                    }
                    let worst = right_count.max(left_count);
                    if worst < best_worst {
                        best_worst = worst;
                        best_split = split;
                    }
                }
                let desc_off = index_offset;
                for ci in 0..n {
                    buf[index_offset] = [encode_offset(curve_texel[desc_list[off + ci] as usize]), 0, 0, 0];
                    index_offset += 1;
                }
                let asc_off = index_offset;
                for ci in 0..n {
                    buf[index_offset] = [encode_offset(curve_texel[asc_list[off + ci] as usize]), 0, 0, 0];
                    index_offset += 1;
                }
                buf[hdr_base + b] = [n as i16, encode_offset(desc_off), encode_offset(asc_off), quantize(best_split)];
            }
        };
        pack_bands(
            num_hbands,
            &hband_offsets,
            &hband_counts,
            &hband_curves,
            &hband_curves_asc,
            header_len,
            |i| i.max_x,
            |i| i.min_x,
            (min_x + max_x) * 0.5,
            &mut buf,
        );
        pack_bands(
            num_vbands,
            &vband_offsets,
            &vband_counts,
            &vband_curves,
            &vband_curves_asc,
            header_len + num_hbands,
            |i| i.max_y,
            |i| i.min_y,
            (min_y + max_y) * 0.5,
            &mut buf,
        );
        Some(buf)
    }
}

impl OutlinePen for Encoder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.acc_close_path();
        self.acc_move_to([x as f64, y as f64]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.acc_line_to([x as f64, y as f64]);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.acc_conic_to([cx0 as f64, cy0 as f64], [x as f64, y as f64]);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.acc_cubic_to([cx0 as f64, cy0 as f64], [cx1 as f64, cy1 as f64], [x as f64, y as f64]);
    }
    fn close(&mut self) {
        self.acc_close_path();
    }
}

/// Bounds from the quantised control points, which is what the shader sees.
fn curve_info(c: &Curve) -> CurveInfo {
    let q = |v: f64| quantize(v) as i32;
    let (p1x, p2x, p3x) = (q(c.p1[0]), q(c.p2[0]), q(c.p3[0]));
    let (p1y, p2y, p3y) = (q(c.p1[1]), q(c.p2[1]), q(c.p3[1]));
    CurveInfo {
        min_x: dequantize(p1x.min(p2x).min(p3x)),
        max_x: dequantize(p1x.max(p2x).max(p3x)),
        min_y: dequantize(p1y.min(p2y).min(p3y)),
        max_y: dequantize(p1y.max(p2y).max(p3y)),
        is_horizontal: p1y == p2y && p2y == p3y,
        is_vertical: p1x == p2x && p2x == p3x,
        hband_lo: 0,
        hband_hi: -1,
        vband_lo: 0,
        vband_hi: -1,
    }
}

fn cubic_farthest_fit_inside(p0: [f64; 2], p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], tol: f64, depth: u32) -> bool {
    if p1[0].hypot(p1[1]) <= tol && p2[0].hypot(p2[1]) <= tol {
        return true;
    }
    if depth >= 8 {
        return false;
    }
    let mid = [(p0[0] + 3.0 * (p1[0] + p2[0]) + p3[0]) * 0.125, (p0[1] + 3.0 * (p1[1] + p2[1]) + p3[1]) * 0.125];
    if mid[0].hypot(mid[1]) > tol {
        return false;
    }
    let d3 = [(p3[0] + p2[0] - p1[0] - p0[0]) * 0.125, (p3[1] + p2[1] - p1[1] - p0[1]) * 0.125];
    cubic_farthest_fit_inside(p0, lerp(p0, p1, 0.5), [mid[0] - d3[0], mid[1] - d3[1]], mid, tol, depth + 1)
        && cubic_farthest_fit_inside(mid, [mid[0] + d3[0], mid[1] + d3[1]], lerp(p2, p3, 0.5), p3, tol, depth + 1)
}

fn cubic_approx_quadratic(c0: [f64; 2], c1: [f64; 2], c2: [f64; 2], c3: [f64; 2], tol: f64) -> Option<[f64; 2]> {
    let (ax, ay) = (c1[0] - c0[0], c1[1] - c0[1]);
    let (dx, dy) = (c3[0] - c2[0], c3[1] - c2[1]);
    let (px, py) = (-ay, ax);
    let denom = px * dx + py * dy;
    if denom.abs() < 1e-12 {
        return None;
    }
    let h = (px * (c0[0] - c2[0]) + py * (c0[1] - c2[1])) / denom;
    let q1 = [c2[0] + dx * h, c2[1] + dy * h];
    let err1 = [c0[0] + (q1[0] - c0[0]) * (2.0 / 3.0) - c1[0], c0[1] + (q1[1] - c0[1]) * (2.0 / 3.0) - c1[1]];
    let err2 = [c3[0] + (q1[0] - c3[0]) * (2.0 / 3.0) - c2[0], c3[1] + (q1[1] - c3[1]) * (2.0 / 3.0) - c2[1]];
    cubic_farthest_fit_inside([0.0; 2], err1, err2, [0.0; 2], tol, 0).then_some(q1)
}

/// `hb_qsort_inline`: median-of-three quicksort down to runs of 24, then one
/// insertion sort pass. Not stable; ported so ties land where HarfBuzz puts them.
fn hb_qsort(v: &mut [u32], cmp: impl Fn(u32, u32) -> i32 + Copy) {
    hb_qsort_loop(v, cmp);
    for pi in 1..v.len() {
        let mut pj = pi;
        while pj > 0 && cmp(v[pj - 1], v[pj]) > 0 {
            v.swap(pj - 1, pj);
            pj -= 1;
        }
    }
}

fn hb_qsort_loop(v: &mut [u32], cmp: impl Fn(u32, u32) -> i32 + Copy) {
    let mut base = 0usize;
    let mut nel = v.len();
    while nel > 24 {
        let last = base + nel - 1;
        let mid = base + nel / 2;
        if cmp(v[base], v[mid]) > 0 {
            v.swap(base, mid);
        }
        if cmp(v[mid], v[last]) > 0 {
            v.swap(mid, last);
            if cmp(v[base], v[mid]) > 0 {
                v.swap(base, mid);
            }
        }
        v.swap(mid, last - 1);
        let pivot = v[last - 1];
        let (mut i, mut j) = (base, last - 1);
        loop {
            loop {
                i += 1;
                if cmp(v[i], pivot) >= 0 {
                    break;
                }
            }
            loop {
                j -= 1;
                if cmp(v[j], pivot) <= 0 {
                    break;
                }
            }
            if i >= j {
                break;
            }
            v.swap(i, j);
        }
        v.swap(i, last - 1);
        let left = i - base;
        let right = nel - left - 1;
        if left < right {
            hb_qsort_loop(&mut v[base..base + left], cmp);
            base = i + 1;
            nel = right;
        } else {
            hb_qsort_loop(&mut v[i + 1..i + 1 + right], cmp);
            nel = left;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hb_qsort_sorts() {
        let mut v: Vec<u32> = (0..200).map(|i| (i * 7919 + 13) % 101).collect();
        hb_qsort(&mut v, |a, b| (a > b) as i32 - (a < b) as i32);
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn square_blob_layout() {
        let mut e = Encoder::new();
        e.x_scale = 1000;
        e.y_scale = 1000;
        e.move_to(0.0, 0.0);
        e.line_to(100.0, 0.0);
        e.line_to(100.0, 100.0);
        e.line_to(0.0, 100.0);
        e.close();
        let g = e.encode().unwrap();
        assert_eq!(g.texels[0], [0, 0, 400, 400]);
        assert_eq!(g.texels[1], [4, 4, 1000, 1000]);
        assert_eq!(g.extents, [0.0, 0.0, 100.0, 100.0]);
    }
}
