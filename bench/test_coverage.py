"""Compare the functions cargo-atlas marks as tests with a text search for `#[test]`.

Usage: python3 test_coverage.py WORKSPACE

Reads WORKSPACE/.atlas/graph.json. For each `#[test]` line, finds the function
it marks and checks that the graph has it as a test. A miss falls in one of:

- inside a `macro_rules!` body, where the name is a placeholder like `$name`;
- behind a cfg on the test itself, such as `#[cfg(windows)]`;
- not in the index at all, usually a module behind a feature;
- a node that exists but isn't marked: a cargo-atlas bug.
"""

import collections
import json
import pathlib
import re
import sys


def main():
    root = pathlib.Path(sys.argv[1])
    graph = json.loads((root / ".atlas" / "graph.json").read_text())
    nodes = {(n["file"], n["line"]) for n in graph["nodes"] if n.get("file")}
    tests = {(n["file"], n["line"]) for n in graph["nodes"] if n.get("test")}

    counts = collections.Counter()
    examples = {}
    for path in sorted(root.rglob("*.rs")):
        rel = path.relative_to(root).as_posix()
        if rel.startswith(("target/", ".atlas/")):
            continue
        lines = path.read_text(errors="replace").splitlines()
        for index, line in enumerate(lines):
            if not re.match(r"\s*#\[test\]", line):
                continue
            name, name_line = None, None
            for j in range(index + 1, min(index + 7, len(lines))):
                m = re.search(r"\bfn\s+(\$?\w+)", lines[j])
                if m:
                    name, name_line = m.group(1), j + 1
                    break
            if name and (rel, name_line) in tests:
                counts["marked as a test"] += 1
                continue
            if name is None or name.startswith("$"):
                why = "inside a macro_rules! body"
            elif (rel, name_line) in nodes:
                why = "NODE EXISTS BUT IS NOT MARKED"
            else:
                attrs = " ".join(lines[index : name_line - 1])
                cfg = re.search(r"#\[cfg\((.*)\)\]", attrs)
                why = f"cfg on the test: {cfg.group(1)}" if cfg else "not in the index"
            counts[why] += 1
            examples.setdefault(why, f"{rel}:{name_line or index + 1}")

    for why, count in counts.most_common():
        example = f"   e.g. {examples[why]}" if why in examples else ""
        print(f"{count:6}  {why}{example}")


if __name__ == "__main__":
    main()
