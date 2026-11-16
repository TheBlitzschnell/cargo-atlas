# Accuracy checks on ripgrep 15.1.0

Setup: rust-analyzer 2026-07-06 (commit 7ae18ed), Rust 1.97.0 with the
standard library's source, Graphify 0.9.67 with `--code-only`, on a 2-core
Linux VM. Run on 2026-11-16, after the 2026-10-23 changes (see
`DECISIONS.md`).

The first run, on 2026-09-25 and 2026-09-26, used Rust 1.95.0 and
rust-analyzer 0.3.3057. The cargo-atlas from before the 2026-10-23 changes,
run in the new setup, gives that run's counts and Graphify comparison again,
so the differences below come from those changes, not from the newer
rust-analyzer.

How cargo-atlas compares with the rust-analyzer language server that Claude
Code's LSP plugin runs, measured against tests that were run: `LSP.md`.

## Checked by reading the code

These checks don't rely on either tool: each sampled line was checked against
the source code.

| Check | Sample | Result |
| --- | --- | --- |
| Precision: is each call link right? | 50 random call links (two draws of 25, seeds 20260925 and 424242) | 50 of 50 correct: at least 94% at 95% confidence |
| Recall: is each call found? | 150 random call sites from the source text (seed 11), found with a regex, not with either tool | 73 of the 74 calls into workspace functions found: at least 93% at 95% confidence |
| SYNTAX links from `macro_rules!` bodies | All 21 | 21 of 21 point at the function the call's path names |

The bounds are one-sided 95% intervals: `0.05^(1/50) = 0.942` for no error in
50, and the exact (Clopper-Pearson) bound for one error in 74, 0.937. The
recall sample only holds calls written as `name(...)`; calls Rust makes
without naming them (`drop`, `from` behind `?`) are outside it.

The one miss is `set.is_match($path)` in a `macro_rules!` body
(`crates/globset/src/glob.rs:1169`): SYNTAX links cover calls written as a
path in a macro body, not method calls. The sample's other site in a macro
body, `gi_from_str(..)` at `crates/ignore/src/gitignore.rs:702`, has its
SYNTAX link. The 2026-09-25 run set macro bodies aside as templates; counted
that way, this sample gives 72 of 72, at least 95.9%.

Of the other 76 sites, 46 have names no workspace function has. The other 30
were read one by one: 24 call std or another crate (`push`, `len`,
`String::new`, termcolor's `NoColor::new`, pcre2's `jit_if_available`), 3 call
a derived `Clone` or `Default`, which has no source to link to, 2 are inside
string literals, and 1 calls a closure.

## What changed since 2026-09-25

Every link that differs between the maps from before and after the 2026-10-23
changes, listed with `diff_graphs.py` and read against the code:

- 108 impls of traits from std and other crates (`Display`, `serde::Serialize`):
  the trait was a SYNTAX node named by its last segment; it is now the trait's
  exact symbol, and the link is EXACT. 24 such traits.
- 5 operators went from `references` to `calls`: `*stats += ...` in `search`
  and `search_parallel` (`crates/core/main.rs:137`, `202`), `self + &rhs` and
  `*self += &rhs` (`crates/printer/src/stats.rs:111`, `134`), and
  `&*self.tokens` (`crates/globset/src/glob.rs:339`).
- 6 calls and 1 reference moved from two `Flag` methods to the statics
  declared inside them: `DOC` in `<HyperlinkFormat as Flag>::doc_long`, and
  `CHOICES` and `BORROWED` in `doc_choices` (`crates/core/flags/defs.rs`).
- 2 calls and 114 `uses_type` links are new, from consts and statics to what
  their initializers name: `HYPERLINK_PATTERN_ALIASES` calls `alias` and
  `prioritized_alias`, ripgrep's `FLAGS` table names 104 flag types and the
  `Flag` trait, and six glob test consts name `Options`.
- 21 SYNTAX calls are new, from `macro_rules!` bodies (the table above).

