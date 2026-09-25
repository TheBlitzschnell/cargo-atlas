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

/// Environment variable that points at a specific rust-analyzer binary.
pub const BINARY_ENV: &str = "CARGO_ATLAS_RUST_ANALYZER";

/// True when the standard library's source code is where rust-analyzer looks.
///
/// Without it, rust-analyzer can't expand std macros such as `println!` or
/// work out std types such as iterators, so calls hidden there go missing.
/// It comes from `rustup component add rust-src`, or from `$RUST_SRC_PATH`.
pub fn std_sources_available(workspace_root: &Path) -> bool {
    if let Some(path) = std::env::var_os("RUST_SRC_PATH") {
        return Path::new(&path).join("core").is_dir();
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

/// Runs `rust-analyzer scip` on the workspace and writes the index to `out`.
/// This is the slow step: rust-analyzer loads and type-checks the whole workspace.
pub fn write_index(workspace_root: &Path, out: &Path) -> Result<()> {
    let status = Command::new(binary())
        .arg("scip")
        .arg(workspace_root)
        .arg("--output")
        .arg(out)
        .status()
        .with_context(missing_help)?;
    if !status.success() {
        bail!("rust-analyzer scip failed with {status}");
    }
    Ok(())
}

/// Reads an index file written by [`write_index`].
pub fn read_index(path: &Path) -> Result<Index> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Index::parse_from_bytes(&bytes).with_context(|| format!("decoding {}", path.display()))
}
