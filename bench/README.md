# Accuracy checks on ripgrep 15.1.0

Setup: Rust 1.95.0, rust-analyzer 0.3.3057 with the standard library's source,
Graphify 0.9.67 with `--code-only`. Run on 2026-09-25.

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
| Build time | 11 s | 8 s |

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
| tokio 1.47.1 | 159k lines, 636 files | 33 to 48 s | 11,196 call links; 186 symbols shared by several items |
| mini-redis (2026-04-15) | small | 6 to 12 s | Calls inside `#[tokio::main]`, `#[instrument]` and `tokio::select!` found |

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
