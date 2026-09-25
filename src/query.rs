//! Answering questions from `graph.json`: callers, callees, impls, path, explain.
//!
//! Every answer is plain text with `file:line` locations, short enough for an
//! AI assistant to read in one go and then open only the lines it needs.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::model::{Confidence, Edge, EdgeKind, Graph, Node, NodeKind};

/// The graph plus lookup tables for fast queries.
pub struct Atlas {
    graph: Graph,
    by_id: HashMap<String, usize>,
    /// Edge positions in `graph.edges`, by the node they leave / arrive at.
    outgoing: HashMap<String, Vec<usize>>,
    incoming: HashMap<String, Vec<usize>>,
}

/// Show at most this many links per group in `explain`.
const EXPLAIN_LIMIT: usize = 25;

/// Show at most this many CANDIDATE links in `callers` and `callees`.
const CANDIDATE_LIMIT: usize = 8;

impl Atlas {
    pub fn new(graph: Graph) -> Self {
        let by_id = graph
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect();
        let mut outgoing: HashMap<String, Vec<usize>> = HashMap::new();
        let mut incoming: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, e) in graph.edges.iter().enumerate() {
            outgoing.entry(e.from.clone()).or_default().push(i);
            incoming.entry(e.to.clone()).or_default().push(i);
        }
        Atlas {
            graph,
            by_id,
            outgoing,
            incoming,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "no graph at {}; run `cargo atlas build` first",
                path.display()
            )
        })?;
        let graph: Graph = serde_json::from_str(&text).context("reading graph.json")?;
        Ok(Atlas::new(graph))
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    fn node(&self, id: &str) -> &Node {
        &self.graph.nodes[self.by_id[id]]
    }

    fn edges_out(&self, id: &str) -> impl Iterator<Item = &Edge> {
        self.outgoing
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| &self.graph.edges[i])
    }

    fn edges_in(&self, id: &str) -> impl Iterator<Item = &Edge> {
        self.incoming
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| &self.graph.edges[i])
    }

    /// Finds the node a query refers to. A query can be:
    ///
    /// - a location, `src/main.rs:4`: the item defined on that line;
    /// - a name or path, `JsonReader::parse`, optionally with its crate in
    ///   front, `spike::json_reader::JsonReader::parse`;
    /// - the end of a path, `parse`.
    ///
    /// If several nodes match, the error lists them with their locations, and
    /// a location always picks exactly one.
    pub fn find(&self, query: &str) -> Result<&Node> {
        let nodes = &self.graph.nodes;
        let candidates: Vec<&Node> = if let Some((file, line)) = parse_location(query) {
            let found: Vec<&Node> = nodes
                .iter()
                .filter(|n| n.line == Some(line))
                .filter(|n| n.file.as_deref().is_some_and(|f| f.ends_with(file)))
                .collect();
            if found.is_empty() {
                bail!("nothing is defined at `{query}` (use the line of the item's name)");
            }
            found
        } else {
            let exact: Vec<&Node> = nodes
                .iter()
                .filter(|n| n.name == query || n.path == query || qualified(n) == query)
                .collect();
            if exact.is_empty() {
                let suffix = format!("::{query}");
                nodes
                    .iter()
                    .filter(|n| n.name.ends_with(&suffix) || n.path.ends_with(&suffix))
                    .collect()
            } else {
                exact
            }
        };
        match candidates.as_slice() {
            [one] => Ok(*one),
            [] => {
                let lower = query.to_lowercase();
                let mut close: Vec<&str> = nodes
                    .iter()
                    .filter(|n| n.path.to_lowercase().contains(&lower))
                    .map(|n| n.path.as_str())
                    .collect();
                close.sort();
                close.truncate(10);
                if close.is_empty() {
                    bail!("nothing named `{query}` in the graph");
                }
                bail!(
                    "nothing named `{query}`. Close matches:\n  {}",
                    close.join("\n  ")
                );
            }
            many => {
                let mut list: Vec<String> = many
                    .iter()
                    .map(|n| format!("{}  ({})", qualified(n), location(n)))
                    .collect();
                list.sort();
                let example = many
                    .iter()
                    .find_map(|n| Some(format!("{}:{}", n.file.as_ref()?, n.line?)))
                    .unwrap_or_else(|| "src/main.rs:4".to_string());
                bail!(
                    "`{query}` matches {} items. Pick one by its full path or its location \
                     (for example `{example}`):\n  {}",
                    many.len(),
                    list.join("\n  ")
                );
            }
        }
    }

    /// `callers` and `callees` only make sense for things that are called or used by name.
    fn require_callable(&self, node: &Node, command: &str) -> Result<()> {
        let fits = node.kind.is_callable()
            || matches!(
                node.kind,
                NodeKind::Macro | NodeKind::Const | NodeKind::Static
            );
        if !fits {
            bail!(
                "`{}` is a {}; `{command}` works on functions, methods, macros and constants. \
                 `cargo atlas explain {}` lists everything linked to it.",
                node.name,
                kind_name(node.kind),
                node.name
            );
        }
        Ok(())
    }

    /// Who calls this function: direct calls, possible calls through a trait,
    /// and places that name it without calling it.
    pub fn callers(&self, query: &str) -> Result<String> {
        let target = self.find(query)?;
        self.require_callable(target, "callers")?;
        let edges: Vec<&Edge> = self
            .edges_in(&target.id)
            .filter(|e| {
                matches!(
                    e.kind,
                    EdgeKind::Calls | EdgeKind::MayCall | EdgeKind::References
                )
            })
            .collect();
        let rows = edges.iter().map(|e| (self.node(&e.from), *e)).collect();
        Ok(self.link_list(target, "<-", rows, "no callers found"))
    }

    /// What this function calls.
    pub fn callees(&self, query: &str) -> Result<String> {
        let source = self.find(query)?;
        self.require_callable(source, "callees")?;
        let edges: Vec<&Edge> = self
            .edges_out(&source.id)
            .filter(|e| {
                matches!(
                    e.kind,
                    EdgeKind::Calls | EdgeKind::MayCall | EdgeKind::References
                )
            })
            .collect();
        let rows = edges.iter().map(|e| (self.node(&e.to), *e)).collect();
        Ok(self.link_list(source, "->", rows, "no calls found"))
    }

    /// For a trait (or derive): the types that implement it.
    /// For a type: the traits it implements and derives.
    pub fn impls(&self, query: &str) -> Result<String> {
        let node = self.find(query)?;
        let is_impls_edge = |e: &&Edge| matches!(e.kind, EdgeKind::Implements | EdgeKind::Derives);
        let rows: Vec<(&Node, &Edge)> = if node.kind.is_type_like() && node.kind != NodeKind::Trait
        {
            self.edges_out(&node.id)
                .filter(is_impls_edge)
                .map(|e| (self.node(&e.to), e))
                .collect()
        } else {
            self.edges_in(&node.id)
                .filter(is_impls_edge)
                .map(|e| (self.node(&e.from), e))
                .collect()
        };
        let arrow = if node.kind.is_type_like() && node.kind != NodeKind::Trait {
            "->"
        } else {
            "<-"
        };
        Ok(self.link_list(node, arrow, rows, "no impls found"))
    }

    /// The shortest chain of links between two items, in either direction.
    ///
    /// Crates and modules contain everything, so any two items are a few hops
    /// apart through them, and such a path says nothing about how the code
    /// connects. The first search skips links that touch a crate or module;
    /// only if that finds nothing does it try again with every link.
    pub fn path(&self, from: &str, to: &str) -> Result<String> {
        let start = self.find(from)?.id.clone();
        let goal = self.find(to)?.id.clone();
        let is_hub = |id: &str| matches!(self.node(id).kind, NodeKind::Crate | NodeKind::Module);
        let hops = self
            .shortest_path(&start, &goal, |e| !is_hub(&e.from) && !is_hub(&e.to))
            .or_else(|| self.shortest_path(&start, &goal, |_| true));
        let Some(hops) = hops else {
            bail!("no path between `{from}` and `{to}`");
        };

        let mut out = String::new();
        let _ = writeln!(out, "Shortest path: {} link(s)", hops.len());
        let _ = writeln!(out, "  {}", self.node(&start).name);
        let mut current = start;
        for edge_index in hops {
            let e = &self.graph.edges[edge_index];
            let kind = kind_label(e.kind);
            let (arrow, next) = if e.from == current {
                (format!("--{kind}-->"), e.to.clone())
            } else {
                (format!("<--{kind}--"), e.from.clone())
            };
            let _ = writeln!(
                out,
                "    {arrow} {}   {}",
                self.node(&next).name,
                edge_location(e)
            );
            current = next;
        }
        Ok(out)
    }

    /// Breadth-first search over links in both directions. Returns edge positions.
    fn shortest_path(
        &self,
        start: &str,
        goal: &str,
        allowed: impl Fn(&Edge) -> bool,
    ) -> Option<Vec<usize>> {
        let mut came_from: HashMap<&str, (usize, &str)> = HashMap::new();
        let mut queue = VecDeque::from([start]);
        let mut seen = std::collections::HashSet::from([start]);
        while let Some(current) = queue.pop_front() {
            if current == goal {
                let mut hops = Vec::new();
                let mut at = goal;
                while at != start {
                    let (edge_index, previous) = came_from[at];
                    hops.push(edge_index);
                    at = previous;
                }
                hops.reverse();
                return Some(hops);
            }
            let out_links = self
                .outgoing
                .get(current)
                .into_iter()
                .flatten()
                .map(|&i| (i, true));
            let in_links = self
                .incoming
                .get(current)
                .into_iter()
                .flatten()
                .map(|&i| (i, false));
            for (i, is_out) in out_links.chain(in_links) {
                let e = &self.graph.edges[i];
                if !allowed(e) {
                    continue;
                }
                let next = if is_out {
                    e.to.as_str()
                } else {
                    e.from.as_str()
                };
                if seen.insert(next) {
                    came_from.insert(next, (i, current));
                    queue.push_back(next);
                }
            }
        }
        None
    }

    /// Everything about one item: what it is, where, and its links.
    pub fn explain(&self, query: &str) -> Result<String> {
        let node = self.find(query)?;
        let mut out = String::new();
        let _ = writeln!(out, "{}", node.name);
        let _ = writeln!(out, "  kind:       {}", kind_name(node.kind));
        let _ = writeln!(out, "  path:       {}", node.path);
        if !node.krate.is_empty() {
            let _ = writeln!(out, "  crate:      {}", node.krate);
        }
        let _ = writeln!(out, "  defined at: {}", location(node));
        if let Some(signature) = &node.signature {
            let _ = writeln!(out, "  signature:  {signature}");
        }
        for (title, arrow, edges) in [
            (
                "incoming",
                "<-",
                self.edges_in(&node.id).collect::<Vec<_>>(),
            ),
            (
                "outgoing",
                "->",
                self.edges_out(&node.id).collect::<Vec<_>>(),
            ),
        ] {
            let _ = writeln!(out, "\n  {title} ({}):", edges.len());
            let mut rows: Vec<String> = edges
                .iter()
                .map(|e| {
                    let other = if arrow == "<-" { &e.from } else { &e.to };
                    format!(
                        "    {arrow} {:<10} {}   {}   {}",
                        kind_label(e.kind),
                        self.node(other).name,
                        edge_location(e),
                        confidence_label(e.confidence)
                    )
                })
                .collect();
            rows.sort();
            let extra = rows.len().saturating_sub(EXPLAIN_LIMIT);
            for row in rows.into_iter().take(EXPLAIN_LIMIT) {
                let _ = writeln!(out, "{row}");
            }
            if extra > 0 {
                let _ = writeln!(out, "    ... and {extra} more");
            }
        }
        Ok(out)
    }

    /// Formats a header line for `subject` and one aligned row per link.
    fn link_list(
        &self,
        subject: &Node,
        arrow: &str,
        mut rows: Vec<(&Node, &Edge)>,
        empty: &str,
    ) -> String {
        rows.sort_by_key(|(other, e)| {
            (
                e.kind == EdgeKind::MayCall,
                e.confidence,
                e.file.clone(),
                e.lines.first().copied(),
                other.name.clone(),
            )
        });
        let mut out = String::new();
        let _ = writeln!(out, "{}  ({})", subject.name, location(subject));
        if rows.is_empty() {
            let _ = writeln!(out, "  ({empty})");
            return out;
        }
        let name_width = rows.iter().map(|(n, _)| n.name.len()).max().unwrap_or(0);
        let loc_width = rows
            .iter()
            .map(|(_, e)| edge_location(e).len())
            .max()
            .unwrap_or(0);
        // A call to a popular trait method can have a hundred possible impls.
        // Show a few, then say how many more there are.
        let candidates = rows
            .iter()
            .filter(|(_, e)| e.kind == EdgeKind::MayCall)
            .count();
        let mut candidates_shown = 0;
        for (other, e) in rows {
            if e.kind == EdgeKind::MayCall {
                candidates_shown += 1;
                if candidates_shown > CANDIDATE_LIMIT {
                    continue;
                }
            }
            // With `->` the other item is the target of the link; with `<-` the subject is.
            let target = if arrow == "->" { other } else { subject };
            let note = match (e.kind, e.confidence) {
                (EdgeKind::MayCall, _) => "  (through the trait)",
                // rust-analyzer gave several items this symbol; see builder.rs.
                (_, Confidence::Candidate) => "  (best guess among items sharing a symbol)",
                (EdgeKind::References, _) if target.kind.is_callable() => "  (named, not called)",
                (EdgeKind::References, _) => "  (uses the value)",
                (EdgeKind::Derives, _) => "  (derive)",
                _ => "",
            };
            let _ = writeln!(
                out,
                "  {arrow} {:<name_width$}  {:<loc_width$}  {}{note}",
                other.name,
                edge_location(e),
                confidence_label(e.confidence),
            );
        }
        if candidates > CANDIDATE_LIMIT {
            let _ = writeln!(
                out,
                "  ... and {} more possible impls (CANDIDATE); `cargo atlas impls <Trait>` lists them all",
                candidates - CANDIDATE_LIMIT
            );
        }
        out
    }
}

