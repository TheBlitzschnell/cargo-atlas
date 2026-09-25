//! End-to-end test on the spike fixture, the 7-call example from the README.
//!
//! The expected answers come from reading the fixture's code, not from running
//! any tool. The tests drive the real binary, exactly as a user would.

mod common;

use common::{atlas, atlas_error, built};

const F: &str = "spike";

#[test]
fn all_seven_calls_are_found() {
    if !built(F) {
        return;
    }
    let expected = [
        ("main", "JsonReader::new"),
        ("main", "JsonReader::parse"),
        ("main", "CsvReader::new"),
        ("run", "Loader::load"),
        ("<CsvReader as Loader>::load", "CsvReader::parse"),
        ("<JsonReader as Loader>::load", "JsonReader::parse"),
    ];
    for (caller, callee) in expected {
        let out = atlas(F, &["callees", caller]);
        assert!(
            out.contains(&format!("-> {callee} ")),
            "{caller} should call {callee}:\n{out}"
        );
    }

    // `run(&c)` sits inside `println!`. rust-analyzer only sees it when it can
    // expand println!, which needs the standard library's source (rust-src).
    let out = atlas(F, &["callers", "run"]);
    assert!(
        out.contains("<- main "),
        "main -> run is missing. Is the rust-src component installed? \
         (rustup component add rust-src, or set RUST_SRC_PATH)\n{out}"
    );
}

#[test]
fn the_two_parse_methods_are_never_mixed_up() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["callers", "JsonReader::parse"]);
    assert!(out.contains("<- main "), "{out}");
    assert!(out.contains("<- <JsonReader as Loader>::load "), "{out}");
    assert!(
        !out.contains("CsvReader"),
        "a CsvReader caller leaked in:\n{out}"
    );
}

#[test]
fn a_dyn_call_links_the_trait_method_and_lists_the_candidates() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["callees", "run"]);
    let line_with = |needle: &str| {
        out.lines()
            .find(|l| l.contains(needle))
            .unwrap_or("")
            .to_string()
    };
    assert!(line_with("-> Loader::load ").contains("EXACT"), "{out}");
    assert!(
        line_with("<CsvReader as Loader>::load").contains("CANDIDATE"),
        "{out}"
    );
    assert!(
        line_with("<JsonReader as Loader>::load").contains("CANDIDATE"),
        "{out}"
    );
}

#[test]
fn trait_impls_and_derives_are_attached_to_the_structs() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["impls", "Loader"]);
    assert!(
        out.contains("CsvReader") && out.contains("JsonReader"),
        "{out}"
    );

    let out = atlas(F, &["impls", "CsvReader"]);
    for expected in ["Loader", "Debug", "Clone"] {
        assert!(
            out.contains(expected),
            "CsvReader should list {expected}:\n{out}"
        );
    }
}

#[test]
fn ambiguous_names_list_the_choices_with_locations() {
    if !built(F) {
        return;
    }
    let err = atlas_error(F, &["callers", "parse"]);
    assert!(err.contains("matches 2 items"), "{err}");
    assert!(
        err.contains("spike::csv_reader::CsvReader::parse  (src/csv_reader.rs:10)"),
        "{err}"
    );
    assert!(
        err.contains("spike::json_reader::JsonReader::parse  (src/json_reader.rs:10)"),
        "{err}"
    );
}

#[test]
fn a_location_or_a_crate_path_picks_one_item() {
    if !built(F) {
        return;
    }
    let by_location = atlas(F, &["callers", "src/json_reader.rs:10"]);
    let by_path = atlas(F, &["callers", "spike::json_reader::JsonReader::parse"]);
    assert!(
        by_location.starts_with("JsonReader::parse  (src/json_reader.rs:10)"),
        "{by_location}"
    );
    assert_eq!(by_location, by_path);
    assert!(
        atlas_error(F, &["callers", "src/json_reader.rs:11"]).contains("nothing is defined at")
    );
}

#[test]
fn callers_of_a_struct_points_to_explain() {
    if !built(F) {
        return;
    }
    let err = atlas_error(F, &["callers", "CsvReader"]);
    assert!(
        err.contains("is a struct") && err.contains("cargo atlas explain CsvReader"),
        "{err}"
    );
}
