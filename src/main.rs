//! cargo-atlas: a compiler-accurate map of a Rust workspace for AI coding assistants.
//!
//! `cargo atlas build` asks rust-analyzer to resolve every name in the
//! workspace, turns the result into a graph, and saves it as
//! `.atlas/graph.json`. The other commands answer questions from that file.

mod builder;
mod cargo_meta;
mod model;
mod query;
mod report;
mod rust_analyzer;
mod symbols;
mod syntax;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::cargo_meta::Workspace;
use crate::model::EdgeKind;
use crate::query::Atlas;

#[derive(Parser)]
#[command(
    name = "cargo atlas",
    bin_name = "cargo atlas",
    version,
    about = "A compiler-accurate map of a Rust workspace, for AI coding assistants",
    after_help = "An ITEM is a name (`parse`), a path (`JsonReader::parse`, optionally \
                  with the crate first), or a location (`src/json_reader.rs:10`)."
)]
struct Cli {
    /// The workspace to use.
    #[arg(long, global = true, default_value = ".")]
    dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index the workspace with rust-analyzer and write .atlas/graph.json.
    Build,
    /// Who calls a function or method.
    Callers { item: String },
    /// What a function or method calls.
    Callees { item: String },
    /// Types that implement a trait, or the traits a type implements.
    Impls { item: String },
    /// The shortest chain of links between two items.
    Path { from: String, to: String },
    /// Everything about one item: kind, location, signature, links.
    Explain { item: String },
    /// Write .atlas/report.md and print it.
    Report,
}

fn main() -> ExitCode {
    // Cargo runs `cargo atlas build` as `cargo-atlas atlas build`. Drop the extra
    // word so the same parser works for both `cargo atlas` and `cargo-atlas`.
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|a| a == "atlas") {
        args.remove(1);
    }
    match run(Cli::parse_from(args)) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<String> {
    if !cli.dir.is_dir() {
        anyhow::bail!("no such directory: {}", cli.dir.display());
    }
    let workspace = cargo_meta::load(&cli.dir)?;
    let atlas_dir = workspace.root.join(".atlas");
    let graph_path = atlas_dir.join("graph.json");
    let atlas = || Atlas::load(&graph_path);

    match cli.command {
        Command::Build => build(&workspace, &atlas_dir),
        Command::Callers { item } => atlas()?.callers(&item),
        Command::Callees { item } => atlas()?.callees(&item),
        Command::Impls { item } => atlas()?.impls(&item),
        Command::Path { from, to } => atlas()?.path(&from, &to),
        Command::Explain { item } => atlas()?.explain(&item),
        Command::Report => {
            let text = report::markdown(atlas()?.graph());
            std::fs::write(atlas_dir.join("report.md"), &text).context("writing report.md")?;
            Ok(text)
        }
    }
}

fn build(workspace: &Workspace, atlas_dir: &Path) -> Result<String> {
    let started = Instant::now();
    let ra_version = rust_analyzer::version()?;
    std::fs::create_dir_all(atlas_dir).context("creating .atlas")?;
    // A `*` .gitignore inside keeps the whole folder out of git without touching the repo's own.
    std::fs::write(atlas_dir.join(".gitignore"), "*\n").context("writing .atlas/.gitignore")?;

    let std_sources = rust_analyzer::std_sources_available(&workspace.root);
    if !std_sources {
        eprintln!(
            "warning: the standard library's source is missing, so calls inside std macros \
             such as println! will be missing. Fix: rustup component add rust-src"
        );
    }

    eprintln!(
        "Indexing {} with {ra_version} ...",
        workspace.root.display()
    );
    let index_path = atlas_dir.join("index.scip");
    rust_analyzer::write_index(&workspace.root, &index_path)?;
    let index = rust_analyzer::read_index(&index_path)?;

    let produced_by = format!("cargo-atlas {}; {ra_version}", env!("CARGO_PKG_VERSION"));
    let mut graph = builder::build_graph(&index, workspace, produced_by);
    graph.stats.std_sources_found = std_sources;
    let json = serde_json::to_string_pretty(&graph).context("serializing the graph")?;
    std::fs::write(atlas_dir.join("graph.json"), json).context("writing graph.json")?;

    let calls = graph
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .count();
    Ok(format!(
        "Wrote .atlas/graph.json: {} items, {} links ({calls} calls) in {:.1}s\n",
        graph.nodes.len(),
        graph.edges.len(),
        started.elapsed().as_secs_f64()
    ))
}
