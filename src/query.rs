//! Answering questions from `graph.json`: callers, callees, impls, path,
//! explain, search, tests and unsafe code.
//!
//! Every answer is plain text with `file:line` locations, short enough for an
//! AI assistant to read in one go and then open only the lines it needs.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::model::{
    Confidence, Edge, EdgeKind, FORMAT_VERSION, Graph, Node, NodeKind, UnsafeKind, UnsafeSite,
};

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

/// Show at most this many rows in `search`, `tests` and `unsafe`.
const LIST_LIMIT: usize = 50;

/// A name that matches several items. The message lists them.
///
/// The command line prints it as an error. The MCP server returns it as an
/// ordinary answer, since picking one from the list is the expected next step;
/// it tells this error apart with `error.downcast_ref::<Ambiguous>()`.
#[derive(Debug)]
pub struct Ambiguous(String);

impl std::fmt::Display for Ambiguous {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Ambiguous {}

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
        if graph.format_version != FORMAT_VERSION {
            bail!(
                "{} is in an older format ({} instead of {FORMAT_VERSION}); \
                 run `cargo atlas build` to rebuild it",
                path.display(),
                graph.format_version
            );
        }
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
    /// Names and paths may leave out generic arguments (`GlobBuilder::build`
    /// for `GlobBuilder<'a>::build`), and a method in a trait impl may be
    /// written `Square::area` for `<Square as Area>::area`.
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
            // Written exactly as the graph writes it, first.
            let exact: Vec<&Node> = nodes
                .iter()
                .filter(|n| n.name == query || n.path == query || qualified(n) == query)
                .collect();
            let short = strip_generics(query);
            let suffix = format!("::{short}");
            if !exact.is_empty() {
                exact
            } else {
                // Then the way people write it: `GlobBuilder::build` for
                // `GlobBuilder<'a>::build`, `Square::area` for `<Square as Area>::area`.
                let same: Vec<&Node> = nodes
                    .iter()
                    .filter(|n| short_forms(n).contains(&short))
                    .collect();
                if same.is_empty() {
                    nodes
                        .iter()
                        .filter(|n| short_forms(n).iter().any(|f| f.ends_with(&suffix)))
                        .collect()
                } else {
                    same
                }
            }
        };
        match candidates.as_slice() {
            [one] => Ok(*one),
            [] => {
                // A path through a re-export, such as `tokio::task::spawn_blocking`,
                // isn't where the item is defined; offer the items with that name.
                let short = strip_generics(query);
                let last = short.rsplit("::").next().unwrap_or(&short);
                if short.contains("::") {
                    let mut same_name: Vec<String> = nodes
                        .iter()
                        .filter(|n| strip_generics(&n.path).rsplit("::").next() == Some(last))
                        .map(|n| format!("{}  ({})", qualified(n), location(n)))
                        .collect();
                    same_name.sort();
                    same_name.truncate(10);
                    if !same_name.is_empty() {
                        bail!(
                            "nothing is defined at the path `{query}` (it may be a re-export). \
                             Items named `{last}`:\n  {}",
                            same_name.join("\n  ")
                        );
                    }
                }
                let lower = short.to_lowercase();
                let mut close: Vec<&str> = nodes
                    .iter()
                    .filter(|n| strip_generics(&n.path).to_lowercase().contains(&lower))
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
                Err(Ambiguous(format!(
                    "`{query}` matches {} items. Pick one by its full path or its location \
                     (for example `{example}`):\n  {}",
                    many.len(),
                    list.join("\n  ")
                ))
                .into())
            }
        }
    }

    /// `callers` and `callees` only make sense for things that are called or used by name.
    fn require_callable(&self, node: &Node, command: &str) -> Result<()> {
        if !is_used_by_name(node.kind) {
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
        if node.test {
            let _ = writeln!(out, "  test:       yes");
        }
        let sites: Vec<&UnsafeSite> = self
            .graph
            .unsafe_sites
            .iter()
            .filter(|s| s.item.as_deref() == Some(node.id.as_str()))
            .collect();
        if !sites.is_empty() {
            let _ = writeln!(out, "\n  unsafe code ({}):", sites.len());
            for row in self.unsafe_rows(&sites, None) {
                let _ = writeln!(out, "    {row}");
            }
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

    /// Items whose name or path contains `text`, ignoring case, closest first.
    /// `kind` keeps one kind of item, named the way `explain` names it:
    /// `function`, `method`, `struct`, `trait` and so on.
    pub fn search(&self, text: &str, kind: Option<&str>) -> Result<String> {
        let kind_known = kind.is_none_or(|k| NodeKind::ALL.iter().any(|n| kind_name(*n) == k));
        if !kind_known {
            let kinds: Vec<&str> = NodeKind::ALL.iter().map(|k| kind_name(*k)).collect();
            bail!(
                "unknown kind `{}`. Kinds: {}",
                kind.unwrap_or_default(),
                kinds.join(", ")
            );
        }
        let needle = text.to_lowercase();
        // (closeness, full path, node): 0 for the exact name, 1 for a name
        // that starts with the text, 2 for the text anywhere in the path.
        let mut found: Vec<(u8, String, &Node)> = self
            .graph
            .nodes
            .iter()
            .filter(|n| kind.is_none_or(|k| kind_name(n.kind) == k))
            .filter_map(|n| {
                let full = qualified(n);
                let own_name = n.path.rsplit("::").next().unwrap_or(&n.path).to_lowercase();
                let closeness = if own_name == needle {
                    0
                } else if own_name.starts_with(&needle) {
                    1
                } else if full.to_lowercase().contains(&needle)
                    || n.name.to_lowercase().contains(&needle)
                {
                    2
                } else {
                    return None;
                };
                Some((closeness, full, n))
            })
            .collect();
        found.sort_by(|a, b| (a.0, a.1.len(), &a.1).cmp(&(b.0, b.1.len(), &b.1)));

        let mut out = String::new();
        match found.len() {
            0 => {
                let _ = writeln!(out, "Nothing matches `{text}`.");
                return Ok(out);
            }
            1 => {
                let _ = writeln!(out, "1 item matches `{text}`:");
            }
            n => {
                let _ = writeln!(out, "{n} items match `{text}`:");
            }
        }
        let shown = &found[..found.len().min(LIST_LIMIT)];
        let path_width = shown.iter().map(|(_, p, _)| p.len()).max().unwrap_or(0);
        let kind_width = shown
            .iter()
            .map(|(_, _, n)| kind_name(n.kind).len())
            .max()
            .unwrap_or(0);
        for (_, full, node) in shown {
            let _ = writeln!(
                out,
                "  {full:<path_width$}  {:<kind_width$}  {}",
                kind_name(node.kind),
                location(node)
            );
        }
        if found.len() > LIST_LIMIT {
            let _ = writeln!(
                out,
                "  ... and {} more. Search for more of the name, or for one kind of item.",
                found.len() - LIST_LIMIT
            );
        }
        Ok(out)
    }

    /// The tests that reach an item through calls, and a `cargo test` command
    /// that runs only them.
    ///
    /// Walks `calls`, `may_call` and `references` links backwards from the
    /// item, as far as they go. For a type it starts from the type's methods;
    /// for a trait, from the trait's methods and every impl of them.
    pub fn tests(&self, query: &str) -> Result<String> {
        let target = self.find(query)?;
        let starts: Vec<&str> = if target.kind.is_type_like() {
            let methods: Vec<&str> = self
                .edges_out(&target.id)
                .filter(|e| e.kind == EdgeKind::Contains)
                .map(|e| e.to.as_str())
                .filter(|id| self.node(id).kind.is_callable())
                .collect();
            let impls = methods.iter().flat_map(|m| {
                self.edges_in(m)
                    .filter(|e| e.kind == EdgeKind::Implements)
                    .map(|e| e.from.as_str())
            });
            let mut starts: Vec<&str> = methods.iter().copied().chain(impls).collect();
            starts.sort_unstable();
            starts.dedup();
            starts
        } else if is_used_by_name(target.kind) {
            vec![target.id.as_str()]
        } else {
            bail!(
                "`{}` is a {}; `tests` works on functions, methods, macros, constants, types and traits",
                target.name,
                kind_name(target.kind)
            );
        };
        let walk = self.walk(
            &starts,
            Direction::Backward,
            &[EdgeKind::Calls, EdgeKind::MayCall, EdgeKind::References],
        );
        let mut tests: Vec<&Node> = walk
            .keys()
            .map(|id| self.node(id))
            .filter(|n| n.test && n.id != target.id)
            .collect();
        tests.sort_by_key(|n| (n.file.clone(), n.line, n.id.clone()));

        let mut out = String::new();
        let _ = writeln!(
            out,
            "Tests that reach {}  ({})",
            target.name,
            location(target)
        );
        if target.test {
            let _ = writeln!(out, "  ({} is a test itself.)", target.name);
        }
        if tests.is_empty() {
            let _ = writeln!(out, "  (no tests reach it)");
            return Ok(out);
        }
        let shown = &tests[..tests.len().min(LIST_LIMIT)];
        let name_width = shown.iter().map(|n| n.name.len()).max().unwrap_or(0);
        let loc_width = shown.iter().map(|n| location(n).len()).max().unwrap_or(0);
        for test in shown {
            let steps = self.steps(&walk, &test.id, Direction::Backward);
            // The last step is the target itself unless the walk started from methods.
            let names: Vec<&str> = steps
                .ids
                .iter()
                .filter(|id| **id != target.id)
                .map(|id| self.node(id).name.as_str())
                .collect();
            let how = if names.is_empty() {
                "direct".to_string()
            } else {
                format!("via {}", names.join(" -> "))
            };
            let _ = writeln!(
                out,
                "  {:<name_width$}  {:<loc_width$}  {how}{}",
                test.name,
                location(test),
                steps.notes()
            );
        }
        if tests.len() > LIST_LIMIT {
            let _ = writeln!(out, "  ... and {} more", tests.len() - LIST_LIMIT);
        }

        // One command per package. libtest runs the tests whose names contain
        // any of the filters after `--`, and a test's name is its module path.
        let mut by_package: HashMap<&str, Vec<&str>> = HashMap::new();
        for test in &tests {
            by_package
                .entry(test.krate.as_str())
                .or_default()
                .push(test.path.as_str());
        }
        let mut packages: Vec<(&str, Vec<&str>)> = by_package.into_iter().collect();
        packages.sort();
        let _ = writeln!(out, "\nRun them:");
        for (package, mut names) in packages {
            names.sort();
            names.dedup();
            if names.len() > LIST_LIMIT {
                let _ = writeln!(out, "  cargo test -p {package}");
            } else {
                let _ = writeln!(out, "  cargo test -p {package} -- {}", names.join(" "));
            }
        }
        Ok(out)
    }

    /// Unsafe code, by file and line. With an item: the unsafe code inside it,
    /// and for a function also the unsafe code its calls can reach. With
    /// `missing_only`: only the sites without their SAFETY comment or
    /// `# Safety` section.
    pub fn unsafe_code(&self, query: Option<&str>, missing_only: bool) -> Result<String> {
        let sites: Vec<&UnsafeSite> = self
            .graph
            .unsafe_sites
            .iter()
            .filter(|s| !(missing_only && s.documented))
            .collect();
        let mut out = String::new();
        let Some(query) = query else {
            let _ = writeln!(out, "Unsafe code in the workspace: {}", tally(&sites));
            self.write_unsafe_rows(&mut out, &sites, None);
            return Ok(out);
        };

        let node = self.find(query)?;
        let inside = self.contained_in(&node.id);
        let own: Vec<&UnsafeSite> = sites
            .iter()
            .copied()
            .filter(|s| s.item.as_deref().is_some_and(|id| inside.contains(id)))
            .collect();
        let _ = writeln!(
            out,
            "Unsafe code in {}  ({}): {}",
            node.name,
            location(node),
            tally(&own)
        );
        self.write_unsafe_rows(&mut out, &own, None);

        if node.kind.is_callable() {
            let start = [node.id.as_str()];
            let walk = self.walk(
                &start,
                Direction::Forward,
                &[EdgeKind::Calls, EdgeKind::MayCall],
            );
            let reached: Vec<&UnsafeSite> = sites
                .iter()
                .copied()
                .filter(|s| {
                    s.item
                        .as_deref()
                        .is_some_and(|id| !inside.contains(id) && walk.contains_key(id))
                })
                .collect();
            let _ = writeln!(out, "\nReached through its calls: {}", tally(&reached));
            self.write_unsafe_rows(&mut out, &reached, Some(&walk));
        }
        Ok(out)
    }

    /// One aligned row per site, at most [`LIST_LIMIT`]. With a forward walk,
    /// each row also says how the walk's start reaches the site.
    fn write_unsafe_rows<'a>(
        &'a self,
        out: &mut String,
        sites: &[&'a UnsafeSite],
        walk: Option<&Walk<'a>>,
    ) {
        let shown = &sites[..sites.len().min(LIST_LIMIT)];
        let mut rows = self.unsafe_rows(shown, walk);
        if rows.is_empty() {
            rows.push("(none)".to_string());
        }
        for row in rows {
            let _ = writeln!(out, "  {row}");
        }
        if sites.len() > LIST_LIMIT {
            let _ = writeln!(
                out,
                "  ... and {} more. Pass a crate, module, type or function to see part of the list.",
                sites.len() - LIST_LIMIT
            );
        }
    }

    /// `edge-core/src/raw.rs:10  block  in first_unchecked  SAFETY comment`
    fn unsafe_rows<'a>(&'a self, sites: &[&'a UnsafeSite], walk: Option<&Walk<'a>>) -> Vec<String> {
        let cells: Vec<[String; 5]> = sites
            .iter()
            .map(|site| {
                let owner = site.item.as_deref().map(|id| self.node(id).name.as_str());
                let what = match (site.kind, owner) {
                    (UnsafeKind::Block, Some(name)) => format!("in {name}"),
                    (UnsafeKind::Impl, Some(name)) => match &site.trait_name {
                        Some(t) => format!("{t} for {name}"),
                        None => name.to_string(),
                    },
                    (_, Some(name)) => name.to_string(),
                    // Usually code for another platform or feature, which
                    // syn reads but rust-analyzer didn't index.
                    (_, None) => "(not in the index; cfg'd out?)".to_string(),
                };
                let comment = match (site.kind, site.documented) {
                    _ if site.in_trait_impl => "contract on the trait",
                    (UnsafeKind::Block | UnsafeKind::Impl, true) => "SAFETY comment",
                    (UnsafeKind::Block | UnsafeKind::Impl, false) => "no SAFETY comment",
                    (UnsafeKind::Fn | UnsafeKind::Trait, true) => "safety documented",
                    (UnsafeKind::Fn | UnsafeKind::Trait, false) => "no # Safety section",
                };
                let how = match (walk, &site.item) {
                    (Some(walk), Some(owner)) => {
                        let steps = self.steps(walk, owner, Direction::Forward);
                        // The last step is the owner itself, already on the row.
                        let names: Vec<&str> = steps.ids[..steps.ids.len().saturating_sub(1)]
                            .iter()
                            .map(|id| self.node(id).name.as_str())
                            .collect();
                        let via = if names.is_empty() {
                            String::new()
                        } else {
                            format!("via {}", names.join(" -> "))
                        };
                        format!("{via}{}", steps.notes()).trim().to_string()
                    }
                    _ => String::new(),
                };
                [
                    format!("{}:{}", site.file, site.line),
                    unsafe_kind_name(site.kind).to_string(),
                    what,
                    comment.to_string(),
                    how,
                ]
            })
            .collect();
        let width = |i: usize| cells.iter().map(|c| c[i].len()).max().unwrap_or(0);
        let widths = [width(0), width(1), width(2), width(3)];
        cells
            .into_iter()
            .map(|c| {
                let row = format!(
                    "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {}",
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    w0 = widths[0],
                    w1 = widths[1],
                    w2 = widths[2],
                    w3 = widths[3],
                );
                row.trim_end().to_string()
            })
            .collect()
    }

    /// An item and everything inside it, through `contains` links:
    /// crate > module > type > method.
    fn contained_in<'a>(&'a self, id: &'a str) -> HashSet<&'a str> {
        let mut inside = HashSet::from([id]);
        let mut queue = VecDeque::from([id]);
        while let Some(current) = queue.pop_front() {
            for e in self.edges_out(current) {
                if e.kind == EdgeKind::Contains && inside.insert(e.to.as_str()) {
                    queue.push_back(e.to.as_str());
                }
            }
        }
        inside
    }

    /// Breadth-first search from `starts` along links of the given kinds.
    /// Returns every item reached, with the link it was first reached by
    /// (`None` for the starts).
    fn walk<'a>(
        &'a self,
        starts: &[&'a str],
        direction: Direction,
        kinds: &[EdgeKind],
    ) -> Walk<'a> {
        let mut reached: Walk = starts.iter().map(|&id| (id, None)).collect();
        let mut queue: VecDeque<&str> = starts.iter().copied().collect();
        while let Some(current) = queue.pop_front() {
            let links: Vec<&Edge> = match direction {
                Direction::Forward => self.edges_out(current).collect(),
                Direction::Backward => self.edges_in(current).collect(),
            };
            for e in links.into_iter().filter(|e| kinds.contains(&e.kind)) {
                let next = match direction {
                    Direction::Forward => e.to.as_str(),
                    Direction::Backward => e.from.as_str(),
                };
                if !reached.contains_key(next) {
                    reached.insert(next, Some(e));
                    queue.push_back(next);
                }
            }
        }
        reached
    }

    /// The route a [`Walk`] found to `id`, in call order.
    ///
    /// Backward walks start at the target and find callers, so the route runs
    /// from `id` (excluded) to the target (included). Forward walks run from
    /// the start (excluded) to `id` (included).
    fn steps<'a>(&'a self, walk: &Walk<'a>, id: &'a str, direction: Direction) -> Steps<'a> {
        let mut steps = Steps::default();
        let mut at = id;
        while let Some(Some(e)) = walk.get(at) {
            steps.through_trait |= e.kind == EdgeKind::MayCall;
            steps.named_only |= e.kind == EdgeKind::References;
            match direction {
                Direction::Backward => {
                    at = e.to.as_str();
                    steps.ids.push(at);
                }
                Direction::Forward => {
                    steps.ids.push(at);
                    at = e.from.as_str();
                }
            }
        }
        if direction == Direction::Forward {
            steps.ids.reverse();
        }
        steps
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

/// Which way [`Atlas::walk`] follows links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// From `from` to `to`: from a function to what it calls.
    Forward,
    /// From `to` to `from`: from a function to its callers.
    Backward,
}

