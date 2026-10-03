//! End-to-end test on the edge-case fixture: a two-crate workspace with
//! generics, blanket impls, nested modules, renamed imports, a local macro,
//! non-ASCII text, recursion, and two functions named `main` (a build script
//! and a binary) that rust-analyzer gives the same symbol.
//!
//! Every expectation below was written from reading the fixture's code
//! before running the tool.

mod common;

use common::{atlas, atlas_error, atlas_in, built, copy_of};

const F: &str = "edge";

/// The lines of `out` that mention `needle`.
fn rows<'a>(out: &'a str, needle: &str) -> Vec<&'a str> {
    out.lines().filter(|l| l.contains(needle)).collect()
}

#[test]
fn both_mains_exist_and_keep_their_own_calls() {
    if !built(F) {
        return;
    }
    let err = atlas_error(F, &["explain", "main"]);
    assert!(err.contains("matches 2 items"), "{err}");
    assert!(
        err.contains("edge-app/build.rs:1") && err.contains("edge-app/src/main.rs:4"),
        "{err}"
    );

    let out = atlas(F, &["callees", "edge-app/src/main.rs:4"]);
    for callee in [
        "total_dyn",
        "helper",
        "helper_local",
        "<Wrapper<T> as Area>::area",
    ] {
        assert!(
            out.contains(&format!("-> {callee} ")),
            "main should call {callee}:\n{out}"
        );
    }
    assert!(atlas(F, &["callees", "edge-app/build.rs:1"]).contains("(no calls found)"));
}

#[test]
fn a_renamed_import_still_reaches_the_original_function() {
    if !built(F) {
        return;
    }
    // edge-app calls `renamed_helper`, which is `util::helper` re-exported under another name.
    let out = atlas(F, &["callers", "helper"]);
    for caller in ["café_naïve", "inner", "main"] {
        assert!(
            out.contains(&format!("<- {caller} ")),
            "{caller} should call helper:\n{out}"
        );
    }
}

#[test]
fn non_ascii_text_before_a_call_does_not_hide_it() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["callees", "café_naïve"]);
    assert!(
        rows(&out, "-> helper ").iter().any(|r| r.contains("EXACT")),
        "{out}"
    );
}

#[test]
fn calls_inside_a_local_macro_are_found_and_the_macro_counts_as_a_call() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["callees", "uses_macro"]);
    assert!(out.contains("-> deep_fn "), "{out}");
    let twice = rows(&out, "-> twice ");
    assert!(
        twice.len() == 1 && !twice[0].contains("named, not called"),
        "{out}"
    );
}

#[test]
fn trait_calls_resolve_as_far_as_the_types_allow() {
    if !built(F) {
        return;
    }
    // A concrete receiver: rust-analyzer knows the impl.
    let out = atlas(F, &["callees", "square_area"]);
    assert!(out.contains("-> <Square as Area>::area "), "{out}");
    assert!(!out.contains("Circle"), "{out}");

    // `dyn Area` inside a closure: the trait method, plus every impl as a candidate.
    let out = atlas(F, &["callees", "total_dyn"]);
    assert!(
        rows(&out, "-> Area::area ")
            .iter()
            .any(|r| r.contains("EXACT")),
        "{out}"
    );
    for candidate in [
        "<Square as Area>::area",
        "<Circle as Area>::area",
        "<Wrapper<T> as Area>::area",
    ] {
        assert!(
            rows(&out, candidate)
                .iter()
                .any(|r| r.contains("CANDIDATE")),
            "{candidate} should be a candidate:\n{out}"
        );
    }

    // `map(Area::area)` names the method without calling it there.
    let out = atlas(F, &["callees", "total_generic"]);
    assert!(
        rows(&out, "-> Area::area ")
            .iter()
            .any(|r| r.contains("named, not called")),
        "{out}"
    );
}

#[test]
fn impls_include_generic_impls_but_not_blanket_impls_yet() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["impls", "Area"]);
    for ty in ["Square", "Circle", "Wrapper"] {
        assert!(
            out.contains(&format!("<- {ty} ")),
            "{ty} implements Area:\n{out}"
        );
    }
    // Known limitation: `impl<T: Area> Named for T` has no concrete self type,
    // so no type is linked to `Named`. If this starts failing, update the README.
    assert!(atlas(F, &["impls", "Named"]).contains("(no impls found)"));
}

#[test]
fn nested_modules_recursion_and_inner_functions() {
    if !built(F) {
        return;
    }
    assert!(atlas(F, &["callees", "calls_deep"]).contains("-> deep_fn "));
    let out = atlas(F, &["callers", "outer_rec"]);
    assert!(
        out.contains("<- outer ") && out.contains("<- outer_rec "),
        "{out}"
    );
    let out = atlas(F, &["callees", "outer"]);
    assert!(
        out.contains("-> inner ") && out.contains("-> outer_rec "),
        "{out}"
    );
}

