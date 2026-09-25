//! Helpers shared by the end-to-end tests.
//!
//! The tests drive the real binary against small fixture workspaces with
//! known answers. They need rust-analyzer and the standard library's source
//! (`rustup component add rust-analyzer rust-src`). Without them the tests
//! fail with a clear message instead of passing without checking anything;
//! set `CARGO_ATLAS_SKIP_RA_TESTS=1` to skip them on purpose.

// Each test file uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Mutex, OnceLock};

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Runs `cargo-atlas <args> --dir <fixture>`.
pub fn run(fixture_name: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-atlas"))
        .args(args)
        .arg("--dir")
        .arg(fixture(fixture_name))
        .output()
        .expect("cargo-atlas should start")
}

/// Runs a command that must succeed and returns what it printed.
pub fn atlas(fixture_name: &str, args: &[&str]) -> String {
    let output = run(fixture_name, args);
    assert!(
        output.status.success(),
        "cargo-atlas {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("output should be UTF-8")
}

/// Runs a command that must fail and returns its error message.
pub fn atlas_error(fixture_name: &str, args: &[&str]) -> String {
    let output = run(fixture_name, args);
    assert!(
        !output.status.success(),
        "cargo-atlas {args:?} should have failed"
    );
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn rust_analyzer_available() -> bool {
    let binary =
        std::env::var_os("CARGO_ATLAS_RUST_ANALYZER").unwrap_or_else(|| "rust-analyzer".into());
    Command::new(binary)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Builds a fixture's graph once per test binary. Returns false when the
/// tests are skipped on purpose; panics when rust-analyzer is simply missing.
pub fn built(fixture_name: &str) -> bool {
    static BUILT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    if std::env::var_os("CARGO_ATLAS_SKIP_RA_TESTS").is_some() {
        eprintln!("skipping: CARGO_ATLAS_SKIP_RA_TESTS is set");
        return false;
    }
    assert!(
        rust_analyzer_available(),
        "rust-analyzer not found. Install it with `rustup component add rust-analyzer rust-src`, \
         point CARGO_ATLAS_RUST_ANALYZER at a binary, or set CARGO_ATLAS_SKIP_RA_TESTS=1 to skip"
    );
    let mut done = BUILT
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !done.iter().any(|f| f == fixture_name) {
        atlas(fixture_name, &["build"]);
        done.push(fixture_name.to_string());
    }
    true
}