/// Every item a walk reached, with the link it was first reached by.
type Walk<'a> = HashMap<&'a str, Option<&'a Edge>>;

/// A route found by a walk, and which kinds of links it used.
#[derive(Default)]
struct Steps<'a> {
    ids: Vec<&'a str>,
    through_trait: bool,
    named_only: bool,
}

impl Steps<'_> {
    /// What a reader should know about the route, or nothing.
    fn notes(&self) -> String {
        let mut notes = String::new();
        if self.through_trait {
            notes.push_str("  (through a trait)");
        }
        if self.named_only {
            notes.push_str("  (one step names a function without calling it)");
        }
        notes
    }
}

/// Things a `callers`, `callees` or `tests` question makes sense for.
fn is_used_by_name(kind: NodeKind) -> bool {
    kind.is_callable() || matches!(kind, NodeKind::Macro | NodeKind::Const | NodeKind::Static)
}

/// `12 sites, 4 without a SAFETY comment or # Safety section`, or `none`.
fn tally(sites: &[&UnsafeSite]) -> String {
    let missing = sites.iter().filter(|s| !s.documented).count();
    match sites.len() {
        0 => "none".to_string(),
        n => format!(
            "{n} {}, {missing} without a SAFETY comment or # Safety section",
            if n == 1 { "site" } else { "sites" }
        ),
    }
}

