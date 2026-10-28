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
# Roboto Flex (AUR ttf-roboto-flex) keeps four variation axes: wght 100 to 900, wdth 75 to
# 125, opsz 8 to 72 and slnt -10 to 0. Its other axes are pinned to their defaults.
sub RobotoFlex-Variable-Latin.ttf /usr/share/fonts/TTF/RobotoFlex-VF.ttf 0 "$latin" \
    --instance="GRAD=drop,XOPQ=drop,YOPQ=drop,XTRA=drop,YTUC=drop,YTLC=drop,YTAS=drop,YTDE=drop,YTFI=drop,wdth=75:125,opsz=8:72,wght=100:900"

# Japanese for theme labels, not for dictated text (for that, add a system face such as
# `fonts.add_system(":lang=ja")` to the fallback chain). The space, kana, CJK punctuation,
# full-width forms, and the kanji below. Face 0 of NotoSansCJK-*.ttc is the JP face. Add
# kanji here and rerun when a theme needs one.
kanji="音声入力録中作戦継続未来都市変換字幕認識処理待機完了停止開始終了警告接続送信受信準備解析出力文字言語日本時間状態信号機能同期確認通信記録再生無線有効作動起動異常正常電源系統番号零一二三四五六七八九十百千万"
cjk="U+0020,U+00A0,U+3000-303F,U+3040-309F,U+30A0-30FF,U+FF01-FF5E"
for c in $(echo "$kanji" | grep -o .); do cjk+=",U+$(printf '%04X' "'$c")"; done
sub NotoSansJP-Regular-Labels.otf /usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc 0 "$cjk"
sub NotoSansJP-Bold-Labels.otf /usr/share/fonts/noto-cjk/NotoSansCJK-Bold.ttc 0 "$cjk"
