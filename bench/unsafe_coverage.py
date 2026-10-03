"""Compare cargo-atlas's unsafe sites with a plain text search for `unsafe`.

Usage: python3 unsafe_coverage.py WORKSPACE

Reads WORKSPACE/.atlas/graph.json (run `cargo atlas build` first). Every line
of Rust code containing the word `unsafe` outside a comment should be a site,
except function pointer types such as `unsafe fn(u8)`, which are types, not
code, and lines in files rust-analyzer didn't index. Prints the counts, then
each line that is neither found nor explained, for reading by hand.
"""

import collections
import json
import pathlib
import re
import sys


def main():
    root = pathlib.Path(sys.argv[1])
    graph = json.loads((root / ".atlas" / "graph.json").read_text())
    found = {(s["file"], s["line"]) for s in graph["unsafe_sites"]}
    indexed = {f["path"] for f in graph["files"]}

    counts = collections.Counter()
    missed = []
    for path in sorted(root.rglob("*.rs")):
        rel = path.relative_to(root).as_posix()
        if rel.startswith(("target/", ".atlas/")):
            continue
        for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            code = line.split("//")[0]
            if code.strip().startswith(("*", "/*")) or not re.search(r"\bunsafe\b", code):
                continue
            fn_pointer = re.search(r'\bunsafe\s+(extern\s+"[^"]*"\s+)?fn\s*\(', code)
            if fn_pointer and not re.search(r"\bunsafe\s+fn\s+\w", code):
                counts["function pointer type (not code)"] += 1
            elif (rel, number) in found:
                counts["found"] += 1
            elif rel not in indexed:
                counts["in a file rust-analyzer didn't index"] += 1
            else:
                counts["not found"] += 1
                missed.append(f"{rel}:{number}  {line.strip()[:90]}")

    for name, count in counts.most_common():
        print(f"{count:6}  {name}")
    sites = graph["unsafe_sites"]
    print(f"\n{len(sites)} sites, {sum(not s['documented'] for s in sites)} without their comment")
    print(f"{sum(s['item'] is None for s in sites)} sites with no indexed item around them")
    if missed:
        print("\nNot found (check whether each is in a macro_rules! body or a DSL macro):")
        print("\n".join(missed))


if __name__ == "__main__":
    main()
