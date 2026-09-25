//! The graph's data model: nodes, edges, and how sure we are about each edge.
//!
//! Everything here is plain data. `graph.json` is exactly a serialized [`Graph`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Bump this when the shape of `graph.json` changes.
pub const FORMAT_VERSION: u32 = 2;

/// What kind of Rust item a node stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Crate,
    Module,
    Struct,
    Enum,
    Union,
    Trait,
    TypeAlias,
    Function,
    Method,
    TraitMethod,
    Const,
    Static,
    Macro,
    /// A derive name such as `Clone`, read from `#[derive(...)]`.
    Derive,
    /// A trait from outside the workspace (for example `std::fmt::Display`),
    /// known only by name because the index has no entry for it.
    ExternalTrait,
}

impl NodeKind {
    /// Functions, methods and trait methods: things that can be called.
    pub fn is_callable(self) -> bool {
        matches!(self, Self::Function | Self::Method | Self::TraitMethod)
    }

    /// Types and traits: things that appear in signatures and impl headers.
    pub fn is_type_like(self) -> bool {
        matches!(
            self,
            Self::Struct | Self::Enum | Self::Union | Self::Trait | Self::TypeAlias
        )
    }
}

/// What a link between two nodes means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// `from` calls `to`.
    Calls,
    /// `from` names the function `to` without calling it, e.g. `map(Self::parse)`.
    References,
    /// `from` calls a trait method, and `to` is one of the impls that may run.
    MayCall,
    /// Type `from` implements trait `to`.
    Implements,
    /// Type `from` has `#[derive(to)]`.
    Derives,
    /// `from` mentions type `to`, e.g. in its signature or body.
    UsesType,
    /// `from` contains `to`: crate > module > type > method.
    Contains,
    /// Crate `from` depends on crate `to`.
    DependsOn,
}

/// How an edge was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Confidence {
    /// Resolved by rust-analyzer or Cargo.
    Exact,
    /// One of several possible targets, as with a `dyn Trait` call.
    Candidate,
    /// Read from the source text but not type-checked.
    Syntax,
}

/// One item in the project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Stable id: rust-analyzer's symbol string, or `crate:NAME`, `derive:NAME`, `trait:NAME`.
    pub id: String,
    /// Short name, e.g. `JsonReader::parse` or `<CsvReader as Loader>::load`.
    pub name: String,
    /// Module path plus name, e.g. `json_reader::JsonReader::parse`.
    pub path: String,
    pub kind: NodeKind,
    /// The Cargo package the item belongs to.
    #[serde(rename = "crate")]
    pub krate: String,
    /// File relative to the workspace root.
    pub file: Option<String>,
    /// 1-based line of the item's name.
    pub line: Option<u32>,
    /// The item's signature as rust-analyzer prints it, e.g. `pub fn parse(&self) -> Vec<String>`.
    pub signature: Option<String>,
}

/// One link between two nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub confidence: Confidence,
    /// The file the link was seen in. A link always starts at one item, so one file.
    pub file: Option<String>,
    /// Every 1-based line the link was seen on, sorted: each call site of a call.
    pub lines: Vec<u32>,
}

/// Numbers worth reporting about a build.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    /// Whether rust-analyzer could see the standard library's source.
    /// Without it, calls inside std macros like `println!` are missing.
    pub std_sources_found: bool,
    pub files_indexed: usize,
    pub files_unparsed_by_syn: usize,
    /// References to items outside the workspace (std, dependencies). Not in the graph.
    pub external_references: usize,
    /// References that sit outside any function or type, e.g. in `use` lines.
    pub unattributed_references: usize,
    /// Symbols rust-analyzer gave to more than one item, e.g. `main` in both
    /// `build.rs` and `src/main.rs`. Each item still gets its own node.
    pub duplicate_symbols: usize,
}

/// The whole map, as saved to `graph.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    pub format_version: u32,
    /// Which tools produced it, e.g. `cargo-atlas 0.1.0; rust-analyzer 0.3.3057`.
    pub produced_by: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub stats: Stats,
}

/// Collects nodes and edges while building, merging duplicates.
///
/// Adding the same node twice keeps the first one. Adding the same edge
/// (same `from`, `to` and `kind`) twice merges them: the lines are combined
/// and the stronger confidence is kept.
#[derive(Default)]
pub struct GraphBuilder {
    nodes: Vec<Node>,
    node_index: HashMap<String, usize>,
    edges: Vec<Edge>,
    edge_index: HashMap<(String, String, EdgeKind), usize>,
    pub stats: Stats,
}

impl GraphBuilder {
    pub fn add_node(&mut self, node: Node) {
        if !self.node_index.contains_key(&node.id) {
            self.node_index.insert(node.id.clone(), self.nodes.len());
            self.nodes.push(node);
        }
    }

    pub fn has_node(&self, id: &str) -> bool {
        self.node_index.contains_key(id)
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.node_index.get(id).map(|&i| &self.nodes[i])
    }

    /// Adds an edge, or merges it into an identical one. Both ends must already exist.
    pub fn add_edge(&mut self, edge: Edge) {
        debug_assert!(self.has_node(&edge.from) && self.has_node(&edge.to));
        let key = (edge.from.clone(), edge.to.clone(), edge.kind);
        match self.edge_index.get(&key) {
            Some(&i) => {
                let existing = &mut self.edges[i];
                existing.lines.extend(edge.lines);
                // EXACT < CANDIDATE < SYNTAX, so the smaller one is the stronger claim.
                existing.confidence = existing.confidence.min(edge.confidence);
            }
            None => {
                self.edge_index.insert(key, self.edges.len());
                self.edges.push(edge);
            }
        }
    }

    /// Finishes the build: sorts everything so `graph.json` is stable between runs.
    pub fn finish(mut self, produced_by: String) -> Graph {
        self.nodes.sort_by(|a, b| a.id.cmp(&b.id));
        for e in &mut self.edges {
            e.lines.sort_unstable();
            e.lines.dedup();
        }
        self.edges
            .sort_by(|a, b| (&a.from, &a.to, a.kind).cmp(&(&b.from, &b.to, b.kind)));
        Graph {
            format_version: FORMAT_VERSION,
            produced_by,
            nodes: self.nodes,
            edges: self.edges,
            stats: self.stats,
        }
    }
}

/// A short constructor for edges, used all over the builder.
pub fn edge(
    from: &str,
    to: &str,
    kind: EdgeKind,
    confidence: Confidence,
    file: Option<&str>,
    line: Option<u32>,
) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        kind,
        confidence,
        file: file.map(str::to_string),
        lines: line.into_iter().collect(),
    }
}