#[test]
fn crates_and_cross_crate_calls() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["explain", "edge-app"]);
    assert!(out.contains("depends_on edge-core"), "{out}");
    let out = atlas(F, &["callers", "total_dyn"]);
    assert!(
        out.contains("<- main ") && out.contains("edge-app/src/main.rs:8"),
        "{out}"
    );
}

// ---------------------------------------------------------------------------
// Tests and unsafe code (edge-core/src/raw.rs, edge-core/tests/shapes.rs)
// ---------------------------------------------------------------------------

/// The row for `file:line` in an `unsafe` answer, with its spacing squeezed.
fn site(out: &str, location: &str) -> String {
    let row = out
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{location} ")))
        .unwrap_or_else(|| panic!("no row for {location}:\n{out}"));
    row.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn every_unsafe_site_is_found_with_its_comment_status() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["unsafe"]);
    assert!(
        out.starts_with(
            "Unsafe code in the workspace: 12 sites, 4 without a SAFETY comment or # Safety section"
        ),
        "{out}"
    );
    let r = "edge-core/src/raw.rs";
    let expected = [
        (8, "fn first_unchecked safety documented"),
        (10, "block in first_unchecked SAFETY comment"),
        (14, "fn last_unchecked no # Safety section"),
        (15, "block in last_unchecked no SAFETY comment"),
        // The comment sits above the `match`, two lines up from `unsafe`.
        (22, "block in first_or_zero SAFETY comment"),
        (30, "block in last_or_zero no SAFETY comment"),
        (39, "fn RawBuf::peek safety documented"),
        (41, "block in RawBuf::peek SAFETY comment"),
        (46, "impl Send for RawBuf SAFETY comment"),
        // A blank line and then code sit between this impl and the comment above.
        (48, "impl Sync for RawBuf no SAFETY comment"),
        (55, "trait Pointer safety documented"),
        (60, "impl Pointer for RawBuf SAFETY comment"),
    ];
    for (line, rest) in expected {
        assert_eq!(
            site(&out, &format!("{r}:{line}")),
            format!("{r}:{line} {rest}")
        );
    }
}

#[test]
fn unsafe_for_a_function_includes_what_its_calls_reach() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["unsafe", "first_or_zero"]);
    let (own, reached) = out.split_once("Reached through its calls").expect(&out);
    assert!(own.contains("raw.rs:22 "), "{out}");
    assert!(
        !own.contains("raw.rs:8 ") && !own.contains("raw.rs:10 "),
        "{out}"
    );
    assert!(
        reached.contains("raw.rs:8 ") && reached.contains("raw.rs:10 "),
        "{out}"
    );
    assert!(
        !reached.contains("raw.rs:14 "),
        "last_unchecked is not reachable:\n{out}"
    );

    let out = atlas(F, &["unsafe", "last_or_zero"]);
    assert!(
        out.contains("Reached through its calls: 2 sites, 2 without"),
        "{out}"
    );
    // Only what is missing its comment: the four sites marked "no ..." above.
    let out = atlas(F, &["unsafe", "--missing"]);
    assert!(
        out.starts_with("Unsafe code in the workspace: 4 sites, 4 without"),
        "{out}"
    );
    for line in [14, 15, 30, 48] {
        assert!(
            out.contains(&format!("raw.rs:{line} ")),
            "line {line}:\n{out}"
        );
    }
}

#[test]
fn unsafe_for_a_type_lists_its_impls_and_methods() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["unsafe", "RawBuf"]);
    assert!(out.contains(": 5 sites, 1 without"), "{out}");
    for line in [39, 41, 46, 48, 60] {
        assert!(
            out.contains(&format!("raw.rs:{line} ")),
            "line {line}:\n{out}"
        );
    }
    assert!(!out.contains("Reached through its calls"), "{out}");

    let out = atlas(F, &["unsafe", "edge-core"]);
    assert!(out.contains(": 12 sites, 4 without"), "{out}");
}

#[test]
fn tests_that_reach_a_function_directly_or_through_calls() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["tests", "first_unchecked"]);
    let row = rows(&out, "first_of_empty_is_zero ")[0];
    assert!(
        row.contains("edge-core/src/raw.rs:79") && row.contains("via first_or_zero"),
        "{out}"
    );
    assert!(
        out.contains("cargo test -p edge-core -- raw::tests::first_of_empty_is_zero"),
        "{out}"
    );

    let out = atlas(F, &["tests", "one_byte"]);
    assert!(
        rows(&out, "sum_of_one_byte ")[0].contains("direct"),
        "{out}"
    );
    // `one_byte` sits in the test module but has no #[test]: not a test.
    assert!(!atlas(F, &["explain", "one_byte"]).contains("test:"));
    assert!(atlas(F, &["explain", "sum_of_one_byte"]).contains("test:       yes"));

    let out = atlas(F, &["tests", "helper_local"]);
    assert!(
        out.contains("cargo test -p edge-app -- tests::calls_helper_local"),
        "{out}"
    );
    assert!(atlas(F, &["tests", "helper"]).contains("(no tests reach it)"));
}

