"""cargo-atlas against the rust-analyzer language server, the one Claude Code's
rust-analyzer-lsp plugin starts, on two questions an AI assistant asks before
changing a function: who calls it, and which tests run it.

    python3 bench/lsp_compare.py targets WORKSPACE > targets.json
    python3 bench/test_reach.py COPY_OF_WORKSPACE targets.json reach.json
    python3 bench/lsp_compare.py run WORKSPACE reach.json > report.txt

Needs `.atlas/graph.json` (cargo atlas build), cargo-atlas ($CARGO_ATLAS or
on the PATH) and rust-analyzer ($CARGO_ATLAS_RUST_ANALYZER or on the PATH),
started with no settings, as the plugin starts it. Both sides use the same
rust-analyzer, so what differs is the interface, not the analysis.

The language server is asked through operations Claude Code's LSP tool has:
call hierarchy and references. For the tests that run a function, the script
walks incoming calls up to the tests as an ideal agent would: it never asks
twice, and it tells a test by the attribute above it, read for free. Its
request counts are a floor for what an agent pays. The "trait hop" variant
also asks for the callers of the trait method when it meets the method of an
impl, as an agent who knows Rust would: 3 more requests each time, one to find
the trait method, one to point the language server at it, one for its callers.
The probes below are lines of ripgrep 15.1.0. Raw results go to
.atlas/lsp_compare.json.
"""
import collections
import functools
import json
import os
import random
import re
import statistics
import subprocess
import sys
import threading
import time
from urllib.parse import quote, unquote

CALLABLE = {"function", "method", "trait_method"}
WALK = {"calls", "may_call", "references"}
SAMPLE, SEED = 40, 7
# Chosen by hand for a known difference each; reported apart from the random sample.
PROBES = [
    ("crates/globset/src/glob.rs", 579, "a builder method; the README's `tests` example"),
    ("crates/searcher/src/line_buffer.rs", 406, "a method that shares its name with its caller"),
    ("crates/printer/src/stats.rs", 139, "run by an operator, `*stats += ...`"),
    ("crates/core/messages.rs", 137, "called only in a macro_rules! body"),
    ("crates/printer/src/hyperlink/mod.rs", 946, "called in the initializers of statics"),
    ("crates/printer/src/hyperlink/aliases.rs", 71, "a const fn called in a const's initializer"),
    ("crates/ignore/src/gitignore.rs", 191, "called directly by many tests"),
    ("crates/globset/src/glob.rs", 126, "reached through std: `\"..\".parse::<Glob>()`"),
    ("crates/regex/src/matcher.rs", 414, "an impl reached through a generic `M: Matcher`"),
    ("crates/printer/src/standard.rs", 766, "an impl reached through a generic `S: Sink`"),
    ("crates/searcher/src/searcher/mod.rs", 769, "a method many tests use through helpers"),
    ("crates/matcher/src/lib.rs", 571, "a trait method with impls in four crates"),
]


def load(ws):
    g = json.load(open(os.path.join(ws, ".atlas/graph.json")))
    g["by_id"] = {n["id"]: n for n in g["nodes"]}
    g["in"], g["out"] = collections.defaultdict(list), collections.defaultdict(list)
    for e in g["edges"]:
        g["in"][e["to"]].append(e)
        g["out"][e["from"]].append(e)
    g["at"] = {(n.get("file"), n.get("line")): n for n in g["nodes"] if n["kind"] in CALLABLE}
    return g


@functools.lru_cache(maxsize=None)
def lines_of(ws, file):
    with open(os.path.join(ws, file), encoding="utf-8", errors="replace") as f:
        return f.read().split("\n")


def fn_column(ws, name, file, line):
    """The function's simple name and the column it starts at, or None without `fn NAME` there."""
    name = re.split(r"::", name)[-1]
    lines = lines_of(ws, file)
    m = re.search(r"\bfn\s+(" + re.escape(name) + r")\b", lines[line - 1] if 0 < line <= len(lines) else "")
    return (name, m.start(1)) if m else None


def is_test_source(ws, file, line):
    """What an agent reading the file sees: a test attribute just above the fn."""
    return any(re.match(r"\s*#\[(\w+::)*test\b", l) for l in lines_of(ws, file)[max(0, line - 6):line - 1])


