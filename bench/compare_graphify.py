"""Smoke comparison: Graphify's call links vs cargo-atlas's, matched by (file, line).

The sample scripts next to this one check links against the code. This one only
measures how often the two tools agree, treating rust-analyzer's resolution as
the reference.
"""
import collections
import json
import re
import sys

repo = sys.argv[1]
atlas = json.load(open(f"{repo}/.atlas/graph.json"))
graphify = json.load(open(f"{repo}/graphify-out/graph.json"))

# --- cargo-atlas: call links as ((file, line), (file, line)) -----------------
a_loc = {n["id"]: (n["file"], n["line"]) for n in atlas["nodes"] if n.get("file")}
# Only function-to-function links are comparable: Graphify has no macro calls.
a_callable = {n["id"] for n in atlas["nodes"] if n["kind"] in ("function", "method", "trait_method")}
a_name = {n["id"]: n["name"] for n in atlas["nodes"]}
a_calls = set()      # EXACT calls
a_any = set()        # calls or may_call (a candidate impl counts as agreement)
for e in atlas["edges"]:
    if e["kind"] in ("calls", "may_call") and e["from"] in a_loc and e["to"] in a_callable:
        pair = (a_loc[e["from"]], a_loc[e["to"]])
        a_any.add(pair)
        if e["kind"] == "calls":
            a_calls.add(pair)
atlas_callables = {a_loc[n["id"]] for n in atlas["nodes"]
                   if n["kind"] in ("function", "method", "trait_method") and n.get("file")}

# --- Graphify: call links -----------------------------------------------------
def g_loc(n):
    f, loc = n.get("source_file"), n.get("source_location") or ""
    m = re.match(r"L(\d+)", loc)
    if not f or not m:
        return None
    f = f.split(repo.rstrip("/") + "/", 1)[-1]
    return (f, int(m.group(1)))

g_nodes = {n["id"]: n for n in graphify["nodes"]}
g_edges = graphify.get("links") or graphify.get("edges")
g_calls = []
for e in g_edges:
    if e.get("relation") != "calls":
        continue
    s, t = g_nodes.get(e["source"]), g_nodes.get(e["target"])
    if s and t and g_loc(s) and g_loc(t):
        g_calls.append((g_loc(s), g_loc(t), e.get("confidence")))

g_set = {(s, t) for s, t, _ in g_calls}
agree = g_set & a_any
# Graphify links whose target is a function rust-analyzer knows, from a caller it knows,
# but which rust-analyzer resolved differently: likely wrong targets.
comparable = {(s, t) for s, t in g_set if s in atlas_callables and t in atlas_callables}
disagree = comparable - a_any

print(f"cargo-atlas: {len(a_calls)} call links (EXACT), {len(a_any - a_calls)} extra candidate links")
print(f"Graphify:    {len(g_set)} call links "
      f"({collections.Counter(c for _, _, c in g_calls)})")
print(f"Graphify links that rust-analyzer confirms:      {len(agree)} / {len(g_set)}"
      f" = {len(agree)/max(1,len(g_set)):.1%}")
print(f"Comparable Graphify links rust-analyzer resolves elsewhere: {len(disagree)} / {len(comparable)}"
      f" = {len(disagree)/max(1,len(comparable)):.1%}")
print(f"rust-analyzer call links Graphify also has:        {len(a_calls & g_set)} / {len(a_calls)}"
      f" = {len(a_calls & g_set)/max(1,len(a_calls)):.1%}")

# A few disagreements, with names, to check against the code.
by_loc = {v: k for k, v in a_loc.items()}
print("\nSample of Graphify links that rust-analyzer resolves differently:")
for s, t in sorted(disagree)[:8]:
    src, dst = by_loc.get(s), by_loc.get(t)
    print(f"  {a_name.get(src, s)} -> {a_name.get(dst, t)}   [{s[0]}:{s[1]}]")
