//! Compares the Rust encoder with HarfBuzz's own `hb_gpu_draw_encode`, glyph
//! by glyph. The reference dump comes from `tools/hb-gpu-dump.c`:
//!
//!   VO_HBGPU_FONT=/usr/share/fonts/noto/NotoSans-Regular.ttf
//!   VO_HBGPU_DUMP=/var/tmp/claude/vo-p0/noto-sans.hbgpu
//!
//! Skipped (with a note) when either file is missing.

use skrifa::{FontRef, GlyphId};
use vo_render::text::slug_encode::Encoder;

fn var(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

#[test]
fn encoder_matches_harfbuzz() {
    let font_path = var("VO_HBGPU_FONT", "/usr/share/fonts/noto/NotoSans-Regular.ttf");
    let dump_path = var("VO_HBGPU_DUMP", "/var/tmp/claude/vo-p0/noto-sans.hbgpu");
    let (Ok(font_data), Ok(dump)) = (std::fs::read(&font_path), std::fs::read(&dump_path)) else {
        eprintln!("skipped: need {font_path} and {dump_path}");
        return;
    };
    let font = FontRef::new(&font_data).unwrap();
    let mut enc = Encoder::new();
    let (mut pos, mut glyphs, mut mismatches) = (0usize, 0usize, Vec::new());
    while pos < dump.len() {
        let gid = u32::from_le_bytes(dump[pos..pos + 4].try_into().unwrap());
        let len = u32::from_le_bytes(dump[pos + 4..pos + 8].try_into().unwrap());
        pos += 8;
        let reference: Option<&[u8]> = if len == u32::MAX {
            None
        } else {
            let r = &dump[pos..pos + len as usize];
            pos += len as usize;
            Some(r)
        };
        let ours = enc.encode_glyph(&font, GlyphId::new(gid));
        let ours_bytes: Option<Vec<u8>> =
            ours.map(|g| g.texels.iter().flat_map(|t| t.iter().flat_map(|v| v.to_le_bytes())).collect());
        if ours_bytes.as_deref() != reference {
            mismatches.push(gid);
        }
        glyphs += 1;
    }
    eprintln!("compared {glyphs} glyphs, {} differ", mismatches.len());
    assert!(mismatches.is_empty(), "glyphs that differ from HarfBuzz: {:?}", &mismatches[..mismatches.len().min(20)]);
}
