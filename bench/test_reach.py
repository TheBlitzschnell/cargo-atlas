"""Which tests really run a function: the ground truth for `tests`, measured.

    python3 bench/test_reach.py COPY targets.json reach.json

COPY must be a throwaway copy of the workspace: this rewrites its source. Each
target in targets.json ({"id", "file", "line", "name"}, from lsp_compare.py)
gets a line at the top of its body that appends its id, and the name of the
program it runs in, to the file named by $REACH_LOG. Then every test that
`cargo test --workspace` runs is run alone, one at a time, so each log
belongs to one test.
A test that runs the built program as a subprocess reaches the target in that
program, and the log says so. `const fn`s and functions without a body are
skipped. Doc tests are left out.
"""
import json
import os
import re
import subprocess
import sys
import tempfile

copy, targets_path, out_path = sys.argv[1:4]
targets = json.load(open(targets_path))

MARK = ('{{ static SEEN: ::std::sync::atomic::AtomicBool = ::std::sync::atomic::AtomicBool::new(false); '
        'if !SEEN.swap(true, ::std::sync::atomic::Ordering::Relaxed) {{ '
        'if let Some(log) = ::std::env::var_os("REACH_LOG") {{ '
        'let exe = ::std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default(); '
        'if let Ok(mut f) = ::std::fs::OpenOptions::new().create(true).append(true).open(log) {{ '
        'let _ = ::std::io::Write::write_all(&mut f, format!("{id} {{}}\\n", exe).as_bytes()); }} }} }} }}')


def body_start(text, start):
    """Offset just after the `{` that opens the body of the fn whose name is at `start`."""
    depth = 0
    for i in range(start, len(text)):
        c = text[i]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth == 0 and c == ";":
            return None                       # a declaration without a body
        elif depth == 0 and c == "{":
            return i + 1
    return None


by_file = {}
for t in targets:
    by_file.setdefault(t["file"], []).append(t)
instrumented, skipped = [], []
for file, ts in by_file.items():
    path = os.path.join(copy, file)
    text = open(path).read()
    line_starts = [0] + [m.end() for m in re.finditer("\n", text)]
    inserts = []
    for t in ts:
        line = text[line_starts[t["line"] - 1]:line_starts[t["line"]]]
        m = re.search(r"\bfn\s+" + re.escape(t["name"]) + r"\b", line)
        before = text[max(0, line_starts[t["line"] - 1] - 200):line_starts[t["line"] - 1] + (m.start() if m else 0)]
        if not m or re.search(r"\bconst\s+(unsafe\s+)?(extern\s+\"\w+\"\s+)?$", before):
            skipped.append((t["id"], "no fn on its line, or a const fn"))
            continue
        at = body_start(text, line_starts[t["line"] - 1] + m.end())
        if at is None:
            skipped.append((t["id"], "no body"))
            continue
        inserts.append((at, MARK.format(id=t["id"])))
        instrumented.append(t["id"])
    for at, code in sorted(inserts, reverse=True):
        text = text[:at] + code + text[at:]
    open(path, "w").write(text)
print(f"instrumented {len(instrumented)} targets; skipped {skipped}", flush=True)

# Build every test binary, then list its tests.
build = subprocess.run(["cargo", "test", "--workspace", "--no-run", "--message-format=json"],
                       cwd=copy, capture_output=True, text=True)
if build.returncode != 0:
    sys.exit(build.stderr[-3000:])
binaries = []
for line in build.stdout.splitlines():
    msg = json.loads(line)
    if msg.get("reason") == "compiler-artifact" and msg["profile"]["test"] and msg.get("executable"):
        binaries.append((msg["executable"], os.path.dirname(msg["manifest_path"]), msg["target"]["name"],
                         msg["target"]["kind"][0]))
jobs = []
for exe, pkg_dir, target, kind in binaries:
    listed = subprocess.run([exe, "--list"], cwd=pkg_dir, capture_output=True, text=True).stdout
    for name in (l[:-len(": test")] for l in listed.splitlines() if l.endswith(": test")):
        jobs.append((exe, pkg_dir, target, kind, name))
print(f"{len(binaries)} test binaries, {len(jobs)} tests", flush=True)

logs = tempfile.mkdtemp(prefix="reach-")


def run(i_job):
    i, (exe, pkg_dir, target, kind, name) = i_job
    log = os.path.join(logs, f"{i}.log")
    env = dict(os.environ, REACH_LOG=log, CARGO_MANIFEST_DIR=pkg_dir, RUST_BACKTRACE="0")
    try:
        r = subprocess.run([exe, "--exact", name, "--test-threads=1", "-q"], cwd=pkg_dir, env=env,
                           capture_output=True, text=True, timeout=300)
        status = "ok" if r.returncode == 0 else "failed"
    except subprocess.TimeoutExpired:
        status = "timeout"
    reached = {}
    if os.path.exists(log):
        for l in open(log):
            tid, _, program = l.strip().partition(" ")
            reached.setdefault(tid, set()).add(program)
    return {"package": os.path.relpath(pkg_dir, copy), "binary": target, "kind": kind, "test": name, "status": status,
            "reached": {tid: sorted(p) for tid, p in reached.items()}}


# One at a time: two tests in two processes can pick the same temporary directory.
results = [run(job) for job in enumerate(jobs)]
json.dump({"instrumented": instrumented, "skipped": skipped, "tests": results}, open(out_path, "w"), indent=1)
failed = [r for r in results if r["status"] != "ok"]
print(f"ran {len(results)} tests, {len(failed)} not ok; "
      f"{sum(bool(r['reached']) for r in results)} reached at least one target -> {out_path}")
