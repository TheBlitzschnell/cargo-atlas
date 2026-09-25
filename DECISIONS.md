# Decisions

One entry per decision: the date, what was decided, why, and what was rejected.
Newest first.

## 2026-09-25: Items that share a symbol each get their own node (bug fix)
- Bug: rust-analyzer gives one symbol to `main` in `build.rs` and `main` in `src/main.rs`, to every example's `main`, and to same-named items declared inside different functions (rust-analyzer issue #18771). The first definition won and the rest vanished with their calls: ripgrep's real `main` was missing, and mini-redis kept 1 of its 6 `main`s.
- Fix: every definition becomes a node; a shared symbol gets ids like `symbol @file:line`. A reference picks the definition in its own file; when that is still a guess, the link is CANDIDATE, not EXACT.
- Rejected: mapping files to Cargo targets to split symbols exactly. More correct, much more code; revisit if CANDIDATE links from this show up often.

## 2026-09-25: Every call site is kept on each link
- Links store every line they were seen on (`lines`). Queries should show each call site, and the recall check needed them too. `graph.json` format version 2.

## 2026-09-25: Macro invocations count as calls; queries accept locations and crate paths
- `twice!(...)` is a `calls` link to the macro, not "named, not called".
- `cargo atlas callees src/main.rs:4` picks the item defined on that line; `spike::json_reader::JsonReader::parse` picks by crate and path. Ambiguous names list both forms.

## 2026-09-25: End-to-end tests fail without rust-analyzer
- They used to pass without checking anything. Now they fail with install instructions, unless `CARGO_ATLAS_SKIP_RA_TESTS=1` asks to skip them.
- CI (`.github/workflows/ci.yml`) runs fmt, clippy and the tests with the toolchain from `rust-toolchain.toml`.

## 2026-09-25: The `println!` gap was a setup problem; rust-src is required
- The first prototype missed a call inside `println!` because the `rust-src` component was missing, not because of rust-analyzer. With the standard library's source, rust-analyzer expands `println!` and all 7 of 7 calls are linked.
- So `rust-toolchain.toml` installs `rust-src`, `cargo atlas build` warns when it's missing, and the fixture test fails with a hint if it is.
- Proc macros: in mini-redis, calls inside `#[tokio::main]`, `#[instrument]` and `tokio::select!` were found once rust-analyzer could build the macros. A proc macro that fails to build still hides its code.

## 2026-09-25: Show at most 8 CANDIDATE links per query
- On ripgrep, one call to a `Flag` trait method has 104 possible impls. The rest are summarized with a pointer to `cargo atlas impls`.

## 2026-09-25: `path` skips crates and modules on its first search
- Everything sits inside a crate, so a path through one says nothing about how code connects. A second search uses every link if the first finds nothing.

## 2026-09-25: Test target is ripgrep 15.1.0, not the latest commit
- ripgrep's main branch needs Rust 1.96. Release 15.1.0 (2025-10-22) needs 1.85, which fits the pinned 1.95.0.

## 2026-09-25: Build on rust-analyzer's index, not tree-sitter
- Why: in the 7-call example, Graphify (tree-sitter plus name matching) linked 0 calls. rust-analyzer's index linked all 7, each to the exact function. On ripgrep 15.1.0 it found 5,946 call links to Graphify's 2,809, and 39% of Graphify's comparable links pointed at a different function (`bench/README.md`).
- Rejected: tree-sitter only, which can't tell two `parse` methods apart. rust-analyzer's library crates (`ra_ap_*`), which are large and change their API every week.
- Risk: `rust-analyzer scip` is not a stable interface ("subcommands ... may be removed or changed without notice"). Pin the version, and keep symbol parsing in `src/symbols.rs` with tests.

## 2026-09-25: syn finds impl headers and derives; the index resolves them
- Why: the index names impl methods with short names only (`impl#[CsvReader][Loader]load().`), and two traits can share a name. Derived impls never appear in the index.
- How: syn gives the line of each impl header and derive list. The index says which exact symbol each name on that line refers to.
- Rejected: matching trait names as strings.

## 2026-09-25: Every link carries a confidence label
- EXACT: resolved by rust-analyzer or Cargo. CANDIDATE: one of several possible targets, as with `dyn Trait` calls. SYNTAX: read from source text, not type-checked, as with derives and traits from outside the workspace.
- Why: whoever reads the graph should know which links are facts and which are possibilities.

## 2026-09-25: `calls` only when the source shows a call
- A function name followed by `(` or `::<` is `calls`. Otherwise it is `references`: `map(Self::parse)` names `parse` without calling it there.

## 2026-09-25: Standalone tool, written in Rust
- Goal: learning and portfolio.
- Rejected: a Python version (easier to check, but no Rust practice); contributing to Graphify (less work, but someone else's project); a plugin with Rust skills and no graph (easy to copy).
- Stop rule: if the ripgrep test shows a recall lead over Graphify of under 20 points, send the importer to Graphify instead.

## 2026-09-25: Toolchain pinned to Rust 1.95.0
- Why: builds and test output stay reproducible; the toolchain moves on purpose, not by accident.
- The ripgrep numbers came from the standalone rust-analyzer 0.3.3057. `rust-toolchain.toml` installs the one that ships with 1.95.0, which CI runs.

## 2026-09-25: Name `cargo-atlas`, license `MIT OR Apache-2.0`
- The name was unclaimed on crates.io on 2026-09-25. The dual license is the Rust convention.
- Repo: `github.com/TheBlitzschnell/cargo-atlas`.

## 2026-09-25: Dev-dependencies left out of `depends_on`
- They only matter for tests, and leaving them out keeps the crate graph readable. Revisit if a query needs them.
