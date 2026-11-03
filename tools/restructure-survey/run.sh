#!/bin/sh
# Regenerate the generated half of docs/restructure-survey.md for the tree at <crates dir>.
# Usage: tools/restructure-survey/run.sh <crates dir> <out dir>
# Needs python3 and tokei. Survey a committed tree (git archive) so other lanes' unsaved
# files do not change the numbers.
set -eu
here=$(dirname "$0")
python3 "$here/survey.py" "$1" > "$2/survey.json"
python3 "$here/tests_map.py" "$1" > "$2/tests.json"
python3 "$here/gen_md.py" "$1" "$2/survey.json" "$2/tests.json" > "$2/generated.md"