def pick(ws, g):
    """The random sample, then the probes: functions of library or program code, not tests."""
    def eligible(n):
        f = n.get("file") or ""
        return (n["kind"] in CALLABLE and not n.get("test") and f
                and not re.search(r"(^|/)(tests|benches|examples|fuzz)/|(^|/)build\.rs$|testutil", f)
                and not re.search(r"(^|::)tests?(::|$)", n["path"]) and fn_column(ws, n["name"], f, n["line"]))
    pool = sorted((n for n in g["nodes"] if eligible(n)), key=lambda n: n["id"])
    chosen = [(n, "random") for n in random.Random(SEED).sample(pool, SAMPLE)]
    chosen += [(g["at"][(f, ln)], why) for f, ln, why in PROBES]
    return [{"id": f"T{i:02}", "node": n["id"], "name": fn_column(ws, n["name"], n["file"], n["line"])[0],
             "file": n["file"], "line": n["line"], "label": n["name"], "why": why}
            for i, (n, why) in enumerate(chosen)]


class Lsp:
    """A small LSP client over stdio: enough for rust-analyzer's navigation requests."""

    def __init__(self, server, root):
        self.root, self.t0 = os.path.abspath(root), time.time()
        self.p = subprocess.Popen([server], cwd=self.root, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        self.next_id, self.waiting, self.lock = 1, {}, threading.Lock()
        self.ready, self.ready_at = threading.Event(), None
        threading.Thread(target=self._read, daemon=True).start()
        caps = {"window": {"workDoneProgress": True},
                "textDocument": {"callHierarchy": {}, "references": {}, "implementation": {}},
                # Only to know when indexing is over; answers don't depend on it.
                "experimental": {"serverStatusNotification": True}}
        self.request("initialize", {"processId": os.getpid(), "rootUri": self.uri(""), "capabilities": caps})
        self._send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

    def uri(self, rel):
        return "file://" + quote(os.path.join(self.root, rel))

    def rel(self, uri):
        return os.path.relpath(unquote(uri[len("file://"):]), self.root)

    def _send(self, msg):
        body = json.dumps(msg).encode()
        self.p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.p.stdin.flush()

    def _read(self):
        while True:
            length = None
            while True:
                line = self.p.stdout.readline()
                if not line:
                    return
                if not line.strip():
                    break
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":")[1])
            msg = json.loads(self.p.stdout.read(length))
            if "id" in msg and "method" in msg:
                self._send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
            elif "id" in msg:
                with self.lock:
                    slot = self.waiting.pop(msg["id"], None)
                if slot:
                    slot[1].append(msg)
                    slot[0].set()
            elif msg["method"] == "experimental/serverStatus" and msg["params"].get("quiescent"):
                if not self.ready.is_set():
                    self.ready_at = time.time() - self.t0
                self.ready.set()

    def request(self, method, params, timeout=600):
        ev, box = threading.Event(), []
        with self.lock:
            i, self.next_id = self.next_id, self.next_id + 1
            self.waiting[i] = (ev, box)
        start = time.time()
        self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        if not ev.wait(timeout):
            raise TimeoutError(method)
        if "error" in box[0]:
            raise RuntimeError(f"{method}: {box[0]['error']}")
        return box[0]["result"], time.time() - start

    def at(self, file, line, col):
        return {"textDocument": {"uri": self.uri(file)}, "position": {"line": line - 1, "character": col}}

    def peak_rss_mb(self):
        with open(f"/proc/{self.p.pid}/status") as f:
            return next(int(l.split()[1]) // 1024 for l in f if l.startswith("VmHWM"))

    def close(self):
        try:
            self.request("shutdown", None, timeout=60)
            self._send({"jsonrpc": "2.0", "method": "exit", "params": None})
            self.p.wait(timeout=60)
        except Exception:
            self.p.kill()


class Agent:
    """Asks the language server what an agent would ask, and counts what it pays."""

    def __init__(self, lsp, ws, g):
        self.lsp, self.ws, self.g = lsp, ws, g
        self.requests, self.seconds, self.chars = 0, 0.0, 0

    def _ask(self, method, params, render):
        result, dt = self.lsp.request(method, params)
        self.requests += 1
        self.seconds += dt
        self.chars += len(render(result or []))
        return result or []

    def mark(self):
        return self.requests, self.seconds, self.chars

    def since(self, mark):
        return {"requests": self.requests - mark[0], "seconds": round(self.seconds - mark[1], 3),
                "chars": self.chars - mark[2]}

    def where(self, item):
        return self.lsp.rel(item["uri"]), item["selectionRange"]["start"]["line"] + 1

    def item(self, name, file, line):
        col = fn_column(self.ws, name, file, line)[1]
        items = self._ask("textDocument/prepareCallHierarchy", self.lsp.at(file, line, col),
                          lambda r: "\n".join(f"{i['name']}  {'%s:%d' % self.where(i)}" for i in r))
        return items[0] if items else None

    def incoming(self, item):
        return self._ask("callHierarchy/incomingCalls", {"item": item}, lambda calls: "\n".join(
            f"{c['from']['name']}  {'%s:%d' % self.where(c['from'])}  calls at "
            + ", ".join(str(r["start"]["line"] + 1) for r in c["fromRanges"]) for c in calls))

    def references(self, name, file, line):
        col = fn_column(self.ws, name, file, line)[1]
        params = dict(self.lsp.at(file, line, col), context={"includeDeclaration": False})
        return self._ask("textDocument/references", params, lambda refs: "\n".join(
            f"{self.lsp.rel(x['uri'])}:{x['range']['start']['line'] + 1}" for x in refs))

    def trait_method(self, file, line):
        """For the method of an impl of a workspace trait: the trait's method. An agent finds it
        by reading the impl's header and the trait; that costs it one request here."""
        node = self.g["at"].get((file, line))
        for e in self.g["out"][node["id"]] if node else []:
            t = self.g["by_id"][e["to"]]
            if e["kind"] == "implements" and t["kind"] in CALLABLE:
                self.requests += 1
                return self.item(t["name"], t["file"], t["line"])
        return None

    def sites(self, item, hop):
        """Call sites of an item: incoming calls, and with `hop` those of its trait method too."""
        calls = self.incoming(item)
        if hop:
            trait = self.trait_method(*self.where(item))
            calls += self.incoming(trait) if trait else []
        return {(self.lsp.rel(c["from"]["uri"]), r["start"]["line"] + 1): c["from"]
                for c in calls for r in c["fromRanges"]}

    def tests(self, item, hop, cap=5000):
        """Walk incoming calls to the top, never asking twice. Returns the tests found, and the
        rounds an agent needs if it sends each level's requests at once."""
        seen, queue, tests, rounds = {self.where(item)}, [(item, 1)], set(), 0
        start = self.requests
        while queue and self.requests - start < cap:
            item, depth = queue.pop(0)
            rounds = max(rounds, depth)
            for caller in self.sites(item, hop).values():
                k = self.where(caller)
                if k in seen:
                    continue
                seen.add(k)
                if is_test_source(self.ws, *k):
                    tests.add(k)
                else:
                    queue.append((caller, depth + 1))
        return tests, bool(queue), rounds


def atlas_cli(ws, *args):
    start = time.time()
    out = subprocess.run([os.environ.get("CARGO_ATLAS", "cargo-atlas"), *args, "--dir", ws],
                         capture_output=True, text=True).stdout
    return out, time.time() - start


def atlas_sites(g, node_id):
    """The call sites `callers` lists: (file, line) -> confidence."""
    return {(e["file"], ln): e["confidence"] for e in g["in"][node_id] if e["kind"] in WALK for ln in e["lines"]}


def atlas_tests(g, node_id):
    """The tests `cargo atlas tests` lists for a function, as src/query.rs finds them."""
    def walk(starts):
        seen, queue = set(starts), list(starts)
        while queue:
            for e in g["in"][queue.pop()]:
                if e["kind"] in WALK and e["from"] not in seen:
                    seen.add(e["from"])
                    queue.append(e["from"])
        return {(g["by_id"][i]["file"], g["by_id"][i]["line"]) for i in seen
                if i != node_id and g["by_id"][i].get("test")}

    tests = walk([node_id])
    node = g["by_id"][node_id]
    if tests or node["kind"] != "method" or any(e["kind"] == "implements" for e in g["out"][node_id]):
        return tests, False
    # A method of an impl of a trait from outside, which Rust may call without naming it:
    # the tests that use its type instead.
    m = re.search(r" as ([^<>]+)[<>]", node["name"])
    owner = next((g["by_id"][e["from"]] for e in g["in"][node_id] if e["kind"] == "contains"), None)
    if not m or not owner or not any(e["kind"] == "implements" and g["by_id"][e["to"]]["kind"] == "external_trait"
                                     and g["by_id"][e["to"]]["name"] == m.group(1) for e in g["out"][owner["id"]]):
        return tests, False
    starts = {e["to"] for e in g["out"][owner["id"]] if e["kind"] == "contains"
              and g["by_id"][e["to"]]["kind"] in CALLABLE}
    starts |= {e["from"] for e in g["in"][owner["id"]] if e["kind"] == "uses_type"
               and g["by_id"][e["from"]]["kind"] in CALLABLE | {"macro", "const", "static"}}
    return walk(sorted(starts)), True


def cli_tests(out):
    """Tests in `cargo atlas tests` output, and how many more it says it left out."""
    shown = {(m.group(1), int(m.group(2))) for m in re.finditer(r"^  \S+\s+(\S+\.rs):(\d+)\s", out, re.M)}
    more = re.search(r"^  \.\.\. and (\d+) more", out, re.M)
    return shown, int(more.group(1)) if more else 0


def truth(g, reach, tid):
    """Tests that ran the target in their own process, and tests that ran it only in a
    program they started, keyed by where the test is."""
    by_path = collections.defaultdict(list)
    for n in g["nodes"]:
        if n["kind"] in CALLABLE and n.get("file"):
            by_path[n["path"]].append(n)
    inproc, subproc = set(), set()
    for r in reach["tests"]:
        if tid not in r["reached"]:
            continue
        cands = by_path.get(r["test"]) or [n for p, ns in by_path.items() if p.endswith("::" + r["test"]) for n in ns]
        pkg = r["package"]
        cands = [n for n in cands if (n["file"].startswith(("tests/", "crates/core/")) if pkg == "."
                                      else n["file"].startswith(pkg + "/"))] or cands
        cands = ([n for n in cands if n.get("test")] or cands) if len(cands) > 1 else cands
        key = (cands[0]["file"], cands[0]["line"]) if cands else ("?", f"{r['binary']}::{r['test']}")
        # A test binary is named after its target plus a hash; any other name is a program it ran.
        own = r["binary"].replace("-", "_") + "-"
        (inproc if any(p.startswith(own) for p in r["reached"][tid]) else subproc).add(key)
    return inproc, subproc


def mcp_server(ws, queries):
    """`cargo atlas serve` as Claude Code runs it: seconds per tools/call, and its memory."""
    p = subprocess.Popen([os.environ.get("CARGO_ATLAS", "cargo-atlas"), "serve", "--dir", ws],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)

    def call(i, method, params):
        p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params}) + "\n")
        p.stdin.flush()
        while True:
            msg = json.loads(p.stdout.readline())
            if msg.get("id") == i:
                return msg
    call(0, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                           "clientInfo": {"name": "lsp_compare", "version": "0"}})
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    p.stdin.flush()
    times = []
    for i, (tool, item) in enumerate(queries, 1):
        start = time.time()
        call(i, "tools/call", {"name": tool, "arguments": {"item": item}})
        times.append(time.time() - start)
    with open(f"/proc/{p.pid}/status") as f:
        rss = next(int(l.split()[1]) // 1024 for l in f if l.startswith("VmHWM"))
    p.stdin.close()
    p.wait(timeout=60)
    return {"first_call_seconds": round(times[0], 3), "median_seconds": round(statistics.median(times[1:]), 4),
            "max_seconds": round(max(times[1:]), 4), "calls": len(times), "peak_rss_mb": rss}


def score(found, true):
    hit = len(found & true)
    return {"found": len(found), "hit": hit, "recall": hit / len(true) if true else None,
            "precision": hit / len(found) if found else None}


def summary(out, g):
    """The numbers bench/LSP.md quotes."""
    rand = [t for t in out["targets"] if t["why"] == "random"]
    kinds = collections.Counter()
    for t in rand:
        a = {(f, ln): c for f, ln, c in t["atlas_sites"]}
        lsp, hop = {tuple(x) for x in t["lsp_sites"]}, {tuple(x) for x in t["lsp_hop_sites"]}
        kinds["same" if set(a) == lsp else "atlas has more" if lsp < set(a) else "lsp has more" if set(a) < lsp
              else "both differ"] += 1
        kinds.update(f"atlas-only site, {a[s]}" for s in set(a) - lsp)
        kinds["lsp-only site"] += len(lsp - set(a))
        if set(a) != lsp:
            kinds["with the trait hop: same" if hop == set(a) else "with the trait hop: still differs"] += 1
    print(f"\nWho calls it, {len(rand)} random functions:", dict(kinds))

    def walk(start, kinds):
        seen, queue = {start}, [start]
        while queue:
            for e in g["in"][queue.pop()]:
                if e["kind"] in kinds and e["from"] not in seen:
                    seen.add(e["from"])
                    queue.append(e["from"])
        return {(g["by_id"][i]["file"], g["by_id"][i]["line"]) for i in seen if i != start and g["by_id"][i].get("test")}

    for label, group in (("random functions", rand), ("probes", [t for t in out["targets"] if t["why"] != "random"])):
        ts = [t for t in group if "scores" in t]
        run = [t for t in ts if t["truth_in_process"]]
        true = sum(len(t["truth_in_process"]) for t in run)
        print(f"\nWhich tests run it, {label}: {len(ts)} measured, {len(run)} run by a test in its own process "
              f"({true} pairs); {sum(len(t['truth_subprocess_only']) for t in ts)} more pairs only inside a program a test starts")
        for tool in ("atlas", "lsp", "lsp_hop"):
            hit = sum(t["scores"][tool]["hit"] for t in run)
            listed = sum(t["scores"][tool]["found"] for t in run)
            print(f"  {tool:8} recall {hit}/{true} = {hit / true:.1%}, complete for {sum(t['scores'][tool]['hit'] == len(t['truth_in_process']) for t in run)}"
                  f" and empty for {sum(t['scores'][tool]['hit'] == 0 for t in run)} of {len(run)}; lists {listed} (precision {hit / max(listed, 1):.1%});"
                  f" lists tests for {sum(1 for t in ts if not t['truth_in_process'] and t['scores'][tool]['found'])} of the"
                  f" {len(ts) - len(run)} that no test runs")
        plain = [t for t in run if not t["atlas_fallback"]]
        calls = [(walk(t["node"], {"calls", "references"}), {tuple(x) for x in t["atlas_tests"]},
                  {tuple(x) for x in t["truth_in_process"]}, {tuple(x) for x in t["lsp_tests"]}) for t in plain]
        e_hit, e_n = sum(len(e & tr) for e, a, tr, l in calls), sum(len(e) for e, a, tr, l in calls)
        c_hit, c_n = sum(len((a - e) & tr) for e, a, tr, l in calls), sum(len(a - e) for e, a, tr, l in calls)
        print(f"  atlas through calls only: {e_hit} of {e_n} listed run it; the same tests as the language server's walk for "
              f"{sum(e == l for e, a, tr, l in calls)} of {len(calls)}. Through a trait: {c_hit} of {c_n}")
        for key in ("lsp_tests_cost", "lsp_hop_tests_cost"):
            req, rounds, chars = ([t[key][k] for t in ts] for k in ("requests", "rounds", "chars"))
            print(f"  {key}: requests median {statistics.median(req)}, max {max(req)}; rounds median {statistics.median(rounds)},"
                  f" max {max(rounds)}; characters median {statistics.median(chars)}, max {max(chars)}")
        chars = [t["atlas_tests_chars"] for t in ts]
        print(f"  atlas `tests`: 1 call; characters median {statistics.median(chars)}, max {max(chars)}")

    print("\nProbes:")
    for t in out["targets"]:
        if t["why"] != "random":
            s = t.get("scores")
            print(f"  {t['label'][:46]:46} callers atlas {len(t['atlas_sites']):2} lsp {len(t['lsp_sites']):2} hop {len(t['lsp_hop_sites']):2}"
                  f" refs {len(t['lsp_references']):2}" + (f" | tests: run by {len(t['truth_in_process'])} (+{len(t['truth_subprocess_only'])} via rg),"
                  f" atlas {s['atlas']['hit']}/{s['atlas']['found']}, lsp {s['lsp']['hit']}/{s['lsp']['found']},"
                  f" hop {s['lsp_hop']['hit']}/{s['lsp_hop']['found']}" if s else ""))


def main():
    mode, ws = sys.argv[1], os.path.abspath(sys.argv[2])
    g = load(ws)
    targets = pick(ws, g)
    if mode == "targets":
        json.dump([{k: t[k] for k in ("id", "file", "line", "name")} for t in targets], sys.stdout, indent=1)
        return
    reach = json.load(open(sys.argv[3]))
    lsp = Lsp(os.environ.get("CARGO_ATLAS_RUST_ANALYZER", "rust-analyzer"), ws)
    lsp.ready.wait(1200)
    agent = Agent(lsp, ws, g)
    out = {"lsp_ready_seconds": round(lsp.ready_at, 1), "targets": []}
    print(f"rust-analyzer quiescent after {lsp.ready_at:.1f} s")
    for t in targets:
        node = g["by_id"][t["node"]]
        a_sites = atlas_sites(g, t["node"])
        mark = agent.mark()
        item = agent.item(t["name"], t["file"], t["line"])
        plain = agent.sites(item, hop=False) if item else {}
        callers_cost = agent.since(mark)
        hop = agent.sites(item, hop=True) if item else {}
        refs = {(lsp.rel(x["uri"]), x["range"]["start"]["line"] + 1) for x in agent.references(t["name"], t["file"], t["line"])}
        text, secs = atlas_cli(ws, "callers", f"{node['file']}:{node['line']}")
        rec = dict(t, atlas_sites=sorted([*k, c] for k, c in a_sites.items()), lsp_sites=sorted(plain),
                   lsp_hop_sites=sorted(hop), lsp_references=sorted(refs), lsp_callers_cost=callers_cost,
                   atlas_callers_chars=len(text), atlas_callers_seconds=round(secs, 3))
        if t["id"] in reach["instrumented"]:
            inproc, subproc = truth(g, reach, t["id"])
            a_tests, fallback = atlas_tests(g, t["node"])
            text, secs = atlas_cli(ws, "tests", f"{node['file']}:{node['line']}")
            shown, more = cli_tests(text)
            assert shown <= a_tests and len(shown) + more == len(a_tests), (t["id"], len(shown), more, len(a_tests))
            mark = agent.mark()
            l_tests, capped, rounds = agent.tests(item, hop=False) if item else (set(), False, 0)
            tests_cost = dict(agent.since(mark), rounds=rounds)
            mark = agent.mark()
            h_tests, h_capped, h_rounds = agent.tests(item, hop=True) if item else (set(), False, 0)
            hop_cost = dict(agent.since(mark), rounds=h_rounds)
            rec.update(truth_in_process=sorted(inproc), truth_subprocess_only=sorted(subproc),
                       atlas_tests=sorted(a_tests), atlas_fallback=fallback, atlas_tests_chars=len(text),
                       atlas_tests_seconds=round(secs, 3), lsp_tests=sorted(l_tests), lsp_tests_cost=tests_cost,
                       lsp_tests_capped=capped, lsp_hop_tests=sorted(h_tests), lsp_hop_tests_cost=hop_cost,
                       lsp_hop_tests_capped=h_capped,
                       scores={"atlas": score(a_tests, inproc), "lsp": score(l_tests, inproc),
                               "lsp_hop": score(h_tests, inproc)})
        out["targets"].append(rec)
        s = rec.get("scores")
        print(f"{t['id']} {t['label'][:48]:48} callers atlas {len(a_sites):3} lsp {len(plain):3} hop {len(hop):3} refs {len(refs):3}"
              + (f" | tests truth {len(rec['truth_in_process']):3} atlas {s['atlas']['found']:3} ({s['atlas']['hit']:3})"
                 f" lsp {s['lsp']['found']:3} ({s['lsp']['hit']:3}) hop {s['lsp_hop']['found']:3} ({s['lsp_hop']['hit']:3})"
                 f" req {rec['lsp_tests_cost']['requests']}/{rec['lsp_hop_tests_cost']['requests']}" if s else ""),
              flush=True)
    out["lsp"] = {"requests": agent.requests, "request_seconds": round(agent.seconds, 1),
                  "peak_rss_mb": lsp.peak_rss_mb()}
    lsp.close()
    probes = [(tool, f"{t['file']}:{t['line']}") for t in targets for tool in ("callers", "tests")]
    out["atlas_server"] = mcp_server(ws, probes)
    json.dump(out, open(os.path.join(ws, ".atlas/lsp_compare.json"), "w"), indent=1)
    summary(out, g)
    print(f"\nrust-analyzer: quiescent after {out['lsp_ready_seconds']} s, {out['lsp']['requests']} requests in "
          f"{out['lsp']['request_seconds']} s, peak RSS {out['lsp']['peak_rss_mb']} MB")
    print(f"cargo atlas serve: first call {out['atlas_server']['first_call_seconds']} s, then median "
          f"{out['atlas_server']['median_seconds'] * 1000:.1f} ms over {out['atlas_server']['calls']} calls, peak RSS "
          f"{out['atlas_server']['peak_rss_mb']} MB")


if __name__ == "__main__":
    main()
