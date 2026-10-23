//! End-to-end tests on the implicit fixture: calls the code doesn't spell
//! out (an operator, `?`, `Drop`, a macro body, a static's initializer),
//! two traits with one name, a trait both derived and written by hand, and a
//! package with a library, a binary and an integration test.
//!
//! Every expectation below was written from reading the fixture's code
//! before running the tool.

mod common;

use common::{atlas, atlas_error, atlas_in, built, copy_of, fixture, run_in};

const F: &str = "implicit";

/// The lines of `out` that mention `needle`.
fn rows<'a>(out: &'a str, needle: &str) -> Vec<&'a str> {
    out.lines().filter(|l| l.contains(needle)).collect()
}

#[test]
fn an_operator_is_a_call() {
    if !built(F) {
        return;
    }
    // `a + b` calls `<Money as Add>::add`, though no `(` follows the `+`.
    let out = atlas(F, &["callers", "Money::add"]);
    let total = rows(&out, "<- total ");
    assert!(
        total.len() == 1
            && total[0].contains("src/money.rs:38")
            && total[0].contains("EXACT")
            && !total[0].contains("named, not called"),
        "{out}"
    );
    // `ledger[0]` calls `<Ledger as Index>::index`.
    assert!(atlas(F, &["callers", "Ledger::index"]).contains("<- first_entry "));
}

#[test]
fn calls_in_a_static_or_const_initializer_belong_to_it() {
    if !built(F) {
        return;
    }
    assert!(atlas(F, &["callers", "load_config"]).contains("<- CONFIG "));
    assert!(atlas(F, &["callers", "default_port"]).contains("<- DEFAULT_PORT "));
    let out = atlas(F, &["tests", "load_config"]);
    assert!(
        rows(&out, "port_comes_from_the_config ")[0].contains("via port -> CONFIG"),
        "{out}"
    );
    // Reading a static is not naming a function.
    assert!(!out.contains("names a function"), "{out}");
}

#[test]
fn a_call_written_in_a_macro_body_is_linked_from_the_macro() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["callers", "write_line"]);
    assert!(
        rows(&out, "<- log_err ")
            .iter()
            .any(|r| r.contains("src/log.rs:10") && r.contains("SYNTAX")),
        "{out}"
    );
    let out = atlas(F, &["tests", "write_line"]);
    assert!(
        rows(&out, "report_writes_a_line ")[0].contains("via report -> log_err"),
        "{out}"
    );
}

#[test]
fn methods_rust_calls_implicitly_fall_back_to_the_tests_that_use_their_type() {
    if !built(F) {
        return;
    }
    // `?` in parse_money calls this `from`, but no token names it.
    let out = atlas(F, &["callers", "MoneyError::from"]);
    assert!(
        out.contains("no callers found. `From` is a trait from outside the workspace"),
        "{out}"
    );
    let out = atlas(F, &["tests", "MoneyError::from"]);
    assert!(
        out.contains("These tests use MoneyError, so they may run it:"),
        "{out}"
    );
    for test in [
        "parses_cents",
        "rejects_garbage",
        "runs_the_arguments",
        "api_parses_money",
    ] {
        assert!(out.contains(&format!("  {test} ")), "{test}:\n{out}");
    }
    assert!(!out.contains("describes_money"), "{out}");

    let out = atlas(F, &["tests", "Guard::drop"]);
    assert!(
        out.contains("`Drop` is a trait from outside the workspace"),
        "{out}"
    );
    assert!(
        rows(&out, "guard_is_released ")[0].contains("uses Guard"),
        "{out}"
    );
    assert!(
        out.contains("cargo test -p implicit --test api -- --exact guard_is_released"),
        "{out}"
    );
}

#[test]
fn traits_from_outside_the_workspace_keep_their_full_paths() {
    if !built(F) {
        return;
    }
    let err = atlas_error(F, &["impls", "Write"]);
    assert!(
        err.contains("matches 2 items")
            && err.contains("core::fmt::Write  (outside the workspace)")
            && err.contains("std::io::Write  (outside the workspace)"),
        "{err}"
    );
    // `Both`'s impl line names `io::Write` in a bound and implements `fmt::Write`.
    let fmt = atlas(F, &["impls", "std::fmt::Write"]);
    assert!(
        fmt.contains("<- Text ") && fmt.contains("<- Both ") && !fmt.contains("Bytes"),
        "{fmt}"
    );
    assert!(rows(&fmt, "<- Text ")[0].contains("EXACT"), "{fmt}");
    let io = atlas(F, &["impls", "std::io::Write"]);
    assert!(
        io.contains("<- Bytes ") && !io.contains("Text") && !io.contains("Both"),
        "{io}"
    );
}

