# cargo-atlas and the rust-analyzer language server

Claude Code's `rust-analyzer-lsp` plugin starts `rust-analyzer` with no
settings and gives Claude an `LSP` tool: definitions, references, types,
symbols, implementations and call hierarchies
([docs](https://code.claude.com/docs/en/tools-reference.md#lsp-tool-behavior)).
cargo-atlas builds its graph from the same rust-analyzer. This page compares
the two interfaces on the same analysis: rust-analyzer 2026-07-06 on both
sides, ripgrep 15.1.0, run on 2026-11-16.

Two questions an assistant asks before it changes a function: who calls it,
and which tests run it.

## How it was measured

- Functions: 40 picked at random (seed 7) from ripgrep's library and program
  code, not tests, and 12 picked by hand to probe known differences and a few
  common cases ("Probes" below).
- Who calls it: cargo-atlas's `callers` against the language server's call
  hierarchy (`prepareCallHierarchy`, then `incomingCalls`), call site by call
  site. `references` too.
- Which tests run it: the truth comes from running them. `test_reach.py` adds
  a line at the top of each function that records that it ran, then runs each
  of ripgrep's 1,144 tests alone. A test counts when the function ran in the
  test's own process. Tests that only run it inside the `rg` program they
  start are left out (2,319 such pairs for the random functions): neither
  tool sees through a subprocess.
- The language server has no request for tests, so `lsp_compare.py` walks
  incoming calls up to the tests the way an ideal agent would: it never asks
  twice, and it knows a test by the `#[test]` above it, read for free. Its
  request counts are a floor; a real agent also has to find each function's
  position first. A second walk also hops from the method of an impl to its
  trait method's callers, as an agent who knows Rust would, for 3 more
  requests per hop.

## Who calls it

| 40 random functions | |
| --- | --- |
| Same call sites | 27, 8 of them with no callers either way |
| cargo-atlas has more | 13: 72 call sites, all CANDIDATE |
| The language server has more | 0 |

The 13 are methods of impls of ripgrep's own traits, called only through the
trait: `flag.name_long()` on a `&dyn Flag`, `sink.matched(..)` on a generic
`S: Sink`. The call hierarchy of such a method is empty, and so is
`references`. An agent that asks about the trait's method instead gets the
same call sites for 10 of the 13. For the other 3, the trait method's callers
also include calls to other impls, such as `NoIgnore.update(..)` inside
another flag's `update`.

The language server takes two requests once the agent knows the function's
file, line and column; cargo-atlas takes one, and a name will do
(`callers LineBuffer::fill`). The language server's symbol search finds types
by default, finds functions only when the query ends with `#` (`fill#`), and
finds nothing for `LineBuffer::fill`.

## Which tests run it

For the 22 random functions that some test runs, 849 pairs of a test and a
function:

| | Found | Listed, and how many of those run it | Requests | Rounds |
| --- | --- | --- | --- | --- |
| cargo-atlas `tests` | 849 (100%), all of them for every function | 2,534, 34% | 1 | 1 |
| Call hierarchy, walked up to the tests | 424 (50%); none at all for 15 of the 22 | 580, 73% | median 1, max 31 | median 1, max 12 |
| ... hopping to trait methods | 847 (99.8%) | 2,292, 37% | median 13, max 213 | median 5, max 15 |

A round is a batch of requests that can go out at once: the walk can't ask
about a caller before it knows it.

cargo-atlas's list has two parts. Take the 21 functions with a call in the
code (the 22nd is a `Display::fmt` that only a format string calls; `tests`
answers it with the tests that use its type, and finds the 2 that run it).
For each of the 21, the tests cargo-atlas reaches through calls alone are
exactly the tests the call hierarchy walk finds: 580 listed, 424 of which run
the function. The tests it reaches only through a call behind a trait, a
CANDIDATE link, are 1,712 more, and 423 of them run the function. So for a
function behind a trait, `tests` lists far more tests than run it:
`<NoIgnoreDot as Flag>::update` is run by one test, and `tests` lists 104
tests of the flag parser, which calls `update` through `&dyn Flag`.

The answers are of similar size: cargo-atlas's `tests` prints a median of
6,300 characters (at most 23,300); the replies of the walk with hops add up
to a median of 9,500 (at most 30,600), counting one line per call site.

## Probes

| Function | Why it was picked | Callers: cargo-atlas, call hierarchy, references | Tests that run it in-process, and how many each found (with hops) |
| --- | --- | --- | --- |
| `<Stats as AddAssign<&Stats>>::add_assign` | run by an operator, `*stats += ...` | 3, 0, 0 | none (13 only inside `rg`); cargo-atlas lists 138 |
| `set_errored` | called only in a `macro_rules!` body | 1 (SYNTAX), 0, 0 | none (24 only inside `rg`) |
| `alias` | a `const fn` called in a const's initializer | 9, 0, 9 | can't be measured: no runtime call |
| `hyperlink_aliases` | called in the initializers of statics | 7, 7, 8 | 4: cargo-atlas 4, walk 3 (4) |
| `<RegexMatcher as Matcher>::find_at` | reached through a generic `M: Matcher` | 4 (CANDIDATE), 0, 0 | 52: cargo-atlas 52, walk 0 (52) |
| `<StandardSink as Sink>::matched` | reached through a generic `S: Sink` | 3 (CANDIDATE), 0, 0 | 74: cargo-atlas 74, walk 0 (74) |
| `Matcher::find_at` | a trait method with impls in four crates | 4, 26, 32 | can't be measured: no body |
| `GlobBuilder::build` | the README's `tests` example | 3, 3, 3 | 391: both 44 |
| `Gitignore::matched` | called directly by many tests | 16, 16, 16 | 109: both 42 |
| `LineBuffer::fill`, `Searcher::search_slice` | a same-named caller; many test helpers | the same | 133 and 37: both all |
| `<Glob as FromStr>::from_str` | reached through std, `"..".parse::<Glob>()` | 0, 0, 0 | none; cargo-atlas lists 58 tests that use `Glob` |

Where the language server is ahead:

- A trait method's callers: for `Matcher::find_at`, the call hierarchy also
  lists 22 calls that resolve to an impl, such as the tests in
  `crates/searcher/src/testutil.rs` calling their `RegexMatcher`'s `find_at`.
  cargo-atlas lists those under each impl, not under the trait method.
- Blanket impls: for `Sink` it lists 9 impls and cargo-atlas 7, missing
  `impl<S: Sink> Sink for &mut S` and `Box<S>`; for `Matcher` 6 and 5. For
  `Flag`, both list 104.

Neither finds the tests written through macros. The 347 tests that run
`GlobBuilder::build` and that both miss, of 391, and the 67 of the 109 that
run `Gitignore::matched`, are all made by macros such as `matches!`,
`toregex!` and `ignored!`, whose bodies rust-analyzer doesn't index (see
"Test functions" in `README.md` here).

## What each costs

| | rust-analyzer server | cargo-atlas |
| --- | --- | --- |
| Before the first answer | 14 s from a fresh clone (10 s in one `cargo test` had built), 4.6 to 5.4 s after; every session | a build: 17 to 21 s from a fresh clone, 12 s after; again when the code changes |
| Memory | 0.8 GB once indexed, 1.6 GB after the 2,140 requests of this comparison | 25 MB for `cargo atlas serve`; the build peaks at 1 GB, in rust-analyzer |
| Per answer | 12 to 13 ms per request on average | 0.4 to 0.6 ms (median of 104 calls; the first call, which loads the graph, 0.07 to 0.1 s) |
| After an edit | answers about the new code at once | notices the change, says so, rebuilds in the background (12 s) |
| Claude Code cloud sessions | not started: "Claude Code doesn't start plugin language servers" ([docs](https://code.claude.com/docs/en/plugins/code-intelligence.md)) | a server in the repository's `.mcp.json` starts ([docs](https://code.claude.com/docs/en/cloud-environments.md)), given cargo-atlas and rust-analyzer in the environment; not tried |

On a 2-core Linux VM, over four runs; times are wall-clock.

## What it means

- For a function called directly, both give the same callers and the same
  tests: it is the same rust-analyzer. The language server answers about code
  as it is being edited and needs no build.
- cargo-atlas adds what the call hierarchy doesn't show: calls through a
  trait, operators, macro bodies, const initializers. And it answers "which
  tests" and "how does A reach B" in one call, where the language server
  takes a walk. An agent that knows to hop to trait methods gets nearly the
  same tests from the language server, at a median of 13 requests in 5
  rounds.
- Through a trait, cargo-atlas lists about four tests for each one that runs
  the function; the call hierarchy finds none of them.
- Both miss tests written through macros, and calls inside a program a test
  starts.

So the two go together: the language server for definitions, types and code
being edited; cargo-atlas for the questions that span the workspace.

What to improve next, from these numbers:

1. Split `tests` in two: the tests reached through calls (73% of them run
   the function here) and those reached only through a trait (25%), each
   with its own command.
2. Tests written through macros: mark them and link them to what the
   macro's body calls.
3. `callers` on a trait method: also list the calls that resolve to its
   impls.
4. `impls`: include blanket impls.

## Reproduce

```sh
cd ripgrep                                   # 15.1.0, after `cargo atlas build`
python3 path/to/cargo-atlas/bench/lsp_compare.py targets . > /tmp/targets.json
mkdir /tmp/ripgrep-copy && git archive HEAD | tar -x -C /tmp/ripgrep-copy   # test_reach.py rewrites it
python3 path/to/cargo-atlas/bench/test_reach.py /tmp/ripgrep-copy /tmp/targets.json /tmp/reach.json
python3 path/to/cargo-atlas/bench/lsp_compare.py run . /tmp/reach.json
```

`lsp_compare.py run` prints the numbers of the tables above, from a warm
start, and writes the call sites and tests of each function to
`.atlas/lsp_compare.json`, from which the examples were read. The cold start,
the build's memory, the impl counts and the symbol search were checked apart,
with the same rust-analyzer. It needs cargo-atlas and rust-analyzer on the
PATH, or in `$CARGO_ATLAS` and `$CARGO_ATLAS_RUST_ANALYZER`. `test_reach.py`
builds the copy's tests and runs each test alone: under a minute for ripgrep
here.
