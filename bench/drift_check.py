"""Check a fresh map against links that were checked by hand, and against its counts.

    python3 -I bench/drift_check.py ripgrep/.atlas/graph.json bench/expected-ripgrep-15.1.0.toml

The expected file names links that must be in the map, each read against the
code, links that must not be, and the counts the map had when they were
checked. CI runs this every night (.github/workflows/drift.yml), so a new
rust-analyzer can't change what cargo-atlas reports without anyone noticing.
Any difference exits with 1.
"""
import json
import os
import sys
import tomllib

CALLABLE = {"function", "method", "trait_method"}


def counts(g):
    kind = {n["id"]: n["kind"] for n in g["nodes"]}
    edges = g["edges"]
    return {
        "items": len(g["nodes"]),
        "links": len(edges),
        "exact_calls": len({(e["from"], e["to"]) for e in edges
                            if e["kind"] == "calls" and e["confidence"] == "EXACT"
                            and kind[e["from"]] in CALLABLE and kind[e["to"]] in CALLABLE}),
        "candidates": sum(e["kind"] == "may_call" for e in edges),
        "syntax_calls": sum(e["kind"] == "calls" and e["confidence"] == "SYNTAX" for e in edges),
        "outside_trait_impls": sum(e["kind"] == "implements" and e["confidence"] == "EXACT"
                                   and kind[e["to"]] == "external_trait" for e in edges),
        "name_only_traits": sum(n["id"].startswith("trait:") for n in g["nodes"]),
        "tests": sum(bool(n.get("test")) for n in g["nodes"]),
        "unsafe_sites": len(g.get("unsafe_sites", [])),
        "targets": len(g.get("targets", [])),
    }


graph_path, expected_path = sys.argv[1], sys.argv[2]
g = json.load(open(graph_path))
with open(expected_path, "rb") as f:
    want = tomllib.load(f)
nodes = {n["id"]: n for n in g["nodes"]}
by_place = {}
for n in g["nodes"]:
    if n.get("file"):
        by_place.setdefault((n["file"], n["line"]), []).append(n)


def find(spec):
    """`NAME @ FILE:LINE` for an item in the workspace, `NAME @ ID` for one outside it."""
    name, _, where = spec.rpartition(" @ ")
    file, _, line = where.rpartition(":")
    found = by_place.get((file, int(line)), []) if line.isdigit() else [nodes.get(where)]
    return [n["id"] for n in found if n and n["name"] == name]


def between(spec):
    """Items the spec names that the graph lacks, and every link between the two."""
    sources, targets = find(spec["from"]), find(spec["to"])
    gone = [s for s, ids in ((spec["from"], sources), (spec["to"], targets)) if not ids]
    return gone, [e for e in g["edges"] if e["from"] in sources and e["to"] in targets]


def show(links):
    return "; ".join(f"{e['kind']} {e['confidence']} at {e.get('file')}:{e['lines']}"
                     for e in links) or "no link"


problems = []
print(f"{want['workspace']}: links checked by hand on {want['checked']}, "
      f"with rust-analyzer {want['rust_analyzer']}")
print(f"this graph: format {g['format_version']}, {g['produced_by']}\n")
if g["format_version"] != want["format_version"]:
    problems.append(f"graph format {g['format_version']}, but the expected file was written "
                    f"for format {want['format_version']}")
if not g["stats"].get("std_sources_found"):
    problems.append("rust-analyzer didn't find the standard library's source (rust-src), "
                    "so calls on std types are unresolved: fix the setup, not the expected file")

have = counts(g)
print(f"  {'count':22}{'expected':>10}{'now':>10}")
for key, expected in want["counts"].items():
    now = have[key]
    print(f"  {key:22}{expected:>10}{now:>10}{'' if now == expected else '   <- differs'}")
    if now != expected:
        problems.append(f"{key}: {expected} expected, {now} now")

for spec in want["link"]:
    gone, found = between(spec)
    file, _, line = spec["at"].rpartition(":")
    right = [e for e in found if e["kind"] == spec["kind"] and e["confidence"] == spec["confidence"]
             and e.get("file") == file and int(line) in e["lines"]]
    if gone:
        problems.append(f"no item {' and no item '.join(gone)}")
    elif not right:
        problems.append(f"{spec['kind']} {spec['confidence']} at {spec['at']}: "
                        f"{spec['from']} -> {spec['to']}; now: {show(found)}")
for spec in want.get("absent", []):
    gone, found = between(spec)
    if gone:
        problems.append(f"no item {' and no item '.join(gone)}")
    elif found:
        problems.append(f"wrong link is back: {spec['from']} -> {spec['to']}: {show(found)}")
print(f"\n{len(want['link'])} links that must be there, {len(want.get('absent', []))} that must not")

if os.environ.get("GITHUB_STEP_SUMMARY"):
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as out:
        out.write(f"### {want['workspace']}: {g['produced_by']}\n\n")
        out.write("| count | expected | now |\n| --- | ---: | ---: |\n")
        for key, expected in want["counts"].items():
            out.write(f"| {key} | {expected} | {have[key]} |\n")
        out.write("\n" + ("".join(f"- {p}\n" for p in problems) or "No differences.\n"))

if not problems:
    print("no differences")
    sys.exit(0)
print(f"\n{len(problems)} differences:")
for p in problems:
    print(f"  - {p}")
print("""
To list every link that changed, compare with a graph from the rust-analyzer
the links were checked with (the nightly job keeps both as artifacts):
    python3 -I bench/diff_graphs.py before/graph.json after/graph.json
Read each change against the code; then update the expected file. Counts now:
""")
print("[counts]")
for key in want["counts"]:
    print(f"{key} = {have[key]}")
sys.exit(1)