#[test]
fn tests_in_the_tests_folder_and_through_dyn_calls() {
    if !built(F) {
        return;
    }
    // total_dyn takes `&[Box<dyn Area>]`, so it may run Circle's area.
    let out = atlas(F, &["tests", "<Circle as Area>::area"]);
    let row = rows(&out, "total_of_two_shapes ")[0];
    assert!(
        row.contains("edge-core/tests/shapes.rs:9")
            && row.contains("via total_dyn")
            && row.contains("(through a trait)"),
        "{out}"
    );
    assert!(!out.contains("square_area_is_four"), "{out}");
    assert!(
        out.contains("cargo test -p edge-core -- total_of_two_shapes"),
        "{out}"
    );

    let out = atlas(F, &["tests", "<Square as Area>::area"]);
    assert!(
        rows(&out, "square_area_is_four ")[0].contains("via square_area"),
        "{out}"
    );
    assert!(
        out.contains("cargo test -p edge-core -- square_area_is_four total_of_two_shapes"),
        "{out}"
    );

    // A trait: tests that reach its methods or any impl of them.
    let out = atlas(F, &["tests", "Area"]);
    for test in ["square_area_is_four", "total_of_two_shapes"] {
        assert!(out.contains(&format!("  {test} ")), "{test}:\n{out}");
    }
}

#[test]
fn search_finds_items_by_part_of_the_name() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["search", "area", "--kind", "trait"]);
    assert!(out.starts_with("1 item matches `area`:"), "{out}");
    assert!(
        out.contains("edge_core::shapes::Area") && out.contains("edge-core/src/shapes.rs:1"),
        "{out}"
    );

    // Exact names come before longer ones.
    let out = atlas(F, &["search", "area"]);
    let first = out.lines().nth(1).unwrap_or("");
    let position = |needle: &str| {
        out.lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing:\n{out}"))
    };
    assert!(first.contains("edge_core::shapes::Area "), "{out}");
    assert!(
        position("edge_core::shapes::Area ") < position("::square_area "),
        "{out}"
    );

    assert!(atlas(F, &["search", "nothing_like_this"]).contains("Nothing matches"));
    assert!(atlas_error(F, &["search", "x", "--kind", "bogus"]).contains("unknown kind"));
}

#[test]
fn items_can_be_named_without_generics_or_the_trait() {
    if !built(F) {
        return;
    }
    // `<Wrapper<T> as Area>::area`, written the short way.
    let out = atlas(F, &["callees", "Wrapper::area"]);
    assert!(
        out.starts_with("<Wrapper<T> as Area>::area  (edge-core/src/shapes.rs:39)"),
        "{out}"
    );
    // `Square::area` is only the trait impl; there is no inherent one.
    assert!(atlas(F, &["callers", "Square::area"]).contains("<- square_area "));
    // Several impls have `area`: the short form lists them.
    let err = atlas_error(F, &["callers", "area"]);
    assert!(err.contains("matches 4 items"), "{err}");
}

#[test]
fn a_path_through_a_re_export_offers_the_items_with_that_name() {
    if !built(F) {
        return;
    }
    // lib.rs has `pub use shapes::Area;`, so users may write `edge_core::Area`.
    let err = atlas_error(F, &["impls", "edge_core::Area"]);
    assert!(
        err.contains("Items named `Area`")
            && err.contains("edge_core::shapes::Area  (edge-core/src/shapes.rs:1)"),
        "{err}"
    );
}

#[test]
fn code_that_rust_analyzer_would_hide_under_miri_is_in_the_graph() {
    if !built(F) {
        return;
    }
    // `#[cfg(not(miri))]`: gone if rust-analyzer's default `cfg(miri)` were on.
    assert!(atlas(F, &["callees", "not_under_miri"]).contains("-> helper "));
    // `#[cfg(feature = "extra")]`: not a default feature.
    assert!(atlas_error(F, &["explain", "only_with_extra"]).contains("nothing named"));
}

#[test]
fn a_feature_turned_on_at_build_time_brings_its_code_in() {
    if !built(F) {
        return;
    }
    let dir = copy_of(F, "edge-with-extra");
    let summary = atlas_in(&dir, &["build", "--features", "extra"]);
    assert!(
        summary
            .trim_end()
            .ends_with("with default features plus extra"),
        "{summary}"
    );
    let out = atlas_in(&dir, &["callees", "only_with_extra"]);
    assert!(out.contains("-> outer "), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
