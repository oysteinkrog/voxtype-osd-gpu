//! The font table that text layout and the renderer share.
//!
//! `GlyphInstance::font` and `TextStyle::font` are ids into one `FontRegistry`. The host
//! makes the registry, adds the fonts, and hands clones of it to the scene's text layout
//! and to the renderer. A clone is a handle to the same table, so both sides see the same
//! ids. Ids count up from 0 in the order fonts are added and never change; fonts are never
//! removed.
//!
//! The display list stays plain data: it carries ids only, never font bytes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use skrifa::instance::{LocationRef, Size};
use skrifa::raw::{ReadError, TableProvider};
use skrifa::{FontRef, MetadataProvider};

#[path = "motion/var_axes.rs"]
pub mod var_axes;

/// Index of a font in a `FontRegistry`.
pub type FontId = u32;

/// The family the overlay asks fontconfig for when nothing else is set.
pub const DEFAULT_FAMILY: &str = "Noto Sans";

type Bytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// One loaded face of a font file, read once and shared.
pub struct Face {
    data: Bytes,
    index: u32,
    upem: u16,
    shaping: harfrust::Font,
    /// The variation location of an instance (`FontRegistry::add_instance`); empty for the
    /// font's default.
    coords: Vec<skrifa::instance::NormalizedCoord>,
    /// Shape plans compiled for this face, one per script, direction, language and
    /// feature set (`Face::plan`). A face sees few of these, so a list is enough.
    plans: Mutex<Vec<Arc<harfrust::ShapePlan>>>,
    /// How many plans `Face::plan` has compiled, for tests.
    plans_compiled: AtomicUsize,
}

impl Face {
    fn new(data: Bytes, index: u32) -> Result<Face, ReadError> {
        let upem = FontRef::from_index((*data).as_ref(), index)?.head()?.units_per_em();
        // skrifa has already parsed the file, so harfrust only fails on something skrifa
        // accepted but cannot shape with.
        let shaping =
            harfrust::Font::new(data.clone(), index).ok_or(ReadError::MalformedData("harfrust rejected the font"))?;
        Ok(Face {
            data,
            index,
            upem,
            shaping,
            coords: Vec::new(),
            plans: Mutex::default(),
            plans_compiled: AtomicUsize::new(0),
        })
    }

    /// The same face at a variation location, from axis values in user units (`wght` 700,
    /// `wdth` 90). Axes the font lacks are ignored; omitted axes keep their defaults.
    fn instance(&self, axes: &[([u8; 4], f32)]) -> Face {
        let tags = axes.iter().map(|&(tag, value)| (harfrust::Tag::new(&tag), value));
        let shaping = self.shaping.instance_builder().variations(tags).build();
        let coords = shaping.normalized_coords().to_vec();
        Face {
            data: self.data.clone(),
            index: self.index,
            upem: self.upem,
            shaping,
            coords,
            plans: Mutex::default(),
            plans_compiled: AtomicUsize::new(0),
        }
    }

    /// The variation location metrics and outlines are read at.
    pub fn location(&self) -> LocationRef<'_> {
        LocationRef::new(&self.coords)
    }

    /// The font's variation axes: tag, minimum, default and maximum in user units.
    pub fn axes(&self) -> Vec<([u8; 4], f32, f32, f32)> {
        self.skrifa()
            .axes()
            .iter()
            .map(|a| (a.tag().to_be_bytes(), a.min_value(), a.default_value(), a.max_value()))
            .collect()
    }

    /// The font file's bytes.
    pub fn data(&self) -> &[u8] {
        (*self.data).as_ref()
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
        FontRef::from_index(self.data(), self.index).expect("parsed when added")
    }

    /// Whether the face has a glyph for `c`.
    pub fn has(&self, c: char) -> bool {
        self.skrifa().charmap().map(c).is_some_and(|g| g.to_u32() != 0)
    }

    /// The harfrust font for shaping.
    pub fn shaping(&self) -> &harfrust::Font {
        &self.shaping
    }

    /// The shape plan for text with these segment properties and features, compiled on
    /// first use and kept. Compiling a plan collects and sorts the font's lookups, which
    /// costs more than shaping a short line, so text laid out again reuses it.
    pub fn plan(
        &self,
        direction: harfrust::Direction,
        script: Option<harfrust::Script>,
        language: Option<&harfrust::Language>,
        features: &[harfrust::Feature],
    ) -> Arc<harfrust::ShapePlan> {
        let key = harfrust::ShapePlanKey::new(&self.shaping, script, direction).language(language).features(features);
        let mut plans = self.plans.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(plan) = plans.iter().find(|p| key.matches(p)) {
            return plan.clone();
        }
        let plan = Arc::new(harfrust::ShapePlan::new(&self.shaping, direction, script, language, features));
        self.plans_compiled.fetch_add(1, Ordering::Relaxed);
        plans.push(plan.clone());
        plan
    }

    /// How many shape plans this face has compiled.
    pub fn plans_compiled(&self) -> usize {
        self.plans_compiled.load(Ordering::Relaxed)
    }

    /// The font's own line height at `size`: ascent plus descent, without the line gap,
    /// rounded up to a whole unit. This is the height Pango gives a line of this font, so
    /// text set with it takes the room it takes in GTK. skrifa reads the ascent and descent
    /// from `hhea`, or from `OS/2` when the font asks for its typo metrics, as HarfBuzz does.
    pub fn line_height(&self, size: f32) -> f32 {
        let m = self.skrifa().metrics(Size::new(size), self.location());
        // The tolerance keeps a height that is whole up to float error from rounding up.
        (m.ascent - m.descent - 1e-3).ceil()
    }
}

