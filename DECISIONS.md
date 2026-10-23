# Decisions

One entry per decision: the date, what was decided, why, and what was rejected.
Newest first.

## 2026-10-23: `path` follows calls first
- Bug: `path` searched every kind of link in both directions, so `path main CsvReader::parse` answered `main --uses_type--> CsvReader --contains--> CsvReader::parse`: a type mention and containment, not how `main` reaches `parse`, which is what the MCP tool promised.
- Now: the shortest chain of calls (`calls`, `may_call`, `references`, and a last `uses_type` when the goal is a type) from the first item to the second; then the other way round; only then any link, with a first line saying which it found. A type or trait starts from its methods, as in `tests`.
- Rejected: dropping the any-link fallback. Two items that only share a type are still worth showing, said for what it is.

## 2026-10-23: Test commands name each test exactly and pick its binary
- Bug: libtest runs every test whose name contains a filter, so `-- raw::tests::first_of_empty_is_zero` also ran `first_of_empty_is_zero_slowly`, and `-p` alone builds every test binary of the package (150 integration tests in tokio).
- Now: `--exact` with each test's full path, and `--lib`, `--bin NAME`, `--test NAME` (or `--example`, `--bench`) for the binaries the tests are in. Checked with `cargo test` on the implicit fixture: exactly the listed tests run. `#[rstest]`, `#[test_case]` and `#[test_matrix]` tests run once per case, named after the function (`adds::case_1`), so they get a command of their own without `--exact`.
- Which target a file belongs to: `cargo metadata` gives each target's root file; the rest is found by following `mod` declarations as rustc does (`net.rs` keeps `mod tcp;` in `net/tcp.rs`; `mod.rs` and files loaded through `#[path]` own their folder). Saved as `targets` in `graph.json`, format version 4. When a test's file maps to no target, its package's command falls back to `-p` alone.
- Rejected: cargo-nextest filter expressions, which not everyone has. The same map could split shared symbols such as `main` in `build.rs` and `src/main.rs` exactly; not done yet.

## 2026-10-23: Traits from outside the workspace keep their full path
- Bug: such a trait was a node named by its last segment, so `std::fmt::Write` and `std::io::Write`, or `std::error::Error` and `serde::de::Error`, were one node, linked as SYNTAX.
- Now: the impl header's line has an occurrence of the trait with its exact symbol (`core … fmt/Write#`), so the node uses it and the link is EXACT. When a line names two such traits (`impl<W: io::Write> fmt::Write for Both<W>`), syn's column for the trait picks the occurrence; syn's columns are now converted to bytes, the unit of the index. `std::fmt::Write` finds `core::fmt::Write`, which std re-exports.
- Bug: `impls Debug` couldn't be answered when one type derived `Debug` and another implemented it by hand: two nodes named `Debug` without a location, and the list of choices suggested `src/main.rs:4`. Now `impls` answers for the trait and the derive of the same name together, rows from the derive marked `(derive)`, when only one trait from outside has that name. A derive can be picked by its id, `derive:Debug`; the list of choices shows names that pick each item, and its example is one of them.
- Correction: the builder's comment said a trait from outside the workspace has no symbol in the index, which is why the 2026-09-25 entry lists such traits under SYNTAX. It has one at every reference; what the index lacks for it is `SymbolInformation` (kind, signature, docs), which rust-analyzer writes only for files under the workspace root.

## 2026-10-23: Operators are calls; consts and statics own references
- Bug: rust-analyzer resolves `a + b`, `-x`, `*p`, `v[i]` and `.await` to the method that runs and records it on the operator. No `(` follows an operator, so the link was `references`, printed "named, not called". An occurrence whose text isn't a name is now a call.
- Bug: a reference inside a const or static belonged to nothing, so `static CONFIG: LazyLock<Config> = LazyLock::new(|| load_config())` gave `load_config` no caller, and `tests` missed the tests that read `CONFIG`. Consts and statics now own the references in their initializers, like functions and types.
- Found: rust-analyzer looks an operator's impl up for the operand's type as written, so `ledger[0]` on a `&Ledger` resolves to `Index::index` in std and isn't linked. Listed in the README's limits.

## 2026-10-23: Calls Rust makes without naming them
- `drop`, `from` behind `?` (rust-analyzer resolves `?` to `Try::branch` only), `next` in a `for` loop and `fmt` in a format string have no occurrence. For a method of an impl of a trait from outside the workspace that no call reaches, `callers` says why the list is empty, and `tests` lists the tests that use the impl's type (through its methods, or functions, consts and statics that name it) instead of "no tests reach it". For choosing which tests to run, missing one is worse than running a few extra.
- Rejected: linking each `?` to the `From` impls of its error type by reading types from the source: guesswork, and the fallback through the type covers what `tests` needs.

## 2026-10-23: Calls in `macro_rules!` bodies, matched by path (SYNTAX)
- Found on the new fixture: rust-analyzer records no occurrence for a call written in a macro body, and none where the macro is used, so `report -> log_err! -> write_line` broke after the macro.
- Now: the syn pass reads each rule's transcriber (`=> { ... }`, not the matcher) and the builder links the macro to the function a call's path names (`$crate::log::write_line`) when exactly one callable of the macro's package matches. SYNTAX, noted "in the macro's body, matched by path". Method calls, macro variables (`$f(...)`) and definitions are skipped.
- Rejected: matching the last name across the workspace, Graphify's approach, which links a `new` to the wrong `new`.

