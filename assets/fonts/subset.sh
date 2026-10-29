#!/usr/bin/env bash
# Remakes the bundled font subsets in this folder from the system's font files.
# Needs hb-subset (harfbuzz-utils). Every font here is under the SIL Open Font License 1.1.
#
#   assets/fonts/subset.sh
#
# The Latin set is the same as NotoSans-Regular-Latin.ttf's: ASCII, Latin-1, Latin
# Extended-A, dashes, quotes, the ellipsis, per mille, the euro and the trade mark sign.
# Layout features stay, so kerning and ligatures still work.
set -euo pipefail
cd "$(dirname "$0")"

latin="U+0020-007E,U+00A0-017F,U+2010-2027,U+2030-203A,U+20AC,U+2122"

sub() { # out src face-index unicodes [extra args]
    local out=$1 src=$2 face=$3 codes=$4
    shift 4
    hb-subset "$src" --face-index="$face" --unicodes="$codes" --layout-features='*' --output-file="$out" "$@"
    echo "$out $(stat -c %s "$out") bytes"
}

sub NotoSerif-Regular-Latin.ttf /usr/share/fonts/noto/NotoSerif-Regular.ttf 0 "$latin"
sub JetBrainsMono-Regular-Latin.ttf /usr/share/fonts/TTF/JetBrainsMono-Regular.ttf 0 "$latin"
sub BarlowCondensed-Medium-Latin.ttf /usr/share/fonts/ttf-barlow/BarlowCondensed-Medium.ttf 0 "$latin"
sub BarlowCondensed-Bold-Latin.ttf /usr/share/fonts/ttf-barlow/BarlowCondensed-Bold.ttf 0 "$latin"
sub Barlow-Thin-Latin.ttf /usr/share/fonts/ttf-barlow/Barlow-Thin.ttf 0 "$latin"
sub SairaExtraCondensed-ExtraBold-Latin.otf /usr/share/fonts/OTF/SairaExtraCondensed-ExtraBold.otf 0 "$latin"
# Face 0 of Inter.ttc is Inter Regular.
sub Inter-Regular-Latin.ttf /usr/share/fonts/inter/Inter.ttc 0 "$latin"
# Roboto Flex (AUR ttf-roboto-flex) keeps the four variation axes MO-81 drives: wght 300 to
# 900, wdth 75 to 125, opsz 8 to 72 and slnt -10 to 0; its other axes are pinned to their
# defaults. To stay inside the size budget it takes a smaller set than the other Latin files
# (ASCII, the Latin-1 letters, a few symbols, dashes, quotes, the ellipsis, the euro and the
# trade mark sign), no hinting and only kern, liga and locl. Needs fonttools (pacman
# python-fonttools).
flex_set="U+0020-007E,U+00A0,U+00A7,U+00A9,U+00AB,U+00AE,U+00B0,U+00B1,U+00B7,U+00BB,U+00BF-00FF,U+2013,U+2014,U+2018-201E,U+2022,U+2026,U+2030,U+2039,U+203A,U+20AC,U+2122"
flex_tmp=$(mktemp --suffix=.ttf -p "$PWD")
fonttools varLib.instancer /usr/share/fonts/TTF/RobotoFlex-VF.ttf GRAD=drop XOPQ=drop YOPQ=drop \
    XTRA=drop YTUC=drop YTLC=drop YTAS=drop YTDE=drop YTFI=drop wdth=75:125 opsz=8:72 wght=300:900 \
    -o "$flex_tmp" -q
pyftsubset "$flex_tmp" --unicodes="$flex_set" --layout-features='kern,liga,locl' --no-hinting \
    --name-IDs='0,1,2,3,4,5,6,13,14' --output-file=RobotoFlex-Variable-Latin.ttf
rm -f "$flex_tmp"
echo "RobotoFlex-Variable-Latin.ttf $(stat -c %s RobotoFlex-Variable-Latin.ttf) bytes"

