//! Colour conversions on the CPU side of the renderer.
//!
//! Display-list colours arrive as straight-alpha sRGB (`vo_scene::geom::Color`). The shaders
//! work in linear light with premultiplied alpha. Gradient stops go to the GPU as OKLab with
//! straight alpha, because the shape shader interpolates in OKLab (Ottosson 2020) and only
//! then converts to linear sRGB and premultiplies.

use vo_scene::geom::Color;

/// sRGB transfer function, encoded to linear.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB transfer function, linear to encoded.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Linear light, premultiplied alpha, times `opacity`.
pub fn linear_premul(c: Color, opacity: f32) -> [f32; 4] {
    let a = (c.a * opacity).clamp(0.0, 1.0);
    [srgb_to_linear(c.r) * a, srgb_to_linear(c.g) * a, srgb_to_linear(c.b) * a, a]
}

/// Linear sRGB to OKLab.
pub fn linear_to_oklab([r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab to linear sRGB. The WGSL `oklab_to_linear` in `shapes.wgsl` is the same maths.
pub fn oklab_to_linear([ll, a, b]: [f32; 3]) -> [f32; 3] {
    let l = ll + 0.396_337_78 * a + 0.215_803_76 * b;
    let m = ll - 0.105_561_346 * a - 0.063_854_17 * b;
    let s = ll - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// OKLab with straight alpha times `opacity`, the form gradient stops take on the GPU.
pub fn oklab_alpha(c: Color, opacity: f32) -> [f32; 4] {
    let [l, a, b] = srgb_to_oklab(c.r, c.g, c.b);
    [l, a, b, (c.a * opacity).clamp(0.0, 1.0)]
}

/// Encoded sRGB to OKLab, through a small cache: a frame converts the same few colours for
/// hundreds of instances, and each conversion costs three `powf` and three `cbrt` (RD-14).
/// The cache returns the same bits the conversion gives.
fn srgb_to_oklab(r: f32, g: f32, b: f32) -> [f32; 3] {
    use std::cell::RefCell;
    const SLOTS: usize = 64;
    type Slot = Option<([u32; 3], [f32; 3])>;
    thread_local! {
        static CACHE: RefCell<[Slot; SLOTS]> = const { RefCell::new([None; SLOTS]) };
    }
    let key = [r.to_bits(), g.to_bits(), b.to_bits()];
    let slot = (key[0].wrapping_mul(0x9e37_79b1) ^ key[1].wrapping_mul(0x85eb_ca77) ^ key[2].wrapping_mul(0xc2b2_ae3d))
        .wrapping_shr(26) as usize
        % SLOTS;
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        match cache[slot] {
            Some((k, lab)) if k == key => lab,
            _ => {
                let lab = linear_to_oklab([srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)]);
                cache[slot] = Some((key, lab));
                lab
            }
        }
    })
}

/// Turns one premultiplied RGBA8 pixel, as read back from an sRGB render target (linear light
/// premultiplied, then sRGB encoded per channel), into straight-alpha sRGB for a PNG.
pub fn unpremultiply_srgb8(px: [u8; 4]) -> [u8; 4] {
    let a = px[3];
    if a == 0 {
        return [0; 4];
    }
    let k = 255.0 / a as f32;
    let ch = |c: u8| {
        let lin = srgb_to_linear(c as f32 / 255.0) * k;
        (linear_to_srgb(lin.min(1.0)) * 255.0).round() as u8
    };
    [ch(px[0]), ch(px[1]), ch(px[2]), a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_oklab_gives_the_same_bits() {
        for i in 0..2000u32 {
            let c = Color::rgba((i % 17) as f32 / 16.0, (i % 13) as f32 / 12.0, (i % 7) as f32 / 6.0, 1.0);
            let direct = linear_to_oklab([srgb_to_linear(c.r), srgb_to_linear(c.g), srgb_to_linear(c.b)]);
            let [l, a, b, _] = oklab_alpha(c, 1.0);
            assert_eq!([l.to_bits(), a.to_bits(), b.to_bits()], direct.map(f32::to_bits), "{i}");
        }
    }

    #[test]
    fn srgb_round_trips() {
        for i in 0..=255 {
            let c = i as f32 / 255.0;
            assert!((linear_to_srgb(srgb_to_linear(c)) - c).abs() < 1e-5, "{i}");
        }
    }

    #[test]
    fn oklab_round_trips_and_white_is_one() {
        let [l, a, b] = linear_to_oklab([1.0, 1.0, 1.0]);
        assert!((l - 1.0).abs() < 1e-3 && a.abs() < 1e-3 && b.abs() < 1e-3, "{l} {a} {b}");
        for rgb in [[0.2, 0.5, 0.9], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.04, 0.9, 0.3]] {
            let back = oklab_to_linear(linear_to_oklab(rgb));
            for k in 0..3 {
                assert!((back[k] - rgb[k]).abs() < 1e-4, "{rgb:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn premultiply_linearises_first() {
        let p = linear_premul(Color::rgba(0.5, 0.5, 0.5, 1.0), 0.5);
        assert!((p[0] - 0.214 * 0.5).abs() < 1e-3 && p[3] == 0.5, "{p:?}");
    }

    #[test]
    fn unpremultiply_restores_the_colour() {
        // Mid gray (sRGB 128) at half alpha: linear 0.2158 * 0.5, then encoded.
        let enc = (linear_to_srgb(srgb_to_linear(128.0 / 255.0) * 0.5) * 255.0).round() as u8;
        let out = unpremultiply_srgb8([enc, enc, enc, 128]);
        assert!(out[0].abs_diff(128) <= 1, "{out:?}");
        assert_eq!(unpremultiply_srgb8([10, 20, 30, 0]), [0; 4]);
    }
}
