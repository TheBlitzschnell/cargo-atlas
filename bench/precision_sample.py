"""Precision sample: print random call links from the graph, each with the
source line it was seen on, for checking by reading the code.

    python3 precision_sample.py <repo> <how many> <seed>
"""
import json
import random
import sys

repo, n, seed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
g = json.load(open(f"{repo}/.atlas/graph.json"))
N = {x["id"]: x for x in g["nodes"]}
sites = [(e, ln) for e in g["edges"] if e["kind"] == "calls" for ln in e["lines"]]
sources = {}


def source_line(f, ln):
    if f not in sources:
        sources[f] = open(f"{repo}/{f}", encoding="utf-8").read().splitlines()
    return sources[f][ln - 1].strip()


random.seed(seed)
print(f"{len(sites)} call sites in the graph; showing {n}")
for i, (e, ln) in enumerate(random.sample(sites, n), 1):
    target = N[e["to"]]
    print(f"{i:2}. {N[e['from']]['name']} -> {target['path']}  [{e['confidence']}]")
    print(f"      {e['file']}:{ln}: {source_line(e['file'], ln)[:110]}")
