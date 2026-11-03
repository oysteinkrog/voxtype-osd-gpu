#!/usr/bin/env python3
"""V-01 survey: map vo-scene and vo-render modules to target crates and check the graph.

Usage: survey.py <vo crates dir>   (prints a JSON summary)
"""
import json, os, re, subprocess, sys
from collections import defaultdict

sys.path.insert(0, os.path.dirname(__file__))
from items import parse_tree, strip_comments  # noqa: E402

CR = sys.argv[1]
SCENE = os.path.join(CR, "vo-scene/src")
RENDER = os.path.join(CR, "vo-render/src")

DISP, TEXT, CORE, WID = "vo-display", "vo-text", "vo-scene core", "vo-scene widgets"
FXT, FXF, FXG, SVG, REND = "vo-fx-text", "vo-fx-field", "vo-fx-glass", "vo-svg", "vo-render"
FX = {FXT: "text", FXF: "field", FXG: "glass"}

DEFAULT = {
    # vo-display: the display list, paint, geometry and what the renderer reads.
    "geom": DISP, "display": DISP, "path": DISP, "material": DISP, "gpu": DISP,
    "stage": DISP, "svg": DISP, "assets": DISP, "image": DISP,
    # vo-text: fonts, shaping, line fitting, inline objects in shaped text.
    "font": TEXT, "text": TEXT, "inline": TEXT,
    # vo-scene core: keyed view, reconcile, layout, input, signals, timing.
    "element": CORE, "scene": CORE, "layout": CORE, "input": CORE, "inspect": CORE,
    "bind": CORE, "reach": CORE, "canvas": CORE, "arrival": CORE, "story": CORE,
    "timed": CORE, "repeat": CORE, "ambient": CORE, "container": CORE, "focus": CORE, "lib": CORE,
    # Widgets: neither core nor an effect family in plan 3.2. They stay in vo-scene.
    "bars": WID, "levels": WID, "trace": WID, "segments": WID, "meters": WID,
    "frame": WID, "graticule": WID, "spectrum": WID, "status_icon": WID, "reveal": WID,
    "erase": WID, "swap": WID, "roll": WID, "dots": WID, "hearing": WID, "snap": WID,
    # Text effects.
    "flow": FXT, "stroke_write": FXT, "glyph_morph": FXT, "centreline": FXT, "curve": FXT,
    "typo": FXT, "word_voice": FXT, "final_pass": FXT, "lines": FXT,
    "motion/liquid": FXT, "motion/var_axes": FXT, "motion/letter_motion": FXT,
    "motion/letter_physics": FXT, "motion/magnet": FXT, "motion/palimpsest": FXT,
    "motion/revisions": FXT, "motion/overflow": FXT, "motion/margin": FXT,
    "motion/spine": FXT, "motion/bell": FXT,
    # Field effects.
    "emitter": FXF, "emitter_signals": FXF, "lattice": FXF, "ribbon": FXF, "water": FXF,
    "fluid": FXF,
    # Glass effects.
    "glass": FXG, "displace": FXG, "depth_stack": FXG,
}

