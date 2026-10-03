"""Print a random sample of unsafe sites with the lines above each, to check
the comment verdicts by reading.

Usage: python3 unsafe_sample.py WORKSPACE COUNT SEED missing|documented

`missing` samples sites without their `// SAFETY:` comment or `# Safety`
section; `documented` samples sites with one. Sites with no indexed item
(code for another platform) are left out.
"""

import json
import pathlib
import random
import sys


def main():
    root = pathlib.Path(sys.argv[1])
    count, seed, which = int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
    graph = json.loads((root / ".atlas" / "graph.json").read_text())
    want_documented = which == "documented"
    pool = [
        s
        for s in graph["unsafe_sites"]
        if s["item"] is not None and s["documented"] == want_documented
    ]
    random.seed(seed)
    for site in random.sample(pool, min(count, len(pool))):
        lines = (root / site["file"]).read_text(errors="replace").splitlines()
        print(f"### {site['file']}:{site['line']}  {site['kind']}  documented={site['documented']}")
        for number in range(max(1, site["line"] - 6), site["line"] + 1):
            print(f"{number:6}  {lines[number - 1][:110]}")
        print()


if __name__ == "__main__":
    main()
