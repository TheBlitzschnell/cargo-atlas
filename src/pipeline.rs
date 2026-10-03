//! The whole `build` step: run rust-analyzer, turn its index into a graph,
//! stamp the files it read, and save `.atlas/graph.json`.
//!
//! The command line and the MCP server both build through [`run`].

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result};

use crate::cargo_meta::Workspace;
use crate::model::{EdgeKind, Features, Graph};
use crate::{builder, freshness, rust_analyzer};

/// Where a workspace's graph is saved.
pub fn graph_path(workspace: &Workspace) -> PathBuf {
    workspace.root.join(".atlas").join("graph.json")
}

/// A finished build.
pub struct Built {
    pub graph: Graph,
    pub seconds: f64,
    /// Problems the user should hear about, such as a missing `rust-src`.
    pub warnings: Vec<String>,
}

impl Built {
    /// `Wrote .atlas/graph.json: 51 items (5 tests), 120 links (30 calls), 12 unsafe sites
    /// in 8.1s`, plus the features when they aren't the default ones.
    pub fn summary(&self) -> String {
        let g = &self.graph;
        let tests = g.nodes.iter().filter(|n| n.test).count();
        let calls = g.edges.iter().filter(|e| e.kind == EdgeKind::Calls).count();
        let features = if g.features == Features::default() {
            String::new()
        } else {
            format!(" with {}", g.features.describe())
        };
        format!(
            "Wrote .atlas/graph.json: {} items ({tests} tests), {} links ({calls} calls), \
             {} unsafe sites in {:.1}s{features}",
            g.nodes.len(),
            g.edges.len(),
            g.unsafe_sites.len(),
            self.seconds
        )
    }
}

/// Indexes the workspace with the given Cargo features and writes `.atlas/graph.json`.
pub fn run(workspace: &Workspace, features: &Features) -> Result<Built> {
    let started = Instant::now();
    let started_at = SystemTime::now();
    let ra_version = rust_analyzer::version()?;
    let atlas_dir = workspace.root.join(".atlas");
    std::fs::create_dir_all(&atlas_dir).context("creating .atlas")?;
    // A `*` .gitignore inside keeps the whole folder out of git without touching the repo's own.
    std::fs::write(atlas_dir.join(".gitignore"), "*\n").context("writing .atlas/.gitignore")?;

    let mut warnings = Vec::new();
    let std_sources = rust_analyzer::std_sources_available(&workspace.root);
    if !std_sources {
        warnings.push(
            "the standard library's source is missing, so calls inside std macros such as \
             println! will be missing. Fix: rustup component add rust-src"
                .to_string(),
        );
    }

    let index_path = atlas_dir.join("index.scip");
    rust_analyzer::write_index(&workspace.root, &index_path, features)?;
    let index = rust_analyzer::read_index(&index_path)?;

    let produced_by = format!("cargo-atlas {}; {ra_version}", env!("CARGO_PKG_VERSION"));
    let mut graph = builder::build_graph(&index, workspace, produced_by);
    graph.stats.std_sources_found = std_sources;
    graph.features = features.clone();
    let mut files: Vec<String> = index
        .documents
        .iter()
        .map(|d| d.relative_path.clone())
        .collect();
    files.extend(manifests(workspace));
    graph.files = freshness::stamp(&workspace.root, &files, started_at);

    save(&graph, &graph_path(workspace))?;
    Ok(Built {
        graph,
        seconds: started.elapsed().as_secs_f64(),
        warnings,
    })
}

/// Every package's `Cargo.toml` and the workspace's own, relative to the root.
/// A new dependency or target shows up there first.
///
/// `Cargo.lock` is left out: `cargo update` changes it without changing
/// anything in the graph, and Cargo may rewrite it during the build itself.
fn manifests(workspace: &Workspace) -> Vec<String> {
    let mut paths: Vec<PathBuf> = workspace
        .packages
        .iter()
        .map(|p| p.manifest_path.clone())
        .collect();
    paths.push(workspace.root.join("Cargo.toml"));
    paths
        .iter()
        .filter_map(|p| p.strip_prefix(&workspace.root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect()
}

/// Writes a temporary file, then renames it over the old graph. Anything
/// reading graph.json meanwhile sees the old graph or the new one, never half.
fn save(graph: &Graph, path: &Path) -> Result<()> {
    let json = serde_json::to_string_pretty(graph).context("serializing the graph")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).context("writing graph.json")?;
    std::fs::rename(&tmp, path).context("replacing graph.json")?;
    Ok(())
}