/// The shared font table. Cloning is cheap and gives a handle to the same table.
///
/// Fallback (TY05): text in a font that lacks a character is set in the first face of the
/// font's fallback chain that has it, run by run. A font's own chain (`set_fallbacks`)
/// wins over the table's default chain (`set_default_fallbacks`).
#[derive(Clone, Default)]
pub struct FontRegistry {
    faces: Arc<RwLock<Vec<Arc<Face>>>>,
    /// Filled paths, drawn as glyphs of `path::PATH_FONT` (`add_path`).
    paths: Arc<RwLock<Vec<Arc<kurbo::BezPath>>>>,
    fallback: Arc<RwLock<Fallback>>,
    /// Font files already read, by path: the faces of one collection share its bytes.
    files: Arc<RwLock<HashMap<PathBuf, Bytes>>>,
    /// Instances made by `add_instance`, by base font and axis values in hundredths.
    instances: Arc<RwLock<HashMap<InstanceKey, FontId>>>,
}

/// A variable font instance: its base font and its axis values (tag, hundredths).
type InstanceKey = (FontId, Vec<([u8; 4], i32)>);

#[derive(Default)]
struct Fallback {
    chains: HashMap<FontId, Arc<[FontId]>>,
    default: Arc<[FontId]>,
    /// Ids of the built-in assets already added, by name.
    assets: HashMap<&'static str, FontId>,
}

impl FontRegistry {
    pub fn new() -> FontRegistry {
        FontRegistry::default()
    }

    /// Adds a font file (face 0) and returns its id.
    pub fn add(&self, data: Vec<u8>) -> Result<FontId, ReadError> {
        self.add_face(Arc::new(data), 0)
    }

    /// Adds face `index` of a font file or collection and returns its id.
    pub fn add_face(&self, data: Bytes, index: u32) -> Result<FontId, ReadError> {
        let face = Arc::new(Face::new(data, index)?);
        let mut faces = self.faces.write().expect("font table lock");
        faces.push(face);
        Ok(faces.len() as FontId - 1)
    }

    /// Reads and adds a font file.
    pub fn add_file(&self, path: &Path) -> Result<FontId, FontError> {
        self.add_file_face(path, 0)
    }

    /// Reads and adds face `index` of a font file or collection (`.ttc`).
    /// A file is read once: a second face of it, or the same face again, shares the bytes,
    /// so the weights of a 13 MB `.ttc` cost 13 MB and not 13 MB each.
    pub fn add_file_face(&self, path: &Path, index: u32) -> Result<FontId, FontError> {
        let cached = self.files.read().expect("font file lock").get(path).cloned();
        let data = match cached {
            Some(data) => data,
            None => {
                let data: Bytes = Arc::new(std::fs::read(path).map_err(|e| FontError::Io(path.to_path_buf(), e))?);
                self.files.write().expect("font file lock").insert(path.to_path_buf(), data.clone());
                data
            }
        };
        self.add_face(data, index).map_err(|e| FontError::Parse(path.to_path_buf(), e))
    }

    /// Finds `pattern` through fontconfig and adds the face it picks. The pattern is a
    /// family name ("Noto Sans") or any fontconfig pattern (":lang=ja" for a Japanese font).
    pub fn add_system(&self, pattern: &str) -> Result<FontId, FontError> {
        let (path, index) = system_face(pattern).ok_or_else(|| FontError::NotFound(pattern.to_string()))?;
        self.add_file_face(&path, index)
    }