# Japanese for theme labels, not for dictated text (for that, add a system face such as
# `fonts.add_system(":lang=ja")` to the fallback chain). The space, kana, CJK punctuation,
# full-width forms, and the kanji below. Face 0 of NotoSansCJK-*.ttc is the JP face. Add
# kanji here and rerun when a theme needs one.
kanji="音声入力録中作戦継続未来都市変換字幕認識処理待機完了停止開始終了警告接続送信受信準備解析出力文字言語日本時間状態信号機能同期確認通信記録再生無線有効作動起動異常正常電源系統番号零一二三四五六七八九十百千万"
cjk="U+0020,U+00A0,U+3000-303F,U+3040-309F,U+30A0-30FF,U+FF01-FF5E"
for c in $(echo "$kanji" | grep -o .); do cjk+=",U+$(printf '%04X' "'$c")"; done
sub NotoSansJP-Regular-Labels.otf /usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc 0 "$cjk"
sub NotoSansJP-Bold-Labels.otf /usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc 0 "$cjk"

# EG-28: five faces that no system package provides here, so they are downloaded from the
# project's official source and checked before use. Google Fonts files are checked against
# their git blob hash at the google/fonts commit below; Bravura against the sha256 that
# GitHub shows for the steinbergmedia/bravura release. Each family keeps its own OFL-<name>.txt.
# Set VO_FONT_CACHE to reuse a download folder.
gf_commit=9862abc066c9b83bf84c3e8ed1fc2d8d48f9bbf0
cache=${VO_FONT_CACHE:-$(mktemp -d)}
mkdir -p "$cache"
fetch_gf() { # file url-path git-blob-sha
    [ -f "$cache/$1" ] || curl -sfL "https://raw.githubusercontent.com/google/fonts/$gf_commit/ofl/$2" -o "$cache/$1"
    [ "$(git hash-object "$cache/$1")" = "$3" ] || { echo "hash mismatch: $1" >&2; exit 1; }
}
fetch_gf CourierPrime-Regular.ttf courierprime/CourierPrime-Regular.ttf 2a0f8bde9778bb8c7b3a0ca5d4b4fb041737ef1a
fetch_gf Kanit-BoldItalic.ttf kanit/Kanit-BoldItalic.ttf 9ce084eb75d072a005f39727bee85ad1c8278fcb
fetch_gf LeagueGothic-wdth.ttf 'leaguegothic/LeagueGothic%5Bwdth%5D.ttf' 2543bb2aba1744199f3e59a3fd5b55bd219d6644
fetch_gf GreatVibes-Regular.ttf greatvibes/GreatVibes-Regular.ttf a1327ff3b862246f1f253639040edcc473e5f3a9
bravura_sha=cdf0f893ee1fdb64b7f6713d71ee0dcfc349c0ac01429a8e451b01a9e79f5f3b
[ -f "$cache/Bravura.otf" ] || curl -sfL https://github.com/steinbergmedia/bravura/releases/download/bravura-1.482/Bravura.otf -o "$cache/Bravura.otf"
[ "$(sha256sum "$cache/Bravura.otf" | cut -d' ' -f1)" = "$bravura_sha" ] || { echo "hash mismatch: Bravura.otf" >&2; exit 1; }

sub CourierPrime-Regular-Latin.ttf "$cache/CourierPrime-Regular.ttf" 0 "$latin" --no-hinting
sub Kanit-BoldItalic-Latin.ttf "$cache/Kanit-BoldItalic.ttf" 0 "$latin" --no-hinting
sub GreatVibes-Regular-Latin.ttf "$cache/GreatVibes-Regular.ttf" 0 "$latin" --no-hinting
# League Gothic is variable in width (75 to 100); pin the narrowest, the ultra condensed cut.
lg_tmp=$(mktemp --suffix=.ttf -p "$PWD")
fonttools varLib.instancer "$cache/LeagueGothic-wdth.ttf" wdth=75 -o "$lg_tmp" -q
sub LeagueGothic-Condensed-Latin.ttf "$lg_tmp" 0 "$latin" --no-hinting
rm -f "$lg_tmp"
# Bravura (SMuFL) for the pitch staff: staff lines, barlines, time signature digits, the
# augmentation dot, clefs (G, C, F), note heads, flags, accidentals and rests. Needs the
# list in the README to stay in step with this one.
music="U+E010-E014,U+E030-E031,U+E080-E089,U+E1E7,U+E050,U+E05C,U+E062,U+E0A0-E0A4,U+E240-E243,U+E260-E264,U+E4E0-E4E5"
sub Bravura-Music.otf "$cache/Bravura.otf" 0 "$music" --no-hinting
