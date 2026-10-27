# Bundled fonts

Small subsets of SIL Open Font License 1.1 fonts (licence text in `OFL.txt`). They are
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
| `jp-labels` | `NotoSansJP-Regular-Labels.otf` | Noto Sans CJK JP Regular 2.004 | 2014-2021 Adobe |
| `jp-labels-bold` | `NotoSansJP-Bold-Labels.otf` | Noto Sans CJK JP Bold 2.004 | 2014-2021 Adobe |

The Latin files cover ASCII, Latin-1, Latin Extended-A (Norwegian and most European
letters), dashes, quotes, the ellipsis and the euro sign. The Japanese files cover kana,
CJK punctuation, full-width forms and about 100 kanji for theme labels. They are for
labels a theme writes, not for dictated Japanese: for that, add a system face as a
fallback, for example `fonts.add_system(":lang=ja")`.

`subset.sh` remakes every file from the system fonts and holds the exact character lists.
To add a kanji a label needs, add it to the list in `subset.sh` and run it again.
