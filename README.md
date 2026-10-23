# cargo-atlas

A map of a Rust workspace that is as accurate as the compiler, built for AI
coding assistants such as Claude Code. Ask "who calls this?", "what implements
this trait?" or "which tests reach this function?" and get exact answers with
`file:line`, instead of an assistant grepping through files.

Status: prototype (0.1). The commands and the MCP server work on real
workspaces. Claude Code skills come next.

## Why

Tools that match function names guess. Two structs can both have a `parse()`,
and a name alone doesn't say which one `r.parse()` calls. rust-analyzer works
out the type of `r`, so it knows.

In the test fixture (`tests/fixtures/spike`), cargo-atlas links all 7 calls to
the exact function; Graphify, which matches names, links none of them. On
ripgrep 15.1.0:

- 50 random call links, checked by reading the code: 50 correct. With no error in 50, at least 94% of links are right, at 95% confidence.
- 150 random call sites taken from the source text: all 91 calls into a workspace function were found, so recall is at least 96%, at 95% confidence. The sample holds calls written as `name(...)`; calls Rust makes without naming them, such as `drop`, are outside it (see Limits).
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
cargo atlas path main CsvReader::parse # how main reaches it, call by call
cargo atlas explain run               # kind, location, signature, every link
cargo atlas search parse              # items whose name or path contains "parse"
cargo atlas tests JsonReader::parse   # tests that reach it, and the command that runs them
cargo atlas unsafe --missing          # unsafe code without its SAFETY comment
cargo atlas report                    # summary in .atlas/report.md
```

An item can be named three ways: `parse`, a path such as `JsonReader::parse`
(optionally with its crate first, `spike::json_reader::JsonReader::parse`), or
the location of its name, `src/json_reader.rs:10`. Generic arguments can be
left out (`GlobBuilder::build` for `GlobBuilder<'a>::build`), and a method in a
trait impl can be written `Square::area` for `<Square as Area>::area`.

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

`path` follows calls, including the impls behind a trait call:

```
$ cargo atlas path main CsvReader::parse
Shortest chain of calls from main to CsvReader::parse: 3 link(s)
  main
    --calls--> run   src/main.rs:17
    --may_call--> <CsvReader as Loader>::load   src/main.rs:10  (through the trait)
    --calls--> CsvReader::parse   src/loader.rs:9
```

`main` also names `CsvReader`, which contains `parse`: two links, but not how
`main` gets there. Only when no chain of calls connects two items, either way
round, does `path` fall back to any kind of link, and it says so.

A name that matches several items lists them:

```
$ cargo atlas callers parse
error: `parse` matches 2 items. Pick one by its full path or its location (for example `src/csv_reader.rs:10`):
  spike::csv_reader::CsvReader::parse  (src/csv_reader.rs:10)
  spike::json_reader::JsonReader::parse  (src/json_reader.rs:10)
```

Queries answer from the last build. When a file changed since then, they say
so on stderr: `note: 1 file changed (src/lib.rs) since the last build`. A new
file where Cargo finds targets on its own (`tests/`, `src/bin/`, `examples/`,
`benches/`) counts too: `note: 1 new file (tests/api.rs) since the last build`.

## Use it from Claude Code

`cargo atlas serve` answers the same questions as MCP tools on stdin and
stdout. Register it once:

```sh
claude mcp add --transport stdio cargo-atlas -- cargo-atlas serve
```

Add `--scope user` to have it in every project. In a folder that isn't a Cargo
workspace, the tools say so and do nothing else.

The tools are `callers`, `callees`, `impls`, `path`, `explain`, `search`,
`tests`, `unsafe_code` and `refresh`. Each returns the same text as the
command line. The server:

- does nothing until the first call, then loads `.atlas/graph.json`, or
  builds it if there is none (4 s for the edge fixture, 40 s for tokio);
- answers in 2 to 4 ms on tokio after that, including the check below;
- checks before every answer whether an indexed file or `Cargo.toml` changed
  since the build, or a new test, binary, example or bench file appeared. If
  so, the answer starts with a note, and a rebuild runs in the background:

  ```
  Note: 1 file changed (src/json_reader.rs) since the graph was built, so answers about
  code there may be out of date. A rebuild is running (the last build took 4 s); call
  `refresh` to wait for it.
  ```

- rebuilds and waits when `refresh` is called;
- cuts answers at 24,000 characters (about 6,000 tokens), below the size at
  which Claude Code warns.

A name that matches several items is an ordinary answer listing them, not an
error, since the next step is to pick one.

## Tests and unsafe code

`cargo atlas tests ITEM` walks the links backwards from ITEM to every function
marked `#[test]`, `#[tokio::test]`, `#[rstest]` and the like, and prints a
`cargo test` command that runs only those tests:

```
$ cargo atlas tests first_unchecked
Tests that reach first_unchecked  (edge-core/src/raw.rs:8)
  first_of_empty_is_zero  edge-core/src/raw.rs:79  via first_or_zero

Run them:
  cargo test -p edge-core --lib -- --exact raw::tests::first_of_empty_is_zero
```

The command picks the test binary each test is in (`--lib`, `--bin NAME`,
`--test NAME`), so Cargo builds only those, and names each test with
`--exact`, since libtest otherwise runs every test whose name contains a
filter: `tests::parse` would also run `tests::parse_twice`. Which binary a
file belongs to comes from following `mod` declarations from each target's
root file. A parameterized test (`#[rstest]`, `#[test_case]`) runs once per
case, each named after it, so those get a command of their own that matches
by name.