## 2026-10-23: Freshness notices new target files
- Bug: only files the build read were checked, so a new `tests/api.rs`, a new target that no `mod` line names, went unnoticed.
- Now: the build records the files where Cargo finds targets on its own (`src/main.rs`, `src/lib.rs`, `build.rs`, and `*.rs` or `*/main.rs` in `src/bin`, `tests`, `examples`, `benches`), and every check lists them again: a few `read_dir` calls per package.
- Rejected: reporting any new `.rs` file. A new module file changes nothing until a `mod` line names it, and that line's file is stamped.

## 2026-10-23: rust-analyzer builds in `target/rust-analyzer` and runs in the workspace
- Its `cargo check` for build scripts and proc macros used the workspace's `target/`, so a background rebuild held Cargo's lock on it and an assistant's `cargo test` waited. `cargo.targetDir: true` moves that build to `target/rust-analyzer`, at the cost of building the dependencies once more there.
- It is also started in the workspace, so rustup picks the toolchain the workspace's `rust-toolchain.toml` names, as it already did for `rustc --print sysroot`.

## 2026-10-23: Skipped end-to-end tests say so
- With `CARGO_ATLAS_SKIP_RA_TESTS=1`, the end-to-end tests report `ok` while checking nothing. Each test binary now prints a warning once, written straight to stderr because libtest hides `eprintln!` output from tests that pass.

## 2026-09-26: Compare against Claude Code's LSP tool next
- Asked on LinkedIn: Claude Code's LSP tool already gets callers and trait impls from rust-analyzer, so what does this add? Its answers need a file and a position for each question and come one symbol at a time; cargo-atlas answers by name, across the workspace, with every call site, plus tests and unsafe code. Whether that saves an assistant any work is unmeasured.
- Plan: 10 questions about ripgrep, each answered by Claude Code with the LSP tool alone and with cargo-atlas alone. Count tool calls, tokens and wrong answers.

## 2026-09-26: Queries accept the names people write
- `GlobBuilder::build` finds `GlobBuilder<'a>::build`, and `Square::area` finds `<Square as Area>::area`. Found while checking ripgrep: the exact form failed with "nothing named".
- A path through a re-export (`tokio::task::spawn_blocking`, defined in `task/blocking.rs`) lists the items with that last name instead of failing.
- Exact matches still come first, so the full form always picks one item.

## 2026-09-26: rust-analyzer runs with `cfg(miri)` off, and features are a choice
- Bug found on tokio: rust-analyzer's `cargo.cfgs` setting defaults to `["debug_assertions", "miri"]`, so every `#[cfg(not(miri))]` item was missing, including 40 of tokio's 153 test files. cargo-atlas now passes a config with `debug_assertions` only. tokio went from 11,196 call links to 12,512.
- `--features` and `--all-features` on `build` and `serve`. The graph records its features; a server rebuild keeps them unless `serve` was given its own.
- Also fixed: a relative `RUST_SRC_PATH` was accepted by our check but ignored by rust-analyzer, which silently dropped 18% of ripgrep's call links. It is now made absolute before rust-analyzer sees it.

## 2026-09-26: Tests and unsafe code, from the syn pass
- Tests: functions whose attribute's last segment is `test` or ends in `_test`, or is `rstest`, `test_case` or `quickcheck`. `#[cfg(test)]` alone doesn't make a test. `tests ITEM` walks `calls`, `may_call` and `references` links backwards and prints a `cargo test` command with each test's full path as the filter: precise, never runs an unrelated test.
- Unsafe: blocks, fns, impls and traits. "Documented" follows clippy's convention: a comment with `SAFETY:` above a block or impl (blank lines and attributes between are fine, and so is a comment above the whole statement), a `# Safety` section in an unsafe fn's or trait's docs. Also accepted, after checking tokio: a `# Safety` heading in a comment, a `Safety:` line in the docs, and an `unsafe fn` in a trait impl, whose contract is on the trait.
- Macro calls whose contents parse as Rust are walked, which found 78 more sites in tokio's `cfg_*! { ... }` blocks. `macro_rules!` bodies and `quote!` are skipped: templates, not code.
- Rejected: judging whether prose explains soundness. A comment without the marker counts as missing; the output says which rule it applies.

## 2026-09-26: The graph notices edits; rebuilds run in the background
- Each build stamps every file it read (size, modification time, FNV-1a hash of the content) plus the Cargo manifests. A check stats every file and hashes only those whose size or time moved: 2 to 4 ms per answer on tokio.
- The server checks before every answer. Changed files are named in a note at the top of the answer, and a rebuild starts in the background; `refresh` waits for one. After a failed rebuild, automatic rebuilds stop until `refresh` succeeds, so a broken setup doesn't rebuild on every call.
- A file saved while rust-analyzer runs gets an empty hash, so it counts as changed and the next check rebuilds.
- Rejected: a file watcher. More code, platform differences, and it would rebuild while nobody asks anything. Rejected: rebuilding before every answer, which costs 10 to 40 seconds each time.

## 2026-09-26: MCP server on rmcp, the official Rust SDK
- `cargo atlas serve` exposes callers, callees, impls, path, explain, search, tests, unsafe_code and refresh. Each returns the command line's text. A list of matches for an ambiguous name is an ordinary answer, not an error, since picking one is the next step.
- rmcp handles protocol versions: the handshake with a 2025-06-18 client works, and the current spec is 2026-07-28. With tokio it takes the build from 33 crates to 76. Rejected: hand-written JSON-RPC over stdio, about 200 lines, but every protocol change would be ours to track.
- Async stays in `src/server.rs`; builds run on tokio's blocking pool. Nothing starts before the first call, because an assistant may start the server in every project.
- Answers are cut at 24,000 characters, under Claude Code's 10,000-token warning.

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
