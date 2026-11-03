#!/usr/bin/env python3
"""Emit the generated sections of docs/restructure-survey.md.

Usage: gen_md.py <vo crates dir> <survey.json> <tests.json>   (prints markdown)
"""
import json, os, subprocess, sys

CR, D, T = sys.argv[1], json.load(open(sys.argv[2])), json.load(open(sys.argv[3]))
out = []
w = out.append

ORDER = ["vo-display", "vo-text", "vo-svg", "vo-scene core", "vo-scene widgets", "vo-fx-text", "vo-fx-field", "vo-fx-glass", "vo-render"]


def tokei(path):
    r = subprocess.run(["tokei", "-f", "-o", "json", path], capture_output=True, text=True).stdout
    d = json.loads(r).get("Rust", {"reports": []})["reports"]
    return {os.path.relpath(x["name"], path): (x["stats"]["code"], x["stats"]["code"] + x["stats"]["comments"] + x["stats"]["blanks"]) for x in d}


w("## Crate sizes today\n")
w("Rust lines per crate from `tokei`, `src/` only, then `tests/` and `examples/` where present. Code lines leave out comments and blank lines.\n")
w("| Crate | src files | src code | src lines | tests code | examples code |")
w("|---|---:|---:|---:|---:|---:|")
crates = sorted(os.listdir(CR))
appendix = {}
for c in crates:
    s = tokei(os.path.join(CR, c, "src"))
    t = tokei(os.path.join(CR, c, "tests")) if os.path.isdir(os.path.join(CR, c, "tests")) else {}
    e = tokei(os.path.join(CR, c, "examples")) if os.path.isdir(os.path.join(CR, c, "examples")) else {}
    w(f"| {c} | {len(s)} | {sum(v[0] for v in s.values())} | {sum(v[1] for v in s.values())} | {sum(v[0] for v in t.values()) or '-'} | {sum(v[0] for v in e.values()) or '-'} |")
    if c not in ("vo-scene", "vo-render"):
        appendix[c] = s
w("")

w("## Totals per target crate\n")
w("Lines of `vo-scene/src` and `vo-render/src` that each target crate receives. A split file counts toward each target by the share of its lines the items take (see the split column below), so these are estimates within a few percent. `vo-scene widgets` stays in the `vo-scene` crate.\n")
w("| Target | code lines | lines | Bead that moves it |")
w("|---|---:|---:|---|")
bead = {"vo-display": "V-03", "vo-text": "V-04", "vo-svg": "V-05", "vo-scene core": "stays (V-06 adds the effect API)", "vo-scene widgets": "stays (open question 1)", "vo-fx-text": "V-07", "vo-fx-field": "V-08", "vo-fx-glass": "V-09", "vo-render": "stays (imports change in V-03, V-04)"}
for t in ORDER:
    c, l = D["per_target"].get(t, [0, 0])
    w(f"| {t} | {c} | {l} | {bead[t]} |")
w("")

w("## Every file and its target\n")
w("One row per `.rs` file under `vo-scene/src` and `vo-render/src`. `scripts/check-restructure-survey.py` checks that this table and the files in git match. Split: the file's items go to more than one crate; the item lists are in the split table below.\n")
w("<!-- survey-table-start -->")
w("| File | code | lines | Target | Split |")
w("|---|---:|---:|---|---|")
for f, code, total, tgt, split in D["file_rows"]:
    w(f"| `{f}` | {code} | {total} | {tgt} | {split} |")
w("<!-- survey-table-end -->")
w("")

w("## Target crate graph\n")
w("Each edge counts the distinct items one target uses from another, found from `crate::` and `vo_scene::` paths outside test modules. A `use` at the top of a split file counts for the items whose bodies name the import.\n")
w("| From | To | Items | Examples |")
w("|---|---|---:|---|")
for k, v in D["edges"].items():
    a, b = k.split(" -> ")
    mark = " (upward)" if k in D["inversions"] else ""
    w(f"| {a} | {b}{mark} | {len(v)} | {', '.join('`'+x+'`' for x in v[:3])} |")
