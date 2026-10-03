//! cargo-atlas: a compiler-accurate map of a Rust workspace for AI coding assistants.
//!
//! `cargo atlas build` asks rust-analyzer to resolve every name in the
//! workspace, turns the result into a graph, and saves it as
//! `.atlas/graph.json`. The other commands answer questions from that file,
//! and `cargo atlas serve` answers the same questions as MCP tools.

mod builder;
mod cargo_meta;
mod freshness;
mod model;
mod pipeline;
mod query;
mod report;
mod rust_analyzer;
mod server;
mod symbols;
mod syntax;

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::model::Features;
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
    dir: std::path::PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Index the workspace with rust-analyzer and write .atlas/graph.json.
    Build {
        #[command(flatten)]
        features: FeatureArgs,
    },
    // The questions, answered from .atlas/graph.json.
    #[command(flatten)]
    Query(Query),
    /// Answer the same questions as MCP tools, for AI assistants.
    ///
    /// Speaks MCP on stdin and stdout, answering from .atlas/graph.json and
    /// rebuilding it when files change. To use it from Claude Code:
    ///
    ///     claude mcp add --transport stdio cargo-atlas -- cargo-atlas serve
    ///
    /// Rebuilds keep the features of the graph they replace, unless
    /// --features or --all-features is given here.
    Serve {
        #[command(flatten)]
        features: FeatureArgs,
    },
}

/// Which Cargo features rust-analyzer turns on. Without either flag, each
/// package's default features, as with a plain `cargo build`.
#[derive(clap::Args)]
struct FeatureArgs {
    /// Turn on every Cargo feature, like `cargo build --all-features`.
    #[arg(long, conflicts_with = "features")]
    all_features: bool,
    /// Turn on these Cargo features as well as the default ones (comma-separated).
    #[arg(long, value_delimiter = ',', value_name = "FEATURES")]
    features: Vec<String>,
}

impl FeatureArgs {
    /// `None` when neither flag was given.
    fn chosen(&self) -> Option<Features> {
        (self.all_features || !self.features.is_empty()).then(|| Features {
            all: self.all_features,
            listed: self.features.clone(),
        })
    }
}

#[derive(Subcommand)]
enum Query {
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
    /// Items whose name or path contains TEXT.
    Search {
        text: String,
        /// Only one kind of item: function, method, struct, trait, ...
        #[arg(long)]
        kind: Option<String>,
    },
    /// The tests that reach a function, and the command that runs them.
    Tests { item: String },
    /// Unsafe code, and whether each piece has its SAFETY comment.
    ///
    /// With an ITEM, only the unsafe code inside it, and for a function also
    /// the unsafe code its calls reach.
    Unsafe {
        /// A crate, module, type or function.
        item: Option<String>,
        /// Only the sites without their SAFETY comment or # Safety section.
        #[arg(long)]
        missing: bool,
    },
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
        bail!("no such directory: {}", cli.dir.display());
    }
    match cli.command {
        Command::Build { features } => build(&cli.dir, &features.chosen().unwrap_or_default()),
        Command::Query(query) => answer(&cli.dir, query),
        Command::Serve { features } => {
            server::run(&cli.dir, features.chosen())?;
            Ok(String::new())
        }
    }
}

fn build(dir: &Path, features: &Features) -> Result<String> {
    let workspace = cargo_meta::load(dir)?;
    eprintln!(
        "Indexing {} with {}, {} ...",
        workspace.root.display(),
        rust_analyzer::version()?,
        features.describe()
    );
    let built = pipeline::run(&workspace, features)?;
    for warning in &built.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(format!("{}\n", built.summary()))
}

fn answer(dir: &Path, query: Query) -> Result<String> {
    let workspace = cargo_meta::load(dir)?;
    let atlas = Atlas::load(&pipeline::graph_path(&workspace))?;
    // The answer comes from the last build; say so if the code moved on since.
    let changed = freshness::changed(&workspace.root, &atlas.graph().files);
    if !changed.is_empty() {
        eprintln!(
            "note: {} since the last build. Run `cargo atlas build` to update the graph.",
            freshness::describe(&changed)
        );
    }
    match query {
        Query::Callers { item } => atlas.callers(&item),
        Query::Callees { item } => atlas.callees(&item),
        Query::Impls { item } => atlas.impls(&item),
        Query::Path { from, to } => atlas.path(&from, &to),
        Query::Explain { item } => atlas.explain(&item),
        Query::Search { text, kind } => atlas.search(&text, kind.as_deref()),
        Query::Tests { item } => atlas.tests(&item),
        Query::Unsafe { item, missing } => atlas.unsafe_code(item.as_deref(), missing),
        Query::Report => {
            let text = report::markdown(atlas.graph());
            let path = workspace.root.join(".atlas").join("report.md");
            std::fs::write(path, &text).context("writing report.md")?;
            Ok(text)
        }
    }
}
