"""List the links that differ between two cargo-atlas maps of the same workspace.

    python3 -I bench/diff_graphs.py before/graph.json after/graph.json [limit]

Items are matched by name and place, not by id, so a change in how
rust-analyzer spells ids doesn't make every link look new. Each link is printed
with the line it is on, to be read against the code. At most `limit` lines
(default 100) per list.
"""
import collections
import json
import sys


def load(path):
    g = json.load(open(path))
    nodes = {n["id"]: n for n in g["nodes"]}

    def item(i):
        n = nodes[i]
        return f"{n['name']} ({n['file']}:{n['line']})" if n.get("file") else f"{n['name']} ({i})"

    links = {(item(e["from"]), item(e["to"]), e["kind"], e["confidence"], e.get("file"), tuple(e["lines"]))
             for e in g["edges"]}
    return {item(i) for i in nodes}, links


(items_a, links_a), (items_b, links_b) = load(sys.argv[1]), load(sys.argv[2])
limit = int(sys.argv[3]) if len(sys.argv) > 3 else 100
gone, new = links_a - links_b, links_b - links_a
print(f"items {len(items_a)} -> {len(items_b)}, links {len(links_a)} -> {len(links_b)}: "
      f"{len(gone)} links only before, {len(new)} only after")

by_pair = collections.defaultdict(lambda: ([], []))
for i, side in ((0, gone), (1, new)):
    for link in side:
        by_pair[link[:2]][i].append(link)


def show(link):
    return f"{link[2]} {link[3]} at {link[4]}:{','.join(map(str, link[5]))}"


def summary(links):
    c = collections.Counter((link[2], link[3]) for link in links)
    return ", ".join(f"{n} {kind} {conf}" for (kind, conf), n in c.most_common())


print(f"  only before: {summary(gone) or 'none'}\n  only after: {summary(new) or 'none'}")
sections = {"changed": [], "only before": [], "only after": []}
for (a, b), (before, after) in sorted(by_pair.items()):
    head = f"  {a}\n    -> {b}"
    if before and after:
        sections["changed"].append(f"{head}\n      {'; '.join(map(show, before))}\n"
                                   f"      now {'; '.join(map(show, after))}")
    else:
        sections["only before" if before else "only after"].append(
            f"{head}\n      {'; '.join(map(show, before or after))}")
for name, lines in sections.items():
    if lines:
        print(f"\n{name}: {len(lines)} pairs of items")
        print("\n".join(lines[:limit]) + (f"\n  ... and {len(lines) - limit} more" if len(lines) > limit else ""))

for name, items in (("items only before", items_a - items_b), ("items only after", items_b - items_a)):
    if items:
        print(f"\n{name}: {len(items)}")
        print("\n".join(f"  {i}" for i in sorted(items)[:limit]))
