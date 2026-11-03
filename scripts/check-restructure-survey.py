#!/usr/bin/env python3
"""Check that docs/restructure-survey.md lists every .rs file of vo-scene/src and vo-render/src.

Compares the rows between the survey-table markers with the files git tracks, and checks
each row names a known target crate. Exit 1 and print the difference when they disagree.
Usage: scripts/check-restructure-survey.py [repo root]
"""

import os
import re
import subprocess
import sys

TARGETS = {
    "vo-display", "vo-text", "vo-svg", "vo-scene core", "vo-scene widgets",
    "vo-fx-text", "vo-fx-field", "vo-fx-glass", "vo-render",
}


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..")
    doc = open(os.path.join(root, "docs", "restructure-survey.md")).read()
    table = doc.split("<!-- survey-table-start -->")[1].split("<!-- survey-table-end -->")[0]
    rows, bad = {}, []
    for line in table.splitlines():
        m = re.match(r"\| `([^`]+)` \| (\d+) \| (\d+) \| ([^|]+) \|", line)
        if m:
            rows[m.group(1)] = m.group(4).strip()
            if m.group(4).strip() not in TARGETS:
                bad.append(f"unknown target for {m.group(1)}: {m.group(4).strip()}")
    # The crates sit in crates/ today and in the workspace's vo/ after the move.
    base = next(b for b in ("crates", "vo", ".") if os.path.isdir(os.path.join(root, b, "vo-scene")))
    files = set()
    for crate in ("vo-scene", "vo-render"):
        out = subprocess.run(
            ["git", "-C", root, "ls-files", f"{base}/{crate}/src"],
            capture_output=True, text=True, check=True,
        ).stdout
        for f in out.split():
            if f.endswith(".rs"):
                files.add(os.path.relpath(f, base))
    missing = sorted(files - rows.keys())
    extra = sorted(rows.keys() - files)
    for f in missing:
        bad.append(f"in git, not in the survey: {f}")
    for f in extra:
        bad.append(f"in the survey, not in git: {f}")
    if bad:
        print("\n".join(bad))
        sys.exit(1)
    print(f"restructure survey: {len(rows)} files match git")


if __name__ == "__main__":
    main()