    /// The built-in font asset `name` (`assets::ASSETS`), added on first use. Asking again
    /// gives the same id.
    pub fn asset(&self, name: &str) -> Result<FontId, FontError> {
        let asset = crate::assets::get(name)
            .filter(|a| a.kind == crate::assets::AssetKind::Font)
            .ok_or_else(|| FontError::NoAsset(name.to_string()))?;
        let mut fb = self.fallback.write().expect("fallback lock");
        if let Some(&id) = fb.assets.get(asset.name) {
            return Ok(id);
        }
        let id = self.add_face(Arc::new(asset.bytes), 0).map_err(|e| FontError::Parse(PathBuf::from(asset.name), e))?;
        fb.assets.insert(asset.name, id);
        Ok(id)
    }

    /// A variable font `base` at a variation location, from axis values in user units, for
    /// example `[(*b"wght", 700.0), (*b"wdth", 90.0)]`. The instance shares the file's bytes
    /// and the base font's fallback chain; asking again for the same values gives the same id,
    /// so a caller can ask every frame. Values are kept to a hundredth. `None` when there is
    /// no such font.
    pub fn add_instance(&self, base: FontId, axes: &[([u8; 4], f32)]) -> Option<FontId> {
        let key: Vec<([u8; 4], i32)> = axes.iter().map(|&(t, v)| (t, (v * 100.0).round() as i32)).collect();
        if let Some(&id) = self.instances.read().expect("instance lock").get(&(base, key.clone())) {
            return Some(id);
        }
        let face = self.get(base)?;
        let axes: Vec<([u8; 4], f32)> = key.iter().map(|&(t, v)| (t, v as f32 / 100.0)).collect();
        let instance = Arc::new(face.instance(&axes));
        let id = {
            let mut faces = self.faces.write().expect("font table lock");
            faces.push(instance);
            faces.len() as FontId - 1
        };
        let chain = self.fallbacks(base);
        self.fallback.write().expect("fallback lock").chains.insert(id, chain);
        self.instances.write().expect("instance lock").insert((base, key), id);
        Some(id)
    }

    /// Sets the faces tried, in order, for characters font `primary` lacks.
    pub fn set_fallbacks(&self, primary: FontId, chain: &[FontId]) {
        let chain: Vec<FontId> = chain.iter().copied().filter(|&f| f != primary).collect();
        self.fallback.write().expect("fallback lock").chains.insert(primary, chain.into());
    }

    /// Sets the faces tried for any font that has no chain of its own.
    pub fn set_default_fallbacks(&self, chain: &[FontId]) {
        self.fallback.write().expect("fallback lock").default = chain.into();
    }

    /// The faces tried after `primary`, in order: its own chain or the default one.
    pub fn fallbacks(&self, primary: FontId) -> Arc<[FontId]> {
        let fb = self.fallback.read().expect("fallback lock");
        fb.chains.get(&primary).unwrap_or(&fb.default).clone()
    }

    /// The face text in `primary` uses for `c`: `primary` when it has the character, else
    /// the first fallback that has it, else `primary` (which then draws its missing-glyph
    /// box).
    pub fn face_for(&self, primary: FontId, c: char) -> FontId {
        let has = |id: FontId| self.get(id).is_some_and(|f| f.has(c));
        if has(primary) {
            return primary;
        }
        self.fallbacks(primary).iter().copied().find(|&id| id != primary && has(id)).unwrap_or(primary)
    }

    pub fn get(&self, id: FontId) -> Option<Arc<Face>> {
        self.faces.read().expect("font table lock").get(id as usize).cloned()
    }