# Items that go to a different crate than the rest of their file (the file is split).
# Names are top-level items; `impl X` blocks follow X. "Element" means an inherent
# `impl<M> Element<M>` block in that file.
SPLIT = {
    "path": {TEXT: ["glyph_path", "glyph_path_styled"]},
    "image": {SVG: ["svg"], CORE: ["Element"]},
    "element": {TEXT: ["TextStyle"]},
    "glass": {DISP: [
        "GlassKind", "Glass", "LensProfile", "Refract", "GlassShape", "debug_corners",
        "wavy", "clockwise", "GlassInstance", "push_outline", "blur_rects", "FAR",
        "blur_rects_in", "MAX_BLUR_RECTS", "BlurBehind", "outline_blur_rects",
        "outline_blur_rects_in", "Edges", "rows", "cross", "intersect", "shape_points",
        "SHEEN_SECS", "ORBIT_SECS", "ORBIT_FPS", "BLUR_MIN_ALPHA", "srgb_to_linear",
        "luminance", "ratio", "DisplayList", "BlurCache", "GlassKey", "BlurKey",
        "LayerKey"]},
    "emitter": {DISP: [
        "Shape", "Shock", "EmitterSim", "SpriteStyle", "GlyphChars", "GLYPH_SETS", "ColorBy",
        "EmitterLook", "tilt_x", "EmitterSource", "SourceRef", "EmitterInstance",
        "WordParticles", "ParticleWord", "ParticleWords", "MAX_SHAPE_POINTS", "Target",
        "ShapeTarget", "MaskField", "mask_size", "box_blur", "sample_path", "polylines",
        "spans", "INTERIOR_WORK", "interior", "TimedMask", "WordSource"]},
    "motion/liquid": {DISP: ["LiquidRevision", "LiquidPart", "LiquidGlyph", "LIQUID_SECS"]},
    "reach": {DISP: ["particle_reach"]},
    "frame": {DISP: ["Corner", "Edge", "Notch", "Frame", "polygon", "area", "clip_convex",
                     "closed", "over", "Stripes", "Grid"]},
    "displace": {DISP: ["Displace", "Onset", "Ripples", "RING_ATTACK", "PageCurl"]},
}

ITEM_RE = re.compile(
    r"^(?:#\[[^\n]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:const\s+fn|fn|struct|enum|const|static|type|trait|mod|union|macro_rules!)\s+([A-Za-z_][A-Za-z0-9_]*)"
)
IMPL_RE = re.compile(r"^impl(?:<[^{]*?>)?\s+(?:[A-Za-z_:<>, ]+?\s+for\s+)?(?:[a-z_]+::)*([A-Za-z_][A-Za-z0-9_]*)")


def modname(rel):
    m = rel[:-3]
    return m[:-4] if m.endswith("/mod") else m


def load(root):
    out = {}
    for dp, _, fs in os.walk(root):
        for f in fs:
            if f.endswith(".rs"):
                p = os.path.join(dp, f)
                out[os.path.relpath(p, root)] = open(p).read()
    return out


def test_cut(src):
    k = src.find("#[cfg(test)]\nmod ")
    return len(src) if k < 0 else k


def items_of(src):
    """Top-level items as (name, start_line, end_line), end exclusive, tests excluded."""
    lines = src[: test_cut(src)].split("\n")
    starts = []
    for i, l in enumerate(lines):
        m = ITEM_RE.match(l) or IMPL_RE.match(l)
        if m:
            starts.append((m.group(1), i))
    res = []
    for j, (n, s) in enumerate(starts):
        e = starts[j + 1][1] if j + 1 < len(starts) else len(lines)
        res.append((n, s, e))
    return res, len(lines)


def target_of(mod, item):
    for t, names in SPLIT.get(mod, {}).items():
        if item in names:
            return t
    return DEFAULT[mod]


def tokei_lines(path):
    out = subprocess.run(["tokei", "-f", "-o", "json", path], capture_output=True, text=True).stdout
    d = json.loads(out)["Rust"]["reports"]
    return {os.path.relpath(r["name"], path): (r["stats"]["code"], r["stats"]["code"] + r["stats"]["comments"] + r["stats"]["blanks"]) for r in d}


def resolve(p, mods):
    """(module, item) for a path; `flow::liquid::X` and `font::var_axes::X` name a motion file."""
    if len(p) > 2 and p[0] in ("flow", "font") and f"motion/{p[1]}" in mods:
        return f"motion/{p[1]}", p[2]
    if len(p) == 2 and p[0] in ("flow", "font") and f"motion/{p[1]}" in mods:
        return f"motion/{p[1]}", p[1]
    return p[0], (p[1] if len(p) > 1 else p[0])


