//! End-to-end test on the edge-case fixture: a two-crate workspace with
//! generics, blanket impls, nested modules, renamed imports, a local macro,
//! non-ASCII text, recursion, and two functions named `main` (a build script
//! and a binary) that rust-analyzer gives the same symbol.
//!
//! Every expectation below was written from reading the fixture's code
//! before running the tool.

mod common;

use common::{atlas, atlas_error, built};

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