fn unsafe_kind_name(kind: UnsafeKind) -> &'static str {
    match kind {
        UnsafeKind::Block => "block",
        UnsafeKind::Fn => "fn",
        UnsafeKind::Impl => "impl",
        UnsafeKind::Trait => "trait",
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

/// Everything a query may call a node, without generic arguments: its name,
/// its path, its path with the crate in front, and for a method in a trait
/// impl, each of those with the type in place of `<Type as Trait>`.
fn short_forms(node: &Node) -> Vec<String> {
    let mut forms: Vec<String> = [&node.name, &node.path, &qualified(node)]
        .into_iter()
        .map(|form| strip_generics(form))
        .collect();
    let without_trait: Vec<String> = forms.iter().filter_map(|f| without_trait(f)).collect();
    forms.extend(without_trait);
    forms
}

/// `GlobBuilder<'a>::build` becomes `GlobBuilder::build`, and
/// `<Wrapper<T> as Area>::area` becomes `<Wrapper as Area>::area`. A `<`
/// right after a name opens generic arguments; the `<` of `<T as Trait>`
/// doesn't follow a name, so it stays.
fn strip_generics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0;
    let mut previous: Option<char> = None;
    for ch in name.chars() {
        if depth > 0 {
            match ch {
                '<' => depth += 1,
                '>' => depth -= 1,
                _ => {}
            }
            continue;
        }
        if ch == '<' && previous.is_some_and(|p| p.is_alphanumeric() || p == '_') {
            depth = 1;
            continue;
        }
        out.push(ch);
        previous = Some(ch);
    }
    out
}

/// `shapes::<Square as Area>::area` becomes `shapes::Square::area`.
/// Expects a name already passed through [`strip_generics`].
fn without_trait(name: &str) -> Option<String> {
    let start = name.find('<')?;
    let (self_type, rest) = name[start + 1..].split_once(" as ")?;
    let (_, item) = rest.split_once(">::")?;
    Some(format!("{}{self_type}::{item}", &name[..start]))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_generic_arguments_but_not_trait_brackets() {
        assert_eq!(
            strip_generics("GlobBuilder<'a>::build"),
            "GlobBuilder::build"
        );
        assert_eq!(
            strip_generics("<Wrapper<T> as Area>::area"),
            "<Wrapper as Area>::area"
        );
        assert_eq!(strip_generics("<Vec<Box<dyn A>> as B>::c"), "<Vec as B>::c");
        assert_eq!(strip_generics("plain::path"), "plain::path");
    }

    #[test]
    fn a_trait_impl_method_is_also_type_then_method() {
        assert_eq!(
            without_trait("shapes::<Square as Area>::area").as_deref(),
            Some("shapes::Square::area")
        );
        assert_eq!(without_trait("Square::area"), None);
    }
}
