//! Running rust-analyzer and loading the index it writes.
//!
//! `rust-analyzer scip <dir>` resolves every name in the workspace the way the
//! compiler does, then writes the result as a SCIP index: a protobuf file
//! listing each definition and each reference, with exact symbols.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use protobuf::Message;
use scip::types::Index;
use serde_json::json;

use crate::model::Features;

/// Environment variable that points at a specific rust-analyzer binary.
pub const BINARY_ENV: &str = "CARGO_ATLAS_RUST_ANALYZER";

/// True when the standard library's source code is where rust-analyzer looks.
///
/// Without it, rust-analyzer can't expand std macros such as `println!` or
/// work out std types such as iterators, so calls hidden there go missing.
/// It comes from `rustup component add rust-src`, or from `$RUST_SRC_PATH`.
pub fn std_sources_available(workspace_root: &Path) -> bool {
    if let Some(path) = rust_src_path() {
        return path.join("core").is_dir();
    }
    // Run rustc inside the workspace so its rust-toolchain.toml picks the toolchain.
    let Ok(output) = Command::new("rustc")
        .args(["--print", "sysroot"])
        .current_dir(workspace_root)
        .output()
    else {
        return false;
    };
    let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Path::new(&sysroot)
        .join("lib/rustlib/src/rust/library/core")
        .is_dir()
}

/// `$RUST_SRC_PATH`, made absolute. rust-analyzer ignores a relative one
/// without a word, and then every call it could only see with the standard
/// library's source goes missing (18% of ripgrep's), so it always gets an
/// absolute path from us.
fn rust_src_path() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("RUST_SRC_PATH")?);
    std::path::absolute(&path).ok()
}

/// The rust-analyzer to run: `$CARGO_ATLAS_RUST_ANALYZER`, or `rust-analyzer` on the PATH.
pub fn binary() -> PathBuf {
    std::env::var_os(BINARY_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rust-analyzer"))
}

/// The version string, e.g. `rust-analyzer 0.3.3057-standalone`, or an error
/// explaining how to install it.
pub fn version() -> Result<String> {
    let output = Command::new(binary())
        .arg("--version")
        .output()
        .with_context(missing_help)?;
    if !output.status.success() {
        bail!("{}", missing_help());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn missing_help() -> String {
    format!(
        "could not run rust-analyzer. Install it with `rustup component add rust-analyzer`, \
         or set {BINARY_ENV} to the path of a rust-analyzer binary"
    )
}

/// rust-analyzer's settings for the index, in the JSON form `--config-path` reads.
///
/// rust-analyzer turns on `cfg(miri)` unless told otherwise: its `cargo.cfgs`
/// setting defaults to `["debug_assertions", "miri"]`. That removes every item
/// marked `#[cfg(not(miri))]`, which in tokio is 40 of its 153 test files. The
/// graph should show a normal build, so `miri` goes and `debug_assertions` stays.
fn config_json(features: &Features) -> serde_json::Value {
    let features = if features.all {
        json!("all")
    } else {
        json!(features.listed)
    };
    json!({ "cargo": { "cfgs": ["debug_assertions"], "features": features } })
}

/// Runs `rust-analyzer scip` on the workspace and writes the index to `out`.
/// This is the slow step: rust-analyzer loads and type-checks the whole workspace.
///
/// rust-analyzer's own progress output is captured, not passed through: when
/// cargo-atlas runs as an MCP server, its stdout carries the protocol and
/// must hold nothing else. The output is shown only if the run fails.
pub fn write_index(workspace_root: &Path, out: &Path, features: &Features) -> Result<()> {
    let config_path = out.with_file_name("rust-analyzer.json");
    std::fs::write(&config_path, config_json(features).to_string())
        .with_context(|| format!("writing {}", config_path.display()))?;
    let mut command = Command::new(binary());
    if let Some(path) = rust_src_path() {
        command.env("RUST_SRC_PATH", path);
    }
    let output = command
        .arg("scip")
        .arg(workspace_root)
        .arg("--output")
        .arg(out)
        .arg("--config-path")
        .arg(&config_path)
        .output()
        .with_context(missing_help)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        // The first line stands alone: the server repeats only that line.
        bail!(
            "rust-analyzer scip failed ({}).\nIts last output:\n{tail}",
            output.status
        );
    }
    Ok(())
}

/// Reads an index file written by [`write_index`].
pub fn read_index(path: &Path) -> Result<Index> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Index::parse_from_bytes(&bytes).with_context(|| format!("decoding {}", path.display()))
}