    /// `Face::line_height` of font `id` at `size`, or `None` when there is no such font.
    pub fn line_height(&self, id: FontId, size: f32) -> Option<f32> {
        self.get(id).map(|face| face.line_height(size))
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

    /// Adds a path to fill through the glyph shader and returns its id, the `glyph` of a
    /// `GlyphInstance` with font `path::PATH_FONT`. Path units are y down; keep coordinates
    /// within +-1000 units.
    pub fn add_path(&self, path: kurbo::BezPath) -> u32 {
        let mut paths = self.paths.write().expect("path table lock");
        paths.push(Arc::new(path));
        paths.len() as u32 - 1
    }

    pub fn path(&self, id: u32) -> Option<Arc<kurbo::BezPath>> {
        self.paths.read().expect("path table lock").get(id as usize).cloned()
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
    /// No built-in font asset has this name.
    NoAsset(String),
}

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FontError::Io(p, e) => write!(f, "cannot read font {}: {e}", p.display()),
            FontError::Parse(p, e) => write!(f, "cannot parse font {}: {e}", p.display()),
            FontError::NotFound(family) => {
                write!(f, "fontconfig found no font for '{family}'. Is fontconfig (fc-match) installed?")
            }
            FontError::NoAsset(name) => {
                let names: Vec<_> = crate::assets::names(crate::assets::AssetKind::Font).collect();
                write!(f, "no built-in font called '{name}'. Built-in fonts: {}", names.join(", "))
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
    system_face(family).map(|(path, _)| path)
}

/// The file and face index fontconfig picks for `pattern`. The index matters for font
/// collections (`.ttc`), such as Noto Sans CJK, whose faces share one file.
///
/// `VO_FONT`, when set, is a font file path (face 0) that wins over fontconfig.
pub fn system_face(pattern: &str) -> Option<(PathBuf, u32)> {
    if let Some(p) = std::env::var_os("VO_FONT") {
        return Some((PathBuf::from(p), 0));
    }
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}\n%{index}", pattern]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let path = lines.next()?.trim();
    let index = lines.next().and_then(|i| i.trim().parse().ok()).unwrap_or(0);
    (out.status.success() && !path.is_empty()).then(|| (PathBuf::from(path), index))
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
    fn faces_of_one_file_share_its_bytes() {
        let fonts = FontRegistry::new();
        let a = fonts.add_file(&test_font_path()).unwrap();
        let b = fonts.clone().add_file(&test_font_path()).unwrap();
        assert_ne!(a, b);
        let (fa, fb) = (fonts.get(a).unwrap(), fonts.get(b).unwrap());
        assert_eq!(fa.data().as_ptr(), fb.data().as_ptr());
    }

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
    fn line_height_is_ascent_plus_descent_rounded_up() {
        let fonts = FontRegistry::new();
        fonts.add_file(&test_font_path()).unwrap();
        // Noto Sans: ascent 1069, descent 293 per 1000 units, no line gap.
        assert_eq!(fonts.line_height(0, 17.0 * 4.0 / 3.0), Some(31.0));
        assert_eq!(fonts.line_height(0, 10.0), Some(14.0));
        // 1362 units at 1000 / 1362 * 50 px make exactly 50 px: no rounding up.
        assert_eq!(fonts.line_height(0, 50_000.0 / 1362.0), Some(50.0));
        assert_eq!(fonts.line_height(1, 10.0), None);
    }

    #[test]
    fn assets_load_once_by_name() {
        let fonts = FontRegistry::new();
        let a = fonts.asset("condensed-bold").unwrap();
        assert_eq!(fonts.asset("condensed-bold").unwrap(), a);
        let b = fonts.asset("jp-labels").unwrap();
        assert_ne!(a, b);
        assert_eq!(fonts.len(), 2);
        assert!(matches!(fonts.asset("nope"), Err(FontError::NoAsset(_))));
        assert!(fonts.asset("nope").unwrap_err().to_string().contains("condensed-bold"));
    }

    #[test]
    fn face_for_walks_the_chain() {
        let fonts = FontRegistry::new();
        let latin = fonts.asset("sans").unwrap();
        let jp = fonts.asset("jp-labels").unwrap();
        let mono = fonts.asset("mono").unwrap();
        // No chain: the primary, even for a character it lacks.
        assert_eq!(fonts.face_for(latin, '音'), latin);
        fonts.set_default_fallbacks(&[mono, jp]);
        assert_eq!(fonts.face_for(latin, 'a'), latin);
        assert_eq!(fonts.face_for(latin, '音'), jp);
        // A font's own chain wins over the default; none of these has U+0E01.
        fonts.set_fallbacks(latin, &[latin, mono]);
        assert_eq!(&*fonts.fallbacks(latin), &[mono]);
        assert_eq!(fonts.face_for(latin, '音'), latin);
        assert_eq!(fonts.face_for(jp, '\u{0e01}'), jp);
    }

    #[test]
    fn bad_data_is_an_error_and_takes_no_id() {
        let fonts = FontRegistry::new();
        assert!(fonts.add(b"not a font".to_vec()).is_err());
        assert!(fonts.is_empty());
    }
}
