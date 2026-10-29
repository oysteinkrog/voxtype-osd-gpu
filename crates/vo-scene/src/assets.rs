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
    font!(
        "extra-condensed-bold",
        "SairaExtraCondensed-ExtraBold-Latin.otf",
        "Saira ExtraCondensed ExtraBold, Latin: a heavy extra-condensed face"
    ),
    // Roboto Flex for variable font breathing (MO-81), about 300 KB. Made from
    // RobotoFlex-VF.ttf 3.200 by `fonttools varLib.instancer` (other axes dropped, wdth=75:125
    // opsz=8:72 wght=300:900), then `pyftsubset --unicodes=<ASCII, Latin-1 letters, common
    // punctuation> --layout-features=kern,liga,locl --no-hinting`; the exact command is in
    // assets/fonts/subset.sh.
    font!(
        "flex",
        "RobotoFlex-Variable-Latin.ttf",
        "Roboto Flex, Latin, variable: weight 300 to 900, width 75 to 125, optical size 8 to 72, slant -10 to 0"
    ),
    // EG-28: five more faces, about 225 KB in all, downloaded from the projects' official
    // sources and subset by assets/fonts/subset.sh (sources and licences in the README).
    font!("typewriter", "CourierPrime-Regular-Latin.ttf", "Courier Prime Regular, Latin: a typewriter face"),
    font!("display-wide-italic", "Kanit-BoldItalic-Latin.ttf", "Kanit Bold Italic, Latin: a wide, slanted display face"),
    font!(
        "ultra-condensed-heavy",
        "LeagueGothic-Condensed-Latin.ttf",
        "League Gothic at width 75, Latin: an ultra condensed heavy face"
    ),
    font!("script", "GreatVibes-Regular-Latin.ttf", "Great Vibes Regular, Latin: a formal connected script"),
    font!("music", "Bravura-Music.otf", "Bravura (SMuFL): staff lines, clefs, note heads, flags, accidentals and rests"),
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
        assert!(total < 1_800_000, "{total} bytes of fonts");
    }

    /// The SMuFL code points the `music` subset keeps (the same list as `subset.sh`).
    pub const MUSIC_CODE_POINTS: &[(u32, u32)] = &[
        (0xE010, 0xE014),
        (0xE030, 0xE031),
        (0xE080, 0xE089),
        (0xE1E7, 0xE1E7),
        (0xE050, 0xE050),
        (0xE05C, 0xE05C),
        (0xE062, 0xE062),
        (0xE0A0, 0xE0A4),
        (0xE240, 0xE243),
        (0xE260, 0xE264),
        (0xE4E0, 0xE4E5),
    ];

    #[test]
    fn eg28_faces_load_and_cover_their_text() {
        use skrifa::MetadataProvider;
        let fonts = crate::font::FontRegistry::new();
        for name in ["typewriter", "display-wide-italic", "ultra-condensed-heavy", "script", "music"] {
            fonts.asset(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            let font = skrifa::FontRef::new(get(name).unwrap().bytes).unwrap();
            let map = font.charmap();
            if name == "music" {
                for &(lo, hi) in MUSIC_CODE_POINTS {
                    for c in lo..=hi {
                        assert!(map.map(c).is_some_and(|g| g.to_u32() != 0), "music: U+{c:04X}");
                    }
                }
            } else {
                for c in "Hamburgefonstiv 0123 \u{C6}\u{D8}\u{C5}".chars() {
                    assert!(map.map(c).is_some_and(|g| g.to_u32() != 0), "{name}: {c:?}");
                }
            }
        }
    }
}