fn location(node: &Node) -> String {
    match (&node.file, node.line) {
        (Some(file), Some(line)) => format!("{file}:{line}"),
        _ => match node.kind {
            NodeKind::Crate => "Cargo package".to_string(),
            NodeKind::Derive => "read from #[derive(...)]".to_string(),
            NodeKind::ExternalTrait => "outside the workspace".to_string(),
            kind => format!("{} with no source location", kind_name(kind)),
        },
    }
}

/// `src/a.rs:12`, `src/a.rs:12,30`, or `src/a.rs:12,30,41 (+5 more)`.
fn edge_location(e: &Edge) -> String {
    const SHOWN: usize = 3;
    let Some(file) = &e.file else {
        return "-".to_string();
    };
    if e.lines.is_empty() {
        return file.clone();
    }
    let shown: Vec<String> = e.lines.iter().take(SHOWN).map(u32::to_string).collect();
    let more = e.lines.len().saturating_sub(SHOWN);
    if more > 0 {
        format!("{file}:{} (+{more} more)", shown.join(","))
    } else {
        format!("{file}:{}", shown.join(","))
    }
}

/// `spike::json_reader::JsonReader::parse`: the path with its crate in front.
fn qualified(node: &Node) -> String {
    if node.krate.is_empty() || node.kind == NodeKind::Crate {
        node.path.clone()
    } else {
        format!("{}::{}", node.krate.replace('-', "_"), node.path)
    }
}