So function-to-function EXACT calls went from 5,948 to 5,947: 6 moved to
statics, 5 operators came in. Items, tests, unsafe sites and CANDIDATE links
stayed the same.

## Agreement with Graphify

| Measure | cargo-atlas | Graphify |
| --- | --- | --- |
| Function-to-function call links | 5,947 (EXACT) | 2,809 (2,684 EXTRACTED, 125 INFERRED) |
| Possible trait impls per call | 4,411 CANDIDATE links | none |
| Build time on the 2-core VM | 12 to 13 s (17 to 21 s the first time) | 3 s |

- 56.5% of Graphify's call links match what rust-analyzer resolved.
- Of the Graphify links where both ends are functions rust-analyzer knows, 39.3% (1,029 of 2,617) point to a different function.
- Graphify has 25.8% (1,537 of 5,947) of the call links rust-analyzer found.

Two disagreements checked by reading `crates/cli/src/decompress.rs` were both
Graphify errors, each a same-named function in the same file:

- `<DecompressionMatcherBuilder as Default>::default` calls `DecompressionMatcherBuilder::new()`. Graphify linked it to `DecompressionReader::new`.
- `DecompressionMatcherBuilder::build` calls `GlobSetBuilder::build` in the globset crate. Graphify linked it to `DecompressionReaderBuilder::build`.

## Checked every night

`.github/workflows/drift.yml` maps ripgrep 15.1.0 every night and runs
`drift_check.py` against `expected-ripgrep-15.1.0.toml`: ten counts from this
page, 14 links read against the code (a plain call, an operator, calls in a
static's and a const's initializer, a SYNTAX call from a macro body, a
CANDIDATE, impls of traits from std and serde, a derive, among others), and
Graphify's two wrong links above, which must stay out. Any difference fails.

One job uses rust-analyzer 2026-07-06, the release the links were checked
with, so if it fails, cargo-atlas changed. The other uses the latest release:
if only it fails, rust-analyzer changed what it reports. Both keep their graph
for 30 days. To list every link that differs between the two:

```sh
gh run download RUN_ID --dir drift        # one folder per graph
python3 bench/diff_graphs.py drift/graph-ra-2026-07-06/graph.json drift/graph-ra-latest/graph.json
```

Read each change against the code. When the new links are right, update
`expected-ripgrep-15.1.0.toml` (the check prints the new counts), and move the
job's pinned release forward to the one they were checked with.

## Other workspaces

From the 2026-09-25 run; not run again since.

| Workspace | Size | Build time | Notes |
| --- | --- | --- | --- |
| tokio 1.47.1 | 159k lines, 636 files | 36 to 45 s | 12,512 call links; 216 symbols shared by several items |
| mini-redis (2026-04-15) | small | 6 to 12 s | Calls inside `#[tokio::main]`, `#[instrument]` and `tokio::select!` found |

tokio had 11,196 call links before cargo-atlas turned off rust-analyzer's
default `cfg(miri)`, which had hidden every `#[cfg(not(miri))]` item.

## Tests and unsafe code

Checked on 2026-09-26 against text searches and by reading the code, and
again for ripgrep on 2026-11-16: the same numbers.

### Test functions

| | ripgrep | tokio |
| --- | --- | --- |
| Functions marked as tests (any test attribute) | 429 | 1,419 |
| `#[test]` lines found by a text search | 451 | 902 |
| ... marked as a test | 429 | 711 |
| ... inside a `macro_rules!` body (a template, not code) | 15 | 1 |
| ... behind a feature, a platform or another cfg, so not in the index | 7 | 190 |
| ... in the index but not marked | 0 | 0 |

tokio's cfg'd-out tests are mostly loom tests (`#[cfg(loom)]`) and
`tokio_unstable` ones.

The 15 templates matter more than their count. On Linux,
`cargo test --workspace -- --list` lists 1,144 tests in ripgrep's code, plus
24 doc tests. All 1,144 are in the index, but only the 429 above are marked
as tests; the other 715 are written through macros whose body holds the
`#[test]`:

