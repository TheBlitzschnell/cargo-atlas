# Accuracy checks on ripgrep 15.1.0

Setup: Rust 1.95.0, rust-analyzer 0.3.3057 with the standard library's source,
Graphify 0.9.67 with `--code-only`. Run on 2026-09-25, and again on 2026-09-26
after the changes for tests, unsafe code and `cfg(miri)`: the same numbers.

## Checked by reading the code

These checks don't rely on either tool: each sampled line was checked against
the source code.

| Check | Sample | Result |
| --- | --- | --- |
| Precision: is each call link right? | 50 random call links (two draws of 25, seeds 20260925 and 424242) | 50 of 50 correct |
| Recall: is each call found? | 150 random call sites from the source text (seed 11), found with a regex, not with either tool | All 91 calls into workspace functions found |

In the recall sample, the other 59 sites were calls into std or other crates
(`push`, `as_bytes`, `Duration::default`), closures, or text that only looks
like a call (a raw string, a doc comment, a `macro_rules!` body).

## Agreement with Graphify

| Measure | cargo-atlas | Graphify |
| --- | --- | --- |
| Function-to-function call links | 5,948 (EXACT) | 2,809 (2,684 EXTRACTED, 125 INFERRED) |
| Possible trait impls per call | 4,411 CANDIDATE links | none |
| Build time | 11 to 15 s | 8 s |

- 56.5% of Graphify's call links match what rust-analyzer resolved.
- Of the Graphify links where both ends are functions rust-analyzer knows, 39.3% (1,029 of 2,617) point to a different function.
- Graphify has 25.8% of the call links rust-analyzer found.

Two disagreements checked by reading `crates/cli/src/decompress.rs` were both
Graphify errors, each a same-named function in the same file:

- `<DecompressionMatcherBuilder as Default>::default` calls `DecompressionMatcherBuilder::new()`. Graphify linked it to `DecompressionReader::new`.
- `DecompressionMatcherBuilder::build` calls `GlobSetBuilder::build` in the globset crate. Graphify linked it to `DecompressionReaderBuilder::build`.

## Other workspaces

| Workspace | Size | Build time | Notes |
| --- | --- | --- | --- |
| tokio 1.47.1 | 159k lines, 636 files | 36 to 45 s | 12,512 call links; 216 symbols shared by several items |
| mini-redis (2026-04-15) | small | 6 to 12 s | Calls inside `#[tokio::main]`, `#[instrument]` and `tokio::select!` found |

tokio had 11,196 call links before cargo-atlas turned off rust-analyzer's
default `cfg(miri)`, which had hidden every `#[cfg(not(miri))]` item.

## Tests and unsafe code

Checked on 2026-09-26 against text searches and by reading the code.

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
`tokio_unstable` ones. ripgrep's 330 integration tests come from an `rgtest!`
macro and run the built `rg` binary as a subprocess, so no call links them to
library code either way.

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
uv tool install graphifyy==0.9.67         # Graphify's package name has two y's
graphify extract . --code-only            # writes graphify-out/graph.json
python3 path/to/cargo-atlas/bench/compare_graphify.py .
python3 path/to/cargo-atlas/bench/precision_sample.py . 25 424242
python3 path/to/cargo-atlas/bench/recall_sample.py . 150 11
```

The two sample scripts print the lines to check; deciding whether each one is
right means reading the code.

Tests and unsafe code, in ripgrep or tokio after `cargo atlas build`:

```sh
python3 path/to/cargo-atlas/bench/test_coverage.py .
python3 path/to/cargo-atlas/bench/unsafe_coverage.py .
python3 path/to/cargo-atlas/bench/unsafe_sample.py . 20 99 missing
python3 path/to/cargo-atlas/bench/unsafe_sample.py . 15 7 documented
```