w("")
w(f"Cycles after the upward edges are inverted: {D['cycles_after_inversion'] or 'none'}. Cycles in today's code if the files were moved as they are: {len(D['cycles_today'])} (" + "; ".join(" -> ".join(c) for c in D["cycles_today"]) + ").\n")
w("Edges from `vo-render` or `vo-display` to `vo-scene` (core or widgets) or to any `vo-fx-*` crate: " + (", ".join(D["render_to_scene"]) or "none") + ".\n")

w("### Upward edges the effect API must invert\n")
w("Today the core calls into these effect items directly. After the split the core cannot name them, so each one becomes a call through the effect API (V-06): the effect registers, the core calls a trait method.\n")
for k in D["inversions"]:
    if "widgets" in k:
        continue
    w(f"- **{k}** ({len(D['edges'][k])}): " + ", ".join(f"`{x}`" for x in D["edges"][k]))
w("- **vo-display -> vo-svg** (1, found by hand): `image::decode` and `image::decode_bytes` call `svg()`. vo-display gets an SVG decoder hook that vo-svg fills.")
w("")
w("`vo-scene core -> vo-scene widgets` is listed as upward too, but both sides stay in one crate, so it needs no change now.\n")

w("## Effect modules that use crate-private items\n")
pu = D["priv_use"]
w(f"**{len(pu)} effect modules** use a `pub(crate)` item of a module that stays behind. That is 10 or fewer, so by the V-06 split rule V-06 rewires all of them and V-P105 is not needed. Read the caveat in the decision record: the {sum(len(D['edges'][k]) for k in D['inversions'] if 'widgets' not in k)} upward items above are the larger part of V-06's work and this count does not measure them.\n")
w("| Effect module | Family | Items | Crate-private items used |")
w("|---|---|---:|---|")
fam = {r[0].split("/src/")[1][:-3]: r[3] for r in D["file_rows"] if r[0].startswith("vo-scene/")}
for m, items in pu.items():
    w(f"| `{m}` | {fam[m]} | {len(items)} | {', '.join('`'+i+'`' for i in items)} |")
w("")
w("Method: a top-level `pub(crate)` item counts when the effect module names it by path; a `pub(crate)` field or method counts when the effect module names the declaring module and calls `.name` on something, and does not declare a field or method of that name itself. This is a text scan, not the compiler. V-P301's throwaway crate gives the exact list.\n")

w("## Inherent impls that cannot leave vo-scene\n")
w("Rust allows `impl Element<M> { .. }` only in the crate that defines `Element`. These files add builder methods that way:\n")
for m, n in sorted(D["elem_impls"].items()):
    w(f"- `{m}.rs`: {n} block(s), target {fam[m]}")
w("- `glass.rs`: one `impl DisplayList` block, which goes to vo-display with the glass display types.")
w("\nOnly `ribbon` leaves vo-scene among these, so it needs an extension trait (or a free constructor) in V-08. `image`'s block stays in core with the split.\n")

w("## Tests and runner files that use an effect\n")
w("Family found from paths, public effect type names and Element builder methods that belong to one family. vo-scene must end with no dev-dependency on any `vo-fx-*` crate, so every vo-scene test listed here moves.\n")
w("| File | Families | Evidence | Target home |")
w("|---|---|---|---|")
for f, fams in sorted(T["files"].items()):
    if not fams:
        continue
    keys = sorted(fams)
    if f.startswith("vo-scene/"):
        home = f"vo-fx-{keys[0]}/tests/" if len(keys) == 1 else "vo runner crate"
    else:
        home = "vo runner crate (stays)"
    ev = "; ".join(f"{k}: " + ", ".join(fams[k][:4]) for k in keys)
    w(f"| `{f}` | {', '.join(keys)} | {ev} | {home} |")
w("")

w("## Appendix: other vo crates per file\n")
for c, s in appendix.items():
    w(f"**{c}** ({len(s)} files, {sum(v[0] for v in s.values())} code lines): " + ", ".join(f"`{k}` {v[0]}" for k, v in sorted(s.items())))
    w("")
print("\n".join(out))