- 329 integration tests from `rgtest!` (the 330th only runs on 32-bit
  targets). They run the built `rg` binary as a subprocess, so no call links
  them to library code either way.
- 386 unit tests in globset, ignore and grep-matcher (`matches!`, `toregex!`,
  `ignored!`, `find!` and others), which call library code. `tests
  GlobBuilder::build` lists none of the glob tests made with `matches!`.
  Marking them wouldn't be enough: the calls they make are written in the
  macro's body, so they are linked from the macro, or not at all for method
  calls such as `.build()`.

`tests GlobBuilder::build` on ripgrep lists tests up to five calls away, such
as `explicit_ignore` via `Gitignore::new -> GitignoreBuilder::add ->
GitignoreBuilder::add_line`. Each hop of that chain was checked in the source.

### Unsafe code

| | ripgrep | tokio |
| --- | --- | --- |
| Lines with `unsafe` in code, by text search (not comments, not `unsafe fn(..)` pointer types) | 5 | 878 |
| ... in files rust-analyzer didn't index | 0 | 59 |
| ... found | 5 | 806 |
| ... not found | 0 | 13: 11 in `macro_rules!` templates, 2 in a mocking DSL |
| Sites listed | 5 | 807 |
| Without their `// SAFETY:` comment or `# Safety` section | 0 | 484 |

In tokio, 50 of the sites are in code rust-analyzer skipped, such as
`#[cfg(windows)]` impls and a FreeBSD-only test file; they are listed without
an item.

The comment verdicts were read by hand on random tokio samples:

- The first sample of 18 had 4 wrong verdicts: two `# Safety` headings in
  comments, and two `unsafe fn`s in trait impls, whose contract is on the
  trait. Both are now accepted.
- A sample of 15 flagged sites had 1 wrong verdict: a `Safety:` line in an
  unsafe fn's docs. Now accepted.
- With the final rule: 15 documented sites (seed 7) and 20 flagged ones
  (seed 99), all 35 following the rule. 2 of the 20 explain their reasoning
  in a comment without the `SAFETY:` marker; the check looks for the marker,
  not for prose.

### MCP server

On tokio, the first call loads the 13.5 MB graph in about 120 ms. After
that, `callers`, `tests`, `unsafe_code` and `search` each answer in 2 to 4 ms,
including the check for changed files, which stats about 650 files.

## Reproduce

```sh
git clone https://github.com/BurntSushi/ripgrep && cd ripgrep && git checkout 15.1.0
cargo atlas build                         # writes .atlas/graph.json
python3 path/to/cargo-atlas/bench/drift_check.py .atlas/graph.json path/to/cargo-atlas/bench/expected-ripgrep-15.1.0.toml
uv tool install graphifyy==0.9.67         # Graphify's package name has two y's
graphify extract . --code-only            # writes graphify-out/graph.json
python3 path/to/cargo-atlas/bench/compare_graphify.py .
python3 path/to/cargo-atlas/bench/precision_sample.py . 25 20260925
python3 path/to/cargo-atlas/bench/precision_sample.py . 25 424242
python3 path/to/cargo-atlas/bench/recall_sample.py . 150 11
```

The two sample scripts print the lines to check; deciding whether each one is
right means reading the code. The recall script walks folders in sorted order
since 2026-11-16, so a seed draws the same sites on every machine; the
2026-09-25 sample can't be drawn again. `drift_check.py` needs Python 3.11 or
later, for `tomllib`.

Tests and unsafe code, in ripgrep or tokio after `cargo atlas build`:

```sh
python3 path/to/cargo-atlas/bench/test_coverage.py .
python3 path/to/cargo-atlas/bench/unsafe_coverage.py .
python3 path/to/cargo-atlas/bench/unsafe_sample.py . 20 99 missing
python3 path/to/cargo-atlas/bench/unsafe_sample.py . 15 7 documented
cargo test --workspace -- --list 2>/dev/null | grep -c ': test$'   # ripgrep: 1,168 with doc tests
```
