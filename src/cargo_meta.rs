//! Reading the workspace layout from `cargo metadata`.
//!
//! We only need a little of Cargo's output: which packages the workspace has,
//! and which crates each one depends on.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// The parts of a workspace the graph uses.
#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub packages: Vec<Package>,
}

/// One package (crate) in the workspace.
#[derive(Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub dependencies: Vec<Dependency>,
}

/// One entry from a package's `[dependencies]`, `[dev-dependencies]` or `[build-dependencies]`.
#[derive(Debug, Deserialize)]
pub struct Dependency {
    pub name: String,
    /// `None` for normal dependencies, `Some("dev")` or `Some("build")` otherwise.
    pub kind: Option<String>,
}

/// The JSON shape of `cargo metadata --no-deps`, trimmed to what we read.
#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_root: PathBuf,
}

/// Runs `cargo metadata` in `dir`. `--no-deps` keeps it fast and offline:
/// it lists the workspace's own packages without resolving the dependency tree.
pub fn load(dir: &Path) -> Result<Workspace> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(dir)
        .output()
        .context("running cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed in {}:\n{}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let meta: Metadata =
        serde_json::from_slice(&output.stdout).context("reading cargo metadata output")?;
    Ok(Workspace {
        root: meta.workspace_root,
        packages: meta.packages,
    })
}
