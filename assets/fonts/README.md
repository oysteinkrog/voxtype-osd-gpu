# Bundled fonts

Small subsets of SIL Open Font License 1.1 fonts (licence text in `OFL.txt`, and for the five faces added later in `OFL-<family>.txt` beside them). They are
built into the binary through `vo_scene::assets`, and a theme loads one by name with
`FontRegistry::asset("condensed-bold")`, so tests, goldens and themes never depend on the
fonts a machine has installed. Every subset keeps all layout features.

| Asset name | File | Source | Copyright |
|---|---|---|---|
| `sans` | `NotoSans-Regular-Latin.ttf` | Noto Sans Regular 2.015 | 2022 The Noto Project Authors |
| `serif` | `NotoSerif-Regular-Latin.ttf` | Noto Serif Regular | 2022 The Noto Project Authors |
| `mono` | `JetBrainsMono-Regular-Latin.ttf` | JetBrains Mono Regular 2.304 | 2020 The JetBrains Mono Project Authors |
| `grotesk` | `Inter-Regular-Latin.ttf` | Inter Regular 4.1 | 2016 The Inter Project Authors |
| `condensed` | `BarlowCondensed-Medium-Latin.ttf` | Barlow Condensed Medium 1.422 | 2017 The Barlow Project Authors |
| `condensed-bold` | `BarlowCondensed-Bold-Latin.ttf` | Barlow Condensed Bold 1.422 | 2017 The Barlow Project Authors |
| `thin` | `Barlow-Thin-Latin.ttf` | Barlow Thin 1.422 | 2017 The Barlow Project Authors |
| `extra-condensed-bold` | `SairaExtraCondensed-ExtraBold-Latin.otf` | Saira ExtraCondensed ExtraBold 1.101 | 2020 The Saira Project Authors |
| `flex` | `RobotoFlex-Variable-Latin.ttf` | Roboto Flex 3.200, variable: `wght` 300 to 900, `wdth` 75 to 125, `opsz` 8 to 72, `slnt` -10 to 0; ASCII, Latin-1 letters and common punctuation | 2017 The Roboto Flex Project Authors |
| `typewriter` | `CourierPrime-Regular-Latin.ttf` | Courier Prime Regular, google/fonts `9862abc` (`OFL-CourierPrime.txt`) | 2015 The Courier Prime Project Authors |
| `display-wide-italic` | `Kanit-BoldItalic-Latin.ttf` | Kanit Bold Italic, google/fonts `9862abc` (`OFL-Kanit.txt`) | 2020 The Kanit Project Authors |
| `ultra-condensed-heavy` | `LeagueGothic-Condensed-Latin.ttf` | League Gothic variable, `wdth` pinned to 75, google/fonts `9862abc` (`OFL-LeagueGothic.txt`) | 2010 The League Gothic Project Authors |
| `script` | `GreatVibes-Regular-Latin.ttf` | Great Vibes Regular, google/fonts `9862abc` (`OFL-GreatVibes.txt`) | 2015 The Great Vibes Pro Project Authors |
| `music` | `Bravura-Music.otf` | Bravura 1.482 (SMuFL), steinbergmedia/bravura release; only U+E010-E014, E030-E031, E080-E089, E1E7, E050, E05C, E062, E0A0-E0A4, E240-E243, E260-E264, E4E0-E4E5 (`OFL-Bravura.txt`) | 2015 Steinberg Media Technologies GmbH |
| `jp-labels` | `NotoSansJP-Regular-Labels.otf` | Noto Sans CJK JP Regular 2.004 | 2014-2021 Adobe |
| `jp-labels-bold` | `NotoSansJP-Bold-Labels.otf` | Noto Sans CJK JP Bold 2.004 | 2014-2021 Adobe |

The Latin files cover ASCII, Latin-1, Latin Extended-A (Norwegian and most European
letters), dashes, quotes, the ellipsis and the euro sign. The Japanese files cover the space, kana,
CJK punctuation, full-width forms and about 100 kanji for theme labels. They are for
labels a theme writes, not for dictated Japanese: for that, add a system face as a
fallback, for example `fonts.add_system(":lang=ja")`.

`subset.sh` remakes every file from the system fonts and holds the exact character lists.
To add a kanji a label needs, add it to the list in `subset.sh` and run it again.
