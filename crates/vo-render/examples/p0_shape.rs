//! P0-10: cold cost of harfrust shaping plus line fitting for a 40-word
//! line, and the cost of a lazy fontique fallback lookup.
//!
//! Each measurement runs in a fresh process (`--once`); the parent runs 30 of
//! them and prints p50 and p95 per stage. "Cold" means a new process with the
//! font file in the page cache.
//!
//!   cargo run --release -p vo-render --example p0_shape

mod common;

use std::time::Instant;

use common::*;
use unicode_linebreak::{BreakOpportunity, linebreaks};

const LINE_A: &str = "so the plan for tomorrow is that we meet at nine in the small room on the second floor and go through the list of open bugs one by one before lunch then after lunch we split up and test";
const LINE_B: &str = "when the build finishes I want to look at the numbers for the first frame again because last week they were much higher on the office machine than at home and nobody could explain why that would be the case";

const STAGES: [&str; 10] = [
    "fc_match_subprocess",
    "read_font",
    "font_and_shaper",
    "shape_cold",
    "linebreaks",
    "fit_greedy_balanced",
    "shape_warm",
    "total_text_path",
    "fontique_collection_new",
    "fontique_fallback_query",
];

struct Seg {
    end: usize,
    width: f32,
}

/// Splits shaped text at break opportunities into segments with widths.
fn segments(text: &str, infos: &[harfrust::GlyphInfo], pos: &[harfrust::GlyphPosition], scale: f32) -> Vec<Seg> {
    let breaks: Vec<(usize, BreakOpportunity)> = linebreaks(text).collect();
    let mut segs: Vec<Seg> = breaks.iter().map(|&(end, _)| Seg { end, width: 0.0 }).collect();
    let mut si = 0;
    for (i, p) in infos.iter().zip(pos) {
        while si + 1 < segs.len() && i.cluster as usize >= segs[si].end {
            si += 1;
        }
        segs[si].width += p.x_advance as f32 * scale;
    }
    segs
}

/// Greedy fill: returns the segment index that ends each line.
fn fit_greedy(segs: &[Seg], max_w: f32) -> Vec<usize> {
    let mut lines = Vec::new();
    let mut w = 0.0;
    for (i, s) in segs.iter().enumerate() {
        if w > 0.0 && w + s.width > max_w {
            lines.push(i - 1);
            w = 0.0;
        }
        w += s.width;
    }
    lines.push(segs.len() - 1);
    lines
}

/// Balanced: the narrowest width that keeps the greedy line count, found by
/// bisection. Used for centred text.
fn fit_balanced(segs: &[Seg], max_w: f32) -> (Vec<usize>, f32) {
    let n = fit_greedy(segs, max_w).len();
    let widest = segs.iter().map(|s| s.width).fold(0.0, f32::max);
    let (mut lo, mut hi) = (widest, max_w);
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        if fit_greedy(segs, mid).len() <= n {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (fit_greedy(segs, hi), hi)
}

fn once() {
    let t0 = Instant::now();
    let mut t = Vec::new();
    let mut lap = Instant::now();
    let mark = |t: &mut Vec<f64>, lap: &mut Instant| {
        t.push(lap.elapsed().as_secs_f64() * 1e3);
        *lap = Instant::now();
    };

    let path = ui_font_path();
    mark(&mut t, &mut lap);
    let data = std::fs::read(path).expect("font");
    mark(&mut t, &mut lap);

    let font = harfrust::Font::new(data, 0).expect("font");
    let upem = font.units_per_em() as i32;
    let shaper = harfrust::ShaperFont::new(&font).with_scale(upem);
    mark(&mut t, &mut lap);

    let px = 28.0f32;
    let scale = px / upem as f32;
    let shape = |text: &str| {
        let mut buf = harfrust::Buffer::new();
        buf.push_str(text);
        buf.guess_segment_properties();
        harfrust::shape(&shaper, &mut buf, harfrust::ShapeOptions::new()).unwrap();
        buf
    };
    let a = shape(LINE_A);
    mark(&mut t, &mut lap);

    let segs = segments(LINE_A, a.glyph_infos(), a.glyph_positions(), scale);
    mark(&mut t, &mut lap);

    let greedy = fit_greedy(&segs, 900.0);
    let (balanced, bw) = fit_balanced(&segs, 900.0);
    mark(&mut t, &mut lap);

    let b = shape(LINE_B);
    mark(&mut t, &mut lap);
    // Text path total, without the fontconfig subprocess (the overlay will
    // resolve the font through fontconfig in-process or from a cached path).
    t.push(t0.elapsed().as_secs_f64() * 1e3 - t[0]);

    lap = Instant::now();
    let mut coll = fontique::Collection::new(fontique::CollectionOptions::default());
    mark(&mut t, &mut lap);
    let mut cache = fontique::SourceCache::default();
    let found = {
        let mut q = coll.query(&mut cache);
        q.set_fallbacks(fontique::FallbackKey::new(fontique::Script::from_bytes(*b"Hani"), None));
        q.first_available_font().map(|f| f.family.0)
    };
    let family = found.and_then(|id| coll.family_name(id).map(str::to_string));
    mark(&mut t, &mut lap);

    let ms: Vec<String> = t.iter().map(|v| format!("{v:.4}")).collect();
    println!("STAGES {}", ms.join(" "));
    println!(
        "INFO glyphs_a={} glyphs_b={} segments={} greedy_lines={} balanced_lines={} balanced_w={bw:.0} hani_fallback={:?}",
        a.len(),
        b.len(),
        segs.len(),
        greedy.len(),
        balanced.len(),
        family
    );
}

fn main() {
    if std::env::args().any(|a| a == "--once") {
        once();
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let runs = 30;
    let mut cols: Vec<Vec<f64>> = vec![Vec::new(); STAGES.len()];
    let mut info = String::new();
    for _ in 0..runs {
        let out = std::process::Command::new(&exe).arg("--once").output().expect("spawn");
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("STAGES ") {
                for (c, v) in cols.iter_mut().zip(rest.split(' ')) {
                    c.push(v.parse().unwrap());
                }
            } else if let Some(rest) = line.strip_prefix("INFO ") {
                info = rest.to_string();
            }
        }
    }
    println!("font {}", ui_font_path().display());
    println!("{info}");
    println!("{runs} fresh processes, ms per stage:");
    println!("{:<26} {:>9} {:>9} {:>9}", "stage", "p50", "p95", "max");
    for (name, c) in STAGES.iter().zip(&mut cols) {
        c.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "{name:<26} {:>9.3} {:>9.3} {:>9.3}",
            percentile(c, 0.5),
            percentile(c, 0.95),
            c.last().copied().unwrap_or(f64::NAN)
        );
    }
}