/// Reads `src/main.rs:4` as a file and a line. Anything else is a name.
fn parse_location(query: &str) -> Option<(&str, u32)> {
    let (file, line) = query.rsplit_once(':')?;
    if !file.ends_with(".rs") {
        return None;
    }
    Some((file, line.parse().ok()?))
}

pub fn kind_label(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::References => "references",
        EdgeKind::MayCall => "may_call",
        EdgeKind::Implements => "implements",
        EdgeKind::Derives => "derives",
        EdgeKind::UsesType => "uses_type",
        EdgeKind::Contains => "contains",
        EdgeKind::DependsOn => "depends_on",
    }
}

pub fn confidence_label(c: Confidence) -> &'static str {
    match c {
        Confidence::Exact => "EXACT",
        Confidence::Candidate => "CANDIDATE",
        Confidence::Syntax => "SYNTAX",
    }
}

pub fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Crate => "crate",
        NodeKind::Module => "module",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::Union => "union",
        NodeKind::Trait => "trait",
        NodeKind::TypeAlias => "type alias",
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::TraitMethod => "trait method",
        NodeKind::Const => "const",
        NodeKind::Static => "static",
        NodeKind::Macro => "macro",
        NodeKind::Derive => "derive",
        NodeKind::ExternalTrait => "trait from outside the workspace",
    }
}
