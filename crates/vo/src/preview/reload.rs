//! Live reload inside the preview: TOML tokens and WGSL shaders, watched by modification time.
//!
//! A token file is flat or nested TOML. Numbers become number tokens and `"#rrggbb"` or
//! `"#rrggbbaa"` strings become colour tokens; nested tables join their keys with dots:
//!
//! ```toml
//! rise = 90
//! [color]
//! accent = "#ff5a36"   # the token "color.accent"
//! ```

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vo_scene::geom::Color;

use crate::Tokens;

/// Parses a token file.
pub fn parse_tokens(text: &str) -> Result<Tokens, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let mut tokens = Tokens::default();
    walk(&table, "", &mut tokens)?;
    Ok(tokens)
}

fn walk(table: &toml::Table, prefix: &str, out: &mut Tokens) -> Result<(), String> {
    for (k, v) in table {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            toml::Value::Integer(i) => {
                out.numbers.insert(key, *i as f32);
            }
            toml::Value::Float(f) => {
                out.numbers.insert(key, *f as f32);
            }
            toml::Value::Boolean(b) => {
                out.numbers.insert(key, if *b { 1.0 } else { 0.0 });
            }
            toml::Value::String(s) => {
                let c = parse_color(s).ok_or_else(|| format!("{key}: \"{s}\" is not a #rrggbb or #rrggbbaa colour"))?;
                out.colors.insert(key, c);
            }
            toml::Value::Table(t) => walk(t, &key, out)?,
            _ => return Err(format!("{key}: only numbers, booleans, colours and tables are tokens")),
        }
    }
    Ok(())
}

fn parse_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    let v = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(Color::hex(v, 1.0)),
        8 => Some(Color::hex(v >> 8, (v & 0xff) as f32 / 255.0)),
        _ => None,
    }
}

/// Files watched by modification time. Polling every few hundred ms costs nothing at this scale
/// and needs no inotify dependency.
#[derive(Default)]
pub struct Watch {
    files: Vec<(PathBuf, Option<SystemTime>)>,
}

impl Watch {
    /// Watches `path`, a file, or every file with extension `ext` directly in a directory.
    pub fn add(&mut self, path: &Path, ext: Option<&str>) {
        if path.is_dir() {
            let Ok(dir) = std::fs::read_dir(path) else { return };
            let mut found: Vec<PathBuf> = dir
                .flatten()
                .map(|e| e.path())
                .filter(|p| ext.is_none_or(|x| p.extension().is_some_and(|e| e == x)))
                .collect();
            found.sort();
            for p in found {
                self.add(&p, None);
            }
        } else {
            let m = mtime(path);
            self.files.push((path.to_path_buf(), m));
        }
    }

    /// The files that changed since the last call.
    pub fn changed(&mut self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for (p, seen) in &mut self.files {
            let now = mtime(p);
            if now != *seen {
                *seen = now;
                out.push(p.clone());
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_from_nested_toml() {
        let t =
            parse_tokens("rise = 90\nscale = 1.5\nbold = true\n[color]\naccent = \"#ff5a36\"\ndim = \"#00000080\"\n")
                .unwrap();
        assert_eq!(t.num("rise", 0.0), 90.0);
        assert_eq!(t.num("scale", 0.0), 1.5);
        assert_eq!(t.num("bold", 0.0), 1.0);
        assert_eq!(t.color("color.accent", Color::WHITE), Color::hex(0xff5a36, 1.0));
        assert!((t.color("color.dim", Color::WHITE).a - 128.0 / 255.0).abs() < 1e-6);
        assert!(parse_tokens("x = \"red\"").unwrap_err().contains("x:"));
        assert!(parse_tokens("x = [1]").is_err());
    }
}
