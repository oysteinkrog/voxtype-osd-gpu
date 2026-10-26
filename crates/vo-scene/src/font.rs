//! The font table that text layout and the renderer share.
//!
//! `GlyphInstance::font` and `TextStyle::font` are ids into one `FontRegistry`. The host
//! makes the registry, adds the fonts, and hands clones of it to the scene's text layout
//! and to the renderer. A clone is a handle to the same table, so both sides see the same
//! ids. Ids count up from 0 in the order fonts are added and never change; fonts are never
//! removed.
//!
//! The display list stays plain data: it carries ids only, never font bytes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use skrifa::raw::{ReadError, TableProvider};
use skrifa::FontRef;

/// Index of a font in a `FontRegistry`.
pub type FontId = u32;

/// The family the overlay asks fontconfig for when nothing else is set.
pub const DEFAULT_FAMILY: &str = "Noto Sans";

/// One loaded font file (face 0), read once and shared.
pub struct Face {
    data: Arc<Vec<u8>>,
    index: u32,
    upem: u16,
    shaping: harfrust::Font,
}

impl Face {
    fn new(data: Vec<u8>) -> Result<Face, ReadError> {
        let upem = FontRef::from_index(&data, 0)?.head()?.units_per_em();
        let data = Arc::new(data);
        let blob: Arc<dyn AsRef<[u8]> + Send + Sync> = data.clone();
        // skrifa has already parsed the file, so harfrust only fails on something skrifa
        // accepted but cannot shape with.
        let shaping = harfrust::Font::new(blob, 0).ok_or(ReadError::MalformedData("harfrust rejected the font"))?;
        Ok(Face { data, index: 0, upem, shaping })
    }

    /// The font file's bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Face index in the file.
    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn units_per_em(&self) -> u16 {
        self.upem
    }

    /// A skrifa view for metrics and outlines.
    pub fn skrifa(&self) -> FontRef<'_> {
        FontRef::from_index(&self.data, self.index).expect("parsed when added")
    }

    /// The harfrust font for shaping.
    pub fn shaping(&self) -> &harfrust::Font {
        &self.shaping
    }
}

/// The shared font table. Cloning is cheap and gives a handle to the same table.
#[derive(Clone, Default)]
pub struct FontRegistry {
    faces: Arc<RwLock<Vec<Arc<Face>>>>,
}

impl FontRegistry {
    pub fn new() -> FontRegistry {
        FontRegistry::default()
    }

    /// Adds a font file (face 0) and returns its id.
    pub fn add(&self, data: Vec<u8>) -> Result<FontId, ReadError> {
        let face = Arc::new(Face::new(data)?);
        let mut faces = self.faces.write().expect("font table lock");
        faces.push(face);
        Ok(faces.len() as FontId - 1)
    }

    /// Reads and adds a font file.
    pub fn add_file(&self, path: &Path) -> Result<FontId, FontError> {
        let data = std::fs::read(path).map_err(|e| FontError::Io(path.to_path_buf(), e))?;
        self.add(data).map_err(|e| FontError::Parse(path.to_path_buf(), e))
    }

    /// Finds `family` through fontconfig and adds the file it picks.
    pub fn add_system(&self, family: &str) -> Result<FontId, FontError> {
        let path = system_font(family).ok_or_else(|| FontError::NotFound(family.to_string()))?;
        self.add_file(&path)
    }

    pub fn get(&self, id: FontId) -> Option<Arc<Face>> {
        self.faces.read().expect("font table lock").get(id as usize).cloned()
    }

    pub fn len(&self) -> usize {
        self.faces.read().expect("font table lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The faces from `from` on, for a reader that keeps its own copy of the table.
    pub fn faces_from(&self, from: usize) -> Vec<Arc<Face>> {
        self.faces.read().expect("font table lock").get(from..).unwrap_or_default().to_vec()
    }

    /// Whether two handles point at the same table.
    pub fn same_table(&self, other: &FontRegistry) -> bool {
        Arc::ptr_eq(&self.faces, &other.faces)
    }
}

impl std::fmt::Debug for FontRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontRegistry").field("fonts", &self.len()).finish()
    }
}

#[derive(Debug)]
pub enum FontError {
    Io(PathBuf, std::io::Error),
    Parse(PathBuf, ReadError),
    /// fontconfig is missing or found no file for the family.
    NotFound(String),
}

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FontError::Io(p, e) => write!(f, "cannot read font {}: {e}", p.display()),
            FontError::Parse(p, e) => write!(f, "cannot parse font {}: {e}", p.display()),
            FontError::NotFound(family) => {
                write!(f, "fontconfig found no font for '{family}'. Is fontconfig (fc-match) installed?")
            }
        }
    }
}

impl std::error::Error for FontError {}

/// The font file fontconfig picks for `family`, through `fc-match`. fontconfig falls back
/// to its closest match, so this finds a file whenever any font is installed.
///
/// `VO_FONT`, when set, is a font file path that wins over fontconfig.
pub fn system_font(family: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VO_FONT") {
        return Some(PathBuf::from(p));
    }
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", family]).output().ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
}

/// The Latin subset of Noto Sans in `assets/fonts`, for tests and goldens: it does not
/// depend on what a machine has installed.
pub fn test_font_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/NotoSans-Regular-Latin.ttf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_count_up_and_clones_share_the_table() {
        let fonts = FontRegistry::new();
        let other = fonts.clone();
        assert_eq!(fonts.add_file(&test_font_path()).unwrap(), 0);
        assert_eq!(other.add_file(&test_font_path()).unwrap(), 1);
        assert_eq!(fonts.len(), 2);
        assert!(fonts.same_table(&other));
        assert_eq!(fonts.get(1).unwrap().units_per_em(), 1000);
        assert!(fonts.get(2).is_none());
        assert_eq!(fonts.faces_from(1).len(), 1);
        assert!(fonts.faces_from(5).is_empty());
    }

    #[test]
    fn bad_data_is_an_error_and_takes_no_id() {
        let fonts = FontRegistry::new();
        assert!(fonts.add(b"not a font".to_vec()).is_err());
        assert!(fonts.is_empty());
    }
}
