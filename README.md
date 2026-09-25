# cargo-atlas

A map of a Rust workspace that is as accurate as the compiler, built for AI
coding assistants such as Claude Code. Ask "who calls this?" or "what implements
this trait?" and get exact answers with `file:line`, instead of an assistant
grepping through files.

Status: prototype (0.1). The commands below work on real workspaces. An MCP
server and Claude Code skills come next.

## Why

Tools that match function names guess. Two structs can both have a `parse()`,
and a name alone doesn't say which one `r.parse()` calls. rust-analyzer works
out the type of `r`, so it knows.

In the test fixture (`tests/fixtures/spike`), cargo-atlas links all 7 calls to
the exact function; Graphify, which matches names, links none of them. On
ripgrep 15.1.0:

- 50 random call links, checked by reading the code: 50 correct.
- 150 random call sites taken from the source text: every call into a workspace function was found.
- cargo-atlas finds 5,948 function-to-function call links, Graphify 2,809, and 39% of Graphify's comparable links point at a different function than rust-analyzer resolved.

Details and how to reproduce: [`bench/README.md`](bench/README.md).

## Install

```sh
rustup component add rust-analyzer rust-src
cargo install --path .          # from a clone; a crates.io release comes later
```

`rust-src` matters: without the standard library's source, rust-analyzer can't
expand macros like `println!` or infer std types, and calls hidden there go
missing. `cargo atlas build` warns if it's absent.

## Use

```sh
cargo atlas build                     # index the workspace into .atlas/graph.json
cargo atlas callers JsonReader::parse # who calls it
cargo atlas callees run               # what it calls
cargo atlas impls Loader              # types implementing a trait (or a type's traits)
cargo atlas path main CsvReader::parse
cargo atlas explain run               # kind, location, signature, every link
cargo atlas report                    # summary in .atlas/report.md
```

An item can be named three ways: `parse`, a path such as `JsonReader::parse`
(optionally with its crate first, `spike::json_reader::JsonReader::parse`), or
the location of its name, `src/json_reader.rs:10`.

Output on the fixture:

```
$ cargo atlas callees run
run  (src/main.rs:9)
  -> Loader::load                  src/main.rs:10  EXACT
  -> <CsvReader as Loader>::load   src/main.rs:10  CANDIDATE  (through the trait)
  -> <JsonReader as Loader>::load  src/main.rs:10  CANDIDATE  (through the trait)
```

`run` takes a `&dyn Loader`, so the impl that runs is chosen at runtime. The
call to the trait method is exact; the two impls are candidates. When a
function calls the same thing several times, every line is listed, such as
`src/lib.rs:12,30,41`.

A name that matches several items lists them:

```
$ cargo atlas callers parse
error: `parse` matches 2 items. Pick one by its full path or its location (for example `src/csv_reader.rs:10`):
  spike::csv_reader::CsvReader::parse  (src/csv_reader.rs:10)
  spike::json_reader::JsonReader::parse  (src/json_reader.rs:10)
```

## How it works

Three sources feed one graph:

- `rust-analyzer scip` lists every definition and reference in the
  workspace, each resolved to one exact symbol, with each item's full span. A
  reference inside a function's span becomes a link from that function.
- `cargo metadata` gives the crates and their dependencies.
- A pass with `syn` finds impl headers and `#[derive]` lists. The index then
  says which exact type and trait each header names.

Every link carries a confidence label:

| Label | Meaning |
| --- | --- |
| EXACT | Resolved by rust-analyzer or Cargo |
| CANDIDATE | One of several possible targets, as with `dyn Trait` calls |
| SYNTAX | Read from source text but not type-checked, as with derives |

## Limits

- Blanket impls such as `impl<T: Area> Named for T` have no concrete type,
  so no type is linked to `Named`.
- Shared symbols: rust-analyzer gives some different items one symbol, such
  as `main` in `build.rs` and in `src/main.rs`. Each still gets its own node, but
  a link to one of them can be a guess; those links are marked CANDIDATE.
- Proc macros (`#[tokio::main]`, `sqlx::query!`) are expanded once they
  compile. Code from one that fails to build is missing.
- Only default Cargo features are indexed.
- `rust-analyzer scip` is not a stable interface. Tested with rust-analyzer 0.3.3057.

Decisions and their reasons are logged in [`DECISIONS.md`](DECISIONS.md).

## Development

```sh
cargo test        # needs rust-analyzer and rust-src, like the tool itself
```

The end-to-end tests build two fixture workspaces in `tests/fixtures` and
compare the answers with ones written by reading the code. Without
rust-analyzer they fail with install instructions; set
`CARGO_ATLAS_SKIP_RA_TESTS=1` to skip them on purpose. CI runs fmt, clippy and
the tests on every push.

## License

MIT OR Apache-2.0, at your option.