For a type it starts from the type's methods; for a trait, from the trait's
methods and every impl of them. Rust calls some methods without naming them:
`drop` when a value goes out of scope, `from` behind `?`, `next` in a `for`
loop, `fmt` in a format string. No call in the code reaches those, so for
such a method `tests` says so and lists the tests that use its type instead.

`cargo atlas unsafe` lists every `unsafe` block, fn, impl and trait, and
whether each explains itself the way clippy expects: a `// SAFETY:` comment
above a block or impl, a `# Safety` section in the docs of an unsafe fn or
trait. A `Safety:` line or a `# Safety` heading in a comment counts too, and an
`unsafe fn` in a trait impl is covered by the trait's docs. With an item, it
lists the unsafe code inside it, and for a function also the unsafe code its
calls reach:

```
$ cargo atlas unsafe first_or_zero
Unsafe code in first_or_zero  (edge-core/src/raw.rs:18): 1 site, 0 without a SAFETY comment or # Safety section
  edge-core/src/raw.rs:22  block  in first_or_zero  SAFETY comment

Reached through its calls: 2 sites, 0 without a SAFETY comment or # Safety section
  edge-core/src/raw.rs:8   fn     first_unchecked     safety documented
  edge-core/src/raw.rs:10  block  in first_unchecked  SAFETY comment
```

The check looks for the marker, not for prose: a comment that explains the
reasoning without `SAFETY:` counts as missing.

## How it works

Three sources feed one graph:

- `rust-analyzer scip` lists every definition and reference in the
  workspace, each resolved to one exact symbol, with each item's full span. A
  reference inside the span of a function, type, const or static becomes a
  link from it, so a static's initializer calls what it calls. An operator is
  a reference too: rust-analyzer resolves `a + b` to the `add` that runs.
- `cargo metadata` gives the crates, their dependencies and each target's
  root file.
- A pass with `syn` finds impl headers, `#[derive]` lists, test attributes,
  `unsafe` code, `mod` declarations and the calls written in `macro_rules!`
  bodies, including inside macro calls that wrap plain Rust, such as tokio's
  `cfg_rt! { ... }`. The index then says which exact items those names refer
  to.

rust-analyzer gets three settings. It turns on `cfg(miri)` by default, which
removes every item marked `#[cfg(not(miri))]` (40 of tokio's 153 test files),
so cargo-atlas turns it off. Features follow Cargo: the default ones, or
`--features a,b` and `--all-features` on `build` and `serve`. And the `cargo
check` it runs for build scripts and proc macros goes to
`target/rust-analyzer`, so a rebuild in the background never holds the lock on
`target/` that a `cargo test` needs.

Every link carries a confidence label:

| Label | Meaning |
| --- | --- |
| EXACT | Resolved by rust-analyzer or Cargo |
| CANDIDATE | One of several possible targets, as with `dyn Trait` calls |
| SYNTAX | Read from source text but not type-checked, as with derives and calls written in a `macro_rules!` body |

## Limits

- Blanket impls such as `impl<T: Area> Named for T` have no concrete type,
  so no type is linked to `Named`.
- Shared symbols: rust-analyzer gives some different items one symbol, such
  as `main` in `build.rs` and in `src/main.rs`. Each still gets its own node, but
  a link to one of them can be a guess; those links are marked CANDIDATE.
- Proc macros (`#[tokio::main]`, `sqlx::query!`) are expanded once they
  compile. Code from one that fails to build is missing.
- Code for other platforms and features is not in the graph: `#[cfg(windows)]`
  items on Linux, or a feature that isn't on. Unsafe code in such items is
  still listed, marked as not in the index.
- A test or `unsafe` block inside a `macro_rules!` body isn't seen: the body
  is a template, not code. A call written there has no occurrence in the
  index either, so the macro is linked to the function its path names, as
  SYNTAX, when exactly one function of the macro's package matches. Method
  calls in a body (`x.f()`) aren't linked.
- `tests` follows calls in the code. A test that runs the built program as a
  subprocess, as ripgrep's integration tests do, calls nothing in the graph,
  and neither does a call made through std, such as `"..".parse::<Glob>()`
  reaching `Glob::from_str`. Calls Rust makes without naming them (`drop`,
  `from` behind `?`, `next` in a `for` loop, `fmt` in `format!`) aren't in the
  code either; for those methods `tests` falls back to the tests that use the type.
- An operator is linked when rust-analyzer resolves it to an impl. It looks
  the impl up for the operand's type as written, so `ledger[0]` on a
  `&Ledger` resolves to `Index::index` in std, and isn't linked.
- `rust-analyzer scip` is not a stable interface. Tested with rust-analyzer
  0.3.3057 and with a build of its 2026-07-06 release.

Decisions and their reasons are logged in [`DECISIONS.md`](DECISIONS.md).

## Development

```sh
cargo test        # needs rust-analyzer and rust-src, like the tool itself
```

The end-to-end tests build three fixture workspaces in `tests/fixtures` and
compare the answers with ones written by reading the code. `tests/server.rs`
starts `cargo atlas serve` and talks MCP to it, as Claude Code does, including
an edit followed by `refresh`. Without rust-analyzer the tests fail with
install instructions; set `CARGO_ATLAS_SKIP_RA_TESTS=1` to skip them on
purpose, and they say that they checked nothing. CI runs fmt, clippy and the
tests on every push.

## License

MIT OR Apache-2.0, at your option.
