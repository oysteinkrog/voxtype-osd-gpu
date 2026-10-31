//! The built-in material tiles (EG-25): four 256 x 256 greyscale tiles that wrap at every
//! edge, made from fixed seeds so the files are the same on every run and carry no
//! third-party licence.
//!
//! - `paper`: soft mottling with short light and dark fibres, for typewriter paper.
//! - `fabric`: a plain weave of round threads, for a woven ribbon.
//! - `rubber`: a fine stipple with small pores, for a rubber platen.
//! - `film-grain`: clumped per-pixel noise, for film grain.
//!
//! Each file is 65 536 raw bytes, one byte a pixel, row by row. `Material::paper` and its
//! siblings in `vo_scene::material` sample them.
//!
//!   cargo run -p vo --example make_textures -- [--out DIR] [--check]
//!
//! Writes `DIR/<name>.gray` (default DIR: the repo's `assets/textures`). `--check` writes
//! nothing and exits 1 when a file on disk differs from what this run makes.

use std::path::PathBuf;

/// The tile's side in pixels.
const N: usize = 256;

/// The tiles, in the order of their channels in the renderer's tile texture.
const TILES: [(&str, fn() -> Vec<f64>); 4] =
    [("paper", paper), ("fabric", fabric), ("rubber", rubber), ("film-grain", film_grain)];

/// SplitMix64: a small, fixed random source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// 0 to 1.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A fixed value for a lattice point, 0 to 1.
fn hash(x: i64, y: i64, seed: u64) -> f64 {
    let mut r =
        Rng(seed ^ (x as u64).wrapping_mul(0x632b_e59b_d9b4_e019) ^ (y as u64).wrapping_mul(0x8cb9_2ba7_2f3d_8dd7));
    r.next();
    r.unit()
}

/// Value noise with a lattice of `period` cells across the tile, so it wraps: 0 to 1.
fn value_noise(x: f64, y: f64, period: usize, seed: u64) -> f64 {
    let cell = N as f64 / period as f64;
    let (fx, fy) = (x / cell, y / cell);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let p = period as i64;
    let at = |i: i64, j: i64| hash((x0 as i64 + i).rem_euclid(p), (y0 as i64 + j).rem_euclid(p), seed);
    let top = at(0, 0) + (at(1, 0) - at(0, 0)) * s(tx);
    let bottom = at(0, 1) + (at(1, 1) - at(0, 1)) * s(tx);
    top + (bottom - top) * s(ty)
}

/// Octaves of value noise: `(period, weight)` each.
fn fbm(x: f64, y: f64, octaves: &[(usize, f64)], seed: u64) -> f64 {
    octaves.iter().enumerate().map(|(k, &(p, w))| w * value_noise(x, y, p, seed + k as u64)).sum()
}

fn field(f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    (0..N * N).map(|i| f((i % N) as f64, (i / N) as f64)).collect()
}

/// Adds `v` times a soft round brush of radius `r` at `x, y`, wrapping at the edges.
fn dab(px: &mut [f64], x: f64, y: f64, r: f64, v: f64) {
    let reach = (r * 2.0).ceil() as i64;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let (cx, cy) = (x.floor() as i64 + dx, y.floor() as i64 + dy);
            let d2 = (cx as f64 + 0.5 - x).powi(2) + (cy as f64 + 0.5 - y).powi(2);
            let k = (-d2 / (r * r)).exp();
            if k > 0.01 {
                let i = cy.rem_euclid(N as i64) as usize * N + cx.rem_euclid(N as i64) as usize;
                px[i] += v * k;
            }
        }
    }
}

/// A 3 x 3 blur that wraps, `times` over.
fn blur(px: &mut Vec<f64>, times: usize) {
    for _ in 0..times {
        let src = px.clone();
        for y in 0..N {
            for x in 0..N {
                let mut sum = 0.0;
                for (dy, wy) in [(N - 1, 1.0), (0, 2.0), (1, 1.0)] {
                    for (dx, wx) in [(N - 1, 1.0), (0, 2.0), (1, 1.0)] {
                        sum += wy * wx * src[(y + dy) % N * N + (x + dx) % N];
                    }
                }
                px[y * N + x] = sum / 16.0;
            }
        }
    }
}

