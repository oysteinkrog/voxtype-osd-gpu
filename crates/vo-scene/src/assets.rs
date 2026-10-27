//! Files built into the binary: fonts now, small textures and colour tables when a theme
//! needs one. A theme asks for an asset by name and never depends on what a machine has
//! installed.
//!
//! The fonts are Latin subsets (and one Japanese label subset) of SIL Open Font License
//! fonts, about 1 MB in all. `assets/fonts/subset.sh` remakes them and `assets/fonts/README.md`
//! lists their sources and copyrights. `FontRegistry::asset` loads one by name.

/// What an asset holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    /// A TrueType or OpenType file, face 0.
    Font,
    /// A raw texture.
    Texture,
    /// A colour look-up table.
    Lut,
}

/// One built-in file.
#[derive(Debug)]
pub struct Asset {
    pub name: &'static str,
    pub kind: AssetKind,
    pub bytes: &'static [u8],
    /// What it is, for tools and the contact sheet.
    pub about: &'static str,
}

macro_rules! font {
    ($name:literal, $file:literal, $about:literal) => {
        Asset {
            name: $name,
            kind: AssetKind::Font,
            bytes: include_bytes!(concat!("../../../assets/fonts/", $file)),
            about: $about,
        }
    };
}

/// Every built-in asset.
pub static ASSETS: &[Asset] = &[
    font!("sans", "NotoSans-Regular-Latin.ttf", "Noto Sans Regular, Latin"),
    font!("serif", "NotoSerif-Regular-Latin.ttf", "Noto Serif Regular, Latin"),
    font!("mono", "JetBrainsMono-Regular-Latin.ttf", "JetBrains Mono Regular, Latin"),
    font!("grotesk", "Inter-Regular-Latin.ttf", "Inter Regular, Latin: a Helvetica-like grotesk"),
    font!("condensed", "BarlowCondensed-Medium-Latin.ttf", "Barlow Condensed Medium, Latin"),
    font!("condensed-bold", "BarlowCondensed-Bold-Latin.ttf", "Barlow Condensed Bold, Latin"),
    font!("thin", "Barlow-Thin-Latin.ttf", "Barlow Thin, Latin: a thin geometric sans"),
    font!("jp-labels", "NotoSansJP-Regular-Labels.otf", "Noto Sans CJK JP Regular: kana and label kanji"),
    font!("jp-labels-bold", "NotoSansJP-Bold-Labels.otf", "Noto Sans CJK JP Bold: kana and label kanji"),
];

/// The built-in asset called `name`.
pub fn get(name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.name == name)
}

/// The names of the built-in assets of one kind.
pub fn names(kind: AssetKind) -> impl Iterator<Item = &'static str> {
    ASSETS.iter().filter(move |a| a.kind == kind).map(|a| a.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_found() {
        for (i, a) in ASSETS.iter().enumerate() {
            assert!(ASSETS[..i].iter().all(|b| b.name != a.name), "{} twice", a.name);
            assert!(std::ptr::eq(get(a.name).unwrap(), a));
            assert!(!a.bytes.is_empty());
        }
        assert!(get("no-such-asset").is_none());
        assert!(names(AssetKind::Font).any(|n| n == "condensed-bold"));
    }

    #[test]
    fn every_font_parses_and_stays_small() {
        use skrifa::raw::TableProvider;
        let mut total = 0;
        for a in ASSETS.iter().filter(|a| a.kind == AssetKind::Font) {
            let font = skrifa::FontRef::new(a.bytes).unwrap_or_else(|e| panic!("{}: {e}", a.name));
            assert!(font.head().is_ok(), "{}", a.name);
            total += a.bytes.len();
        }
        // The table is meant to stay small: every binary that uses it carries it.
        assert!(total < 1_500_000, "{total} bytes of fonts");
    }
}