#[test]
fn a_trait_derived_and_one_written_by_hand_answer_together() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["impls", "Debug"]);
    assert!(rows(&out, "<- Bytes ")[0].contains("EXACT"), "{out}");
    for derived in ["Money", "MoneyError", "Text"] {
        assert!(
            rows(&out, &format!("<- {derived} "))[0].contains("(derive)"),
            "{derived}:\n{out}"
        );
    }
    // The derive alone, by its id.
    assert!(!atlas(F, &["impls", "derive:Debug"]).contains("Bytes"));
}

#[test]
fn a_location_names_a_whole_file() {
    if !built(F) {
        return;
    }
    // src/domain.rs ends in "main.rs" and has an item on line 4; src/main.rs doesn't.
    let err = atlas_error(F, &["explain", "main.rs:4"]);
    assert!(err.contains("nothing is defined at `main.rs:4`"), "{err}");
    assert!(atlas(F, &["explain", "domain.rs:4"]).starts_with("on_line_four\n"));
}

#[test]
fn test_commands_select_each_tests_target_and_exact_name() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["tests", "parse_money"]);
    assert!(
        out.contains(
            "  cargo test -p implicit --bin implicit --lib --test api -- --exact api_parses_money \
             cli::tests::runs_the_arguments money::tests::parses_cents \
             money::tests::rejects_garbage\n"
        ),
        "{out}"
    );
    // tests/common/mod.rs belongs to the `api` test, through `mod common;`.
    assert!(
        atlas(F, &["tests", "cents"])
            .contains("cargo test -p implicit --test api -- --exact api_parses_money")
    );
    // `mod tcp;` in src/net.rs is src/net/tcp.rs.
    assert!(
        atlas(F, &["tests", "connect"])
            .contains("cargo test -p implicit --lib -- --exact net::tcp::tests::connects")
    );
    // `#[path = "extra/renamed.rs"] pub mod renamed;`
    assert!(
        atlas(F, &["tests", "renamed_fn"])
            .contains("cargo test -p implicit --lib -- --exact renamed::tests::renamed_works")
    );
}

#[test]
fn path_follows_calls_and_says_when_none_connect() {
    if !built(F) {
        return;
    }
    let out = atlas(F, &["path", "main", "parse_money"]);
    assert!(
        out.starts_with("Shortest chain of calls from main to parse_money: 2 link(s)"),
        "{out}"
    );
    assert!(
        out.contains("--calls--> run ") && out.contains("--calls--> parse_money "),
        "{out}"
    );
    // `format!("{m}")` calls `fmt` without naming it; only the type connects them.
    let out = atlas(F, &["path", "describe", "Money::fmt"]);
    assert!(
        out.starts_with("No chain of calls connects describe and <Money as Display>::fmt"),
        "{out}"
    );
}

#[test]
fn a_new_test_file_is_noticed_before_the_next_build() {
    if !built(F) {
        return;
    }
    let dir = copy_of(F, "implicit-new-file");
    atlas_in(&dir, &["build"]);
    let stderr = |args: &[&str]| String::from_utf8_lossy(&run_in(&dir, args).stderr).to_string();
    assert_eq!(stderr(&["callers", "parse_money"]), "");

    // A new file in tests/ is a new test target; no file the build read changed.
    std::fs::write(
        dir.join("tests/brand_new.rs"),
        "#[test]\nfn brand_new() {}\n",
    )
    .unwrap();
    let note = stderr(&["callers", "parse_money"]);
    assert!(
        note.contains("1 new file (tests/brand_new.rs) since the last build"),
        "{note}"
    );

    atlas_in(&dir, &["build"]);
    assert_eq!(stderr(&["callers", "parse_money"]), "");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rust_analyzer_builds_in_a_target_folder_of_its_own() {
    if !built(F) {
        return;
    }
    // Not `target/debug`, whose lock a `cargo test` started meanwhile would wait for.
    assert!(fixture(F).join("target/rust-analyzer").is_dir());
}
