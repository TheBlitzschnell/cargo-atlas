# Contributing

## Commands

```sh
cargo build
cargo test                                   # needs rust-analyzer and rust-src
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run -- build --dir tests/fixtures/spike
cargo run -- callers JsonReader::parse --dir tests/fixtures/spike
cargo run -- serve --dir tests/fixtures/spike   # speaks MCP on stdin/stdout
```

The end-to-end tests run the binary against the fixture workspaces, so they
need `rustup component add rust-analyzer rust-src`. Set
`CARGO_ATLAS_RUST_ANALYZER` to use a different rust-analyzer binary, or
`CARGO_ATLAS_SKIP_RA_TESTS=1` to skip those tests.

## Rules

- The toolchain is pinned to Rust 1.95.0 in `rust-toolchain.toml`. Don't use newer features.
- One idea per module. Each module starts with a `//!` comment saying what it does and why.
- Async code stays in `src/server.rs`. Everything else is plain, blocking Rust.
- Nothing but MCP messages may reach stdout while `serve` runs, so child processes have their output captured.
- Plain, explicit code over clever code. If a Rust feature isn't obvious, a comment explains it where it first appears.
- Every graph link carries a confidence label (EXACT, CANDIDATE, SYNTAX). A guessed link is never stored as EXACT.
- `tests/fixtures/spike` stays exactly as it is, since the README's 7-call comparison uses it. New cases go in a new fixture.
- New decisions go in `DECISIONS.md` (date, decision, why, what was rejected), newest first.

## Layout

| Path | What it holds |
| --- | --- |
| `src/main.rs` | The command line |
| `src/pipeline.rs` | The whole build: index, graph, file stamps, `graph.json` |
| `src/cargo_meta.rs` | Reading `cargo metadata` |
| `src/rust_analyzer.rs` | Running `rust-analyzer scip` with its settings, and reading the index |
| `src/symbols.rs` | Taking rust-analyzer's symbol strings apart |
| `src/syntax.rs` | The syn pass: impl headers, derives, test attributes, unsafe code |
| `src/builder.rs` | Index, Cargo and syntax facts into one graph |
| `src/model.rs` | Nodes, edges, confidence labels, unsafe sites, file stamps |
| `src/freshness.rs` | Telling whether `graph.json` still matches the files |
| `src/query.rs` | callers, callees, impls, path, explain, search, tests, unsafe |
| `src/report.rs` | `report.md` |
| `src/server.rs` | `cargo atlas serve`: the MCP tools and background rebuilds |
| `tests/spike.rs`, `tests/edge.rs` | End-to-end tests on the fixtures in `tests/fixtures` |
| `tests/server.rs` | The MCP server over the real protocol, including an edit and `refresh` |
| `tests/common/mod.rs` | Helpers shared by the end-to-end tests |
| `bench/` | The ripgrep checks: agreement with Graphify, precision and recall samples |
| `.github/workflows/ci.yml` | fmt, clippy and tests on every push |