def refs_with_line(src, prefix):
    body = src[: test_cut(src)]
    clean = strip_comments(body)
    out = []
    for m in re.finditer(r"\b" + prefix + r"::", clean):
        line = clean.count("\n", 0, m.start())
        ps, _ = parse_tree(clean, m.end())
        for p in ps:
            out.append((p, line))
    return out


def main():
    scene = load(SCENE)
    render = load(RENDER)
    mods = {modname(r): r for r in scene}
    missing = sorted(m for m in mods if m not in DEFAULT)
    if missing:
        sys.exit(f"unmapped modules: {missing}")
    sl = tokei_lines(SCENE)
    rl = tokei_lines(RENDER)

    # Per-file split line counts (code lines are apportioned by total-line share).
    per_target = defaultdict(lambda: [0, 0])
    file_rows = []
    item_spans = {}
    for m, rel in sorted(mods.items()):
        its, nlines = items_of(scene[rel])
        item_spans[m] = its
        code, total = sl[rel]
        shares = defaultdict(int)
        head = its[0][1] if its else nlines
        shares[DEFAULT[m]] += head
        for n, s, e in its:
            shares[target_of(m, n)] += e - s
        tail = total - nlines  # test module
        shares[DEFAULT[m]] += max(tail, 0)
        tot = sum(shares.values()) or 1
        parts = []
        for t, n in sorted(shares.items(), key=lambda x: -x[1]):
            c = round(code * n / tot)
            per_target[t][0] += c
            per_target[t][1] += round(total * n / tot)
            if len(shares) > 1:
                parts.append(f"{t} ~{c}")
        file_rows.append((f"vo-scene/src/{rel}", code, total, DEFAULT[m], "; ".join(parts)))
    for rel in sorted(render):
        code, total = rl[rel]
        per_target[REND][0] += code
        per_target[REND][1] += total
        file_rows.append((f"vo-render/src/{rel}", code, total, REND, ""))

    def enclosing(m, line):
        for n, s, e in item_spans[m]:
            if s <= line < e:
                return n
        return None

    def from_targets(m, line, p, lines_m):
        """Targets of the code that uses this reference. A `use` in the file head counts
        for every item whose body names the imported item."""
        n = enclosing(m, line)
        if n is not None:
            return {target_of(m, n)}
        name = p[-1] if p[-1] not in ("self", "*") else p[-2] if len(p) > 1 else p[0]
        hits = set()
        for iname, s0, e0 in item_spans[m]:
            if re.search(r"\b%s\b" % re.escape(name), "\n".join(lines_m[s0:e0])):
                hits.add(target_of(m, iname))
        return hits or {DEFAULT[m]}

    # Target edges from item-level references.
    edges = defaultdict(set)  # (from, to) -> examples
    for m, rel in mods.items():
        lines_m = strip_comments(scene[rel][: test_cut(scene[rel])]).split("\n")
        for p, line in refs_with_line(scene[rel], "crate"):
            if p[0] not in mods or p[0] == m:
                continue
            pm, it = resolve(p, mods)
            if pm == m:
                continue
            tt = target_of(pm, it)
            for ft in from_targets(m, line, p, lines_m):
                if ft != tt:
                    edges[(ft, tt)].add(f"{m} -> {pm}::{it}")
        if m.startswith("motion/"):
            parent = "font" if m == "motion/var_axes" else "flow"
            for p, line in refs_with_line(scene[rel], "super"):
                ft = target_of(m, enclosing(m, line) or "")
                tt = target_of(parent, p[0])
                if ft != tt:
                    edges[(ft, tt)].add(f"{m} -> super({parent})::{p[0]}")
    for rel, src in render.items():
        for p, line in refs_with_line(src, "vo_scene"):
            if p[0] in mods:
                pm, it = resolve(p, mods)
                tt = target_of(pm, it)
                edges[(REND, tt)].add(f"{rel} -> {pm}::{it}")

    # Cycle check over the target graph, ignoring the planned inversions.
    def graph(skip):
        g = defaultdict(set)
        for (a, b) in edges:
            if (a, b) not in skip:
                g[a].add(b)
        return g

    def cycles(g):
        found, stack, seen = [], [], set()

        def dfs(u):
            stack.append(u)
            for v in sorted(g[u]):
                if v in stack:
                    found.append(stack[stack.index(v):] + [v])
                elif v not in seen:
                    dfs(v)
            stack.pop()
            seen.add(u)

        for u in sorted(list(g)):
            if u not in seen:
                dfs(u)
        return found

    rank = {DISP: 0, SVG: 1, TEXT: 1, CORE: 2, WID: 3, FXT: 4, FXF: 4, FXG: 4, REND: 5}
    # A downward edge (higher rank to lower) is allowed. An upward edge is one the
    # effect API (V-06) must invert: today's code calls up, the new code registers down.
    inversions = {k for k in edges if rank[k[0]] < rank[k[1]] or (rank[k[0]] == rank[k[1]] and k[0] != k[1])}

    # Crate-private items used across a target boundary by an effect module.
    priv_decl = defaultdict(list)  # module -> (name, is_member)
    for m, rel in mods.items():
        body = scene[rel][: test_cut(scene[rel])]
        for mm in re.finditer(r"^([ \t]*)pub\((?:crate|super)\)\s+(?:unsafe\s+)?(?:const\s+fn|fn|struct|enum|const|static|type|trait|mod)?\s*([A-Za-z_][A-Za-z0-9_]*)", body, re.M):
            priv_decl[m].append((mm.group(2), bool(mm.group(1))))
    mod_refs = {}
    for e, rel in mods.items():
        mod_refs[e] = {p[0] for p, _ in refs_with_line(scene[rel], "crate")}
    priv_use = defaultdict(set)
    for e, rel in mods.items():
        et = DEFAULT[e]
        if et not in FX:
            continue
        body = strip_comments(scene[rel][: test_cut(scene[rel])])
        for m, decls in priv_decl.items():
            if m == e:
                continue
            short = m.split("/")[-1]
            for n, member in set(decls):
                if target_of(m, n) == et:
                    continue
                if member:
                    # A field or method: count it only when E names module M at all and
                    # does not declare a field or method of that name itself.
                    own = re.search(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:fn\s+%s\b|%s\s*:)" % (re.escape(n), re.escape(n)), body, re.M)
                    hit = not own and (m in mod_refs[e] or short in mod_refs[e]) and re.search(r"\.%s\b" % re.escape(n), body)
                else:
                    hit = re.search(r"\b%s::(?:[A-Za-z_:]*::)?%s\b" % (re.escape(short), re.escape(n)), body) or re.search(r"use crate::%s::\{[^;]*\b%s\b" % (re.escape(m), re.escape(n)), body, re.S)
                if hit:
                    priv_use[e].add(f"{m}::{n}" + (" (member)" if member else ""))
    elem_impls = {}
    for m, rel in mods.items():
        if m == "element":
            continue
        n = len(re.findall(r"^\s*impl(?:<[^>]*>)?\s+(?:crate::element::)?Element<", scene[rel][: test_cut(scene[rel])], re.M))
        if n:
            elem_impls[m] = n

    summary = {
        "file_rows": file_rows,
        "per_target": dict(per_target),
        "edges": {f"{a} -> {b}": sorted(v) for (a, b), v in sorted(edges.items())},
        "inversions": sorted(f"{a} -> {b}" for a, b in inversions),
        "cycles_after_inversion": cycles(graph(inversions)),
        "cycles_today": cycles(graph(set())),
        "render_to_scene": sorted(f"{a} -> {b}" for a, b in edges if a in (REND, DISP) and b in (CORE, WID) or (a in (REND, DISP) and b in FX)),
        "priv_use": {k: sorted(v) for k, v in sorted(priv_use.items())},
        "elem_impls": elem_impls,
    }
    json.dump(summary, sys.stdout, indent=1)


if __name__ == "__main__":
    main()
