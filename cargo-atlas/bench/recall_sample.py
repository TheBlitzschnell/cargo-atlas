"""Recall sample: pick call sites straight from the source text, then ask the graph.

Call sites are found with a regex, independent of both tools. A site counts only
if its target is a function defined in the workspace; calls into std or other
crates are set aside. Sites the graph misses are printed so they can be checked
against the code.
"""
import json
import os
import random
import re
import sys

repo, n, seed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
g = json.load(open(f"{repo}/.atlas/graph.json"))
N = {x["id"]: x for x in g["nodes"]}
callable_kinds = {"function", "method", "trait_method"}
last = lambda name: re.split(r"::", name)[-1]
workspace_fn_names = {last(x["name"]) for x in g["nodes"] if x["kind"] in callable_kinds}

links_at = {}
for e in g["edges"]:
    if e["kind"] in ("calls", "references", "may_call") and e.get("file"):
        for ln in e["lines"]:
            links_at.setdefault((e["file"], ln), set()).add(last(N[e["to"]]["name"]))

KEYWORDS = {"if", "while", "for", "match", "return", "fn", "loop", "in", "let", "move",
            "as", "unsafe", "impl", "where", "use", "mod", "pub", "crate", "super", "self",
            "struct", "enum", "trait", "type", "const", "static", "dyn", "ref", "mut", "else"}
call_re = re.compile(r"(?<![A-Za-z0-9_!])([a-z_][a-z0-9_]*)\s*(?:::<[^()]*>)?\(")

sites = []
for root, dirs, files in os.walk(repo):
    dirs[:] = [d for d in dirs if d not in ("target", ".atlas", ".git", "graphify-out")]
    for f in files:
        if not f.endswith(".rs"):
            continue
        path = os.path.join(root, f)
        rel = os.path.relpath(path, repo)
        for i, text in enumerate(open(path, encoding="utf-8", errors="replace"), 1):
            code = text.split("//")[0]
            for m in call_re.finditer(code):
                ident = m.group(1)
                before = code[: m.start()].rstrip()
                if ident in KEYWORDS or before.endswith("fn"):
                    continue
                sites.append((rel, i, ident, text.strip()))

random.seed(seed)
sample = random.sample(sites, n)
found, external, missed = 0, 0, []
for rel, i, ident, text in sample:
    if ident in links_at.get((rel, i), set()):
        found += 1
    elif ident not in workspace_fn_names:
        external += 1
    else:
        missed.append((rel, i, ident, text))

print(f"{len(sites)} call-like sites in the source; sampled {n}")
print(f"  target outside the workspace (std, dependencies): {external}")
print(f"  found in the graph: {found}")
print(f"  possible misses, to check against the code: {len(missed)}")
for rel, i, ident, text in missed:
    print(f"    {rel}:{i}  `{ident}`  | {text[:100]}")