fn paper() -> Vec<f64> {
    let mut px = field(|x, y| fbm(x, y, &[(4, 0.45), (8, 0.3), (32, 0.15), (128, 0.1)], 11));
    let mut r = Rng(0x7061_7065_72);
    for _ in 0..1400 {
        let (mut x, mut y) = (r.unit() * N as f64, r.unit() * N as f64);
        let mut angle = r.unit() * std::f64::consts::TAU;
        let bend = (r.unit() - 0.5) * 0.08;
        let len = 8.0 + r.unit() * 30.0;
        // Most fibres show lighter than the sheet, some darker.
        let v = if r.unit() < 0.7 { 0.05 } else { -0.04 } * (0.5 + r.unit());
        let mut s = 0.0;
        while s < len {
            dab(&mut px, x, y, 0.6, v);
            x += angle.cos() * 0.5;
            y += angle.sin() * 0.5;
            angle += bend;
            s += 0.5;
        }
    }
    px
}

fn fabric() -> Vec<f64> {
    // A plain weave: 32 threads each way, warp on top where the thread indices add up even.
    const PITCH: usize = 8;
    let threads = N / PITCH;
    field(|x, y| {
        // Half a thread over, so the tile's edges fall mid-thread rather than on the gaps
        // between threads, where every column steps.
        let (x, y) = ((x + 4.0) % N as f64, (y + 4.0) % N as f64);
        let (i, j) = (x as usize / PITCH, y as usize / PITCH);
        let u = (x - (i * PITCH) as f64 + 0.5) / PITCH as f64;
        let v = (y - (j * PITCH) as f64 + 0.5) / PITCH as f64;
        let round = |t: f64| (std::f64::consts::PI * t).sin().powf(0.6);
        // Each thread is a little lighter or darker than the next.
        let warp = 0.85 + 0.15 * hash(i as i64, 0, 21);
        let weft = 0.85 + 0.15 * hash(0, j as i64, 22);
        let (across, along, tone) = if (i + j) % 2 == 0 { (u, v, warp) } else { (v, u, weft) };
        // The top thread is round across and dips where it goes under at the cell's ends.
        let lift = 0.7 + 0.3 * (std::f64::consts::PI * along).sin();
        let fibre = value_noise(x, y, threads * 4, 23);
        tone * round(across) * lift * (0.9 + 0.2 * fibre)
    })
}

fn rubber() -> Vec<f64> {
    let mut px = field(|x, y| fbm(x, y, &[(64, 0.5), (128, 0.35), (16, 0.15)], 31));
    let mut r = Rng(0x7275_6262_6572);
    for _ in 0..1800 {
        let (x, y) = (r.unit() * N as f64, r.unit() * N as f64);
        dab(&mut px, x, y, 0.5 + 0.7 * r.unit(), -0.35);
    }
    blur(&mut px, 1);
    px
}

fn film_grain() -> Vec<f64> {
    let mut r = Rng(0x6772_6169_6e);
    // Three uniforms a pixel sum to a rough bell curve; one blur clumps it like silver grain.
    let mut px: Vec<f64> = (0..N * N).map(|_| r.unit() + r.unit() + r.unit()).collect();
    blur(&mut px, 1);
    px
}

/// The field as bytes: centred on 128 with a spread of `spread` (one standard deviation),
/// kept within 8 to 247 so a sample is never black (an empty image slot reads 0).
fn bytes(px: &[f64], spread: f64) -> Vec<u8> {
    let n = px.len() as f64;
    let mean = px.iter().sum::<f64>() / n;
    let sd = (px.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt().max(1e-9);
    px.iter().map(|v| (128.0 + (v - mean) / sd * spread).round().clamp(8.0, 247.0) as u8).collect()
}

fn main() {
    let mut out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/textures");
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(args.next().expect("--out DIR")),
            "--check" => check = true,
            _ => panic!("unknown argument {a}: use --out DIR or --check"),
        }
    }
    let spread = |name: &str| match name {
        "fabric" => 52.0,
        "film-grain" => 34.0,
        _ => 30.0,
    };
    let mut differ = false;
    if !check {
        std::fs::create_dir_all(&out).expect("output folder");
    }
    for (name, make) in TILES {
        let tile = bytes(&make(), spread(name));
        assert_eq!(tile.len(), N * N);
        let path = out.join(format!("{name}.gray"));
        if check {
            let same = std::fs::read(&path).is_ok_and(|disk| disk == tile);
            println!("{name}: {}", if same { "same" } else { "differs" });
            differ |= !same;
        } else {
            std::fs::write(&path, &tile).expect("write tile");
            println!("wrote {}", path.display());
        }
    }
    if differ {
        std::process::exit(1);
    }
}
