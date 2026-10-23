//! Answering questions from `graph.json`: callers, callees, impls, path,
//! explain, search, tests and unsafe code.
//!
//! Every answer is plain text with `file:line` locations, short enough for an
//! AI assistant to read in one go and then open only the lines it needs.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::model::{
    Confidence, Edge, EdgeKind, FORMAT_VERSION, Graph, Node, NodeKind, Target, TargetKind,
    UnsafeKind, UnsafeSite,
};
use crate::symbols::{strip_generics, without_trait};

/// The graph plus lookup tables for fast queries.
pub struct Atlas {
    graph: Graph,
    by_id: HashMap<String, usize>,
    /// Edge positions in `graph.edges`, by the node they leave / arrive at.
    outgoing: HashMap<String, Vec<usize>>,
    incoming: HashMap<String, Vec<usize>>,
    /// Positions in `graph.targets` of the targets each file belongs to.
    file_targets: HashMap<String, Vec<usize>>,
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
        let mut file_targets: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, target) in graph.targets.iter().enumerate() {
            for file in &target.files {
                file_targets.entry(file.clone()).or_default().push(i);
            }
        }
        Atlas {
            graph,
            by_id,
            outgoing,
            incoming,
            file_targets,
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
        match self.candidates(query)?.as_slice() {
            [one] => Ok(*one),
            many => Err(self.ambiguous(query, many)),
        }
    }

    /// Every node `query` may mean, as [`Atlas::find`] reads it. An error
    /// when there is none.
    fn candidates(&self, query: &str) -> Result<Vec<&Node>> {
        let nodes = &self.graph.nodes;
        let candidates: Vec<&Node> = if let Some((file, line)) = parse_location(query) {
            let found: Vec<&Node> = nodes
                .iter()
                .filter(|n| n.line == Some(line))
                .filter(|n| n.file.as_deref().is_some_and(|f| same_file(f, file)))
                .collect();
            if found.is_empty() {
                bail!("nothing is defined at `{query}` (use the line of the item's name)");
            }
            found
        } else {
            // Written exactly as the graph writes it, first. An id picks
            // nodes known only by name, such as `derive:Debug`.
            let exact: Vec<&Node> = nodes
                .iter()
                .filter(|n| {
                    n.name == query || n.path == query || qualified(n) == query || n.id == query
                })
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
        if !candidates.is_empty() {
            return Ok(candidates);
        }
        // A path through a re-export, such as `tokio::task::spawn_blocking`,
        // isn't where the item is defined; offer the items with that name.
        let short = strip_generics(query);
        let last = short.rsplit("::").next().unwrap_or(&short);
        if short.contains("::") {
            let mut same_name: Vec<String> = nodes
                .iter()
                .filter(|n| strip_generics(&n.path).rsplit("::").next() == Some(last))
                .map(|n| format!("{}  ({})", selector(n), location(n)))
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

    /// The answer for a name that matches several items: each one, written
    /// so that asking again with it picks that one.
    fn ambiguous(&self, query: &str, many: &[&Node]) -> anyhow::Error {
        let mut list: Vec<String> = many
            .iter()
            .map(|n| format!("{}  ({})", selector(n), location(n)))
            .collect();
        list.sort();
        // A location if one has it; otherwise one of the names listed.
        let example = many
            .iter()
            .find_map(|n| Some(format!("{}:{}", n.file.as_ref()?, n.line?)))
            .or_else(|| many.iter().map(|n| selector(n)).min())
            .unwrap_or_default();
        Ambiguous(format!(
            "`{query}` matches {} items. Pick one by its full path or its location \
             (for example `{example}`):\n  {}",
            many.len(),
            list.join("\n  ")
        ))
        .into()
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
        // `drop`, or `from` behind `?`, has callers the code doesn't spell
        // out. Say why the list is empty rather than suggest nothing calls it.
        let empty = match self.implicit_impl(target) {
            Some((trait_name, owner)) => format!(
                "no callers found. {} `tests` on it lists the tests that use {}",
                implicit_explanation(trait_name),
                owner.name
            ),
            None => "no callers found".to_string(),
        };
        Ok(self.link_list(target, "<-", rows, &empty))
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
    ///
    /// `#[derive(Debug)]` and `impl fmt::Debug for X` both implement `Debug`,
    /// so a trait from outside the workspace and the derive with its name
    /// answer together.
    pub fn impls(&self, query: &str) -> Result<String> {
        let found = self.candidates(query)?;
        let node = match found.as_slice() {
            [one] => *one,
            many => match derive_and_its_trait(many) {
                Some(the_trait) => the_trait,
                None => return Err(self.ambiguous(query, many)),
            },
        };
        let is_impls_edge = |e: &&Edge| matches!(e.kind, EdgeKind::Implements | EdgeKind::Derives);
        let type_side = node.kind.is_type_like() && node.kind != NodeKind::Trait;
        let mut rows: Vec<(&Node, &Edge)> = if type_side {
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
        if let Some(derive) = self.derive_of(node) {
            rows.extend(
                self.edges_in(&derive.id)
                    .filter(is_impls_edge)
                    .map(|e| (self.node(&e.from), e)),
            );
        }
        let arrow = if type_side { "->" } else { "<-" };
        Ok(self.link_list(node, arrow, rows, "no impls found"))
    }

    /// The derive with the name of `node`, a trait from outside the
    /// workspace, when no other such trait has that name. A derive is known
    /// by its name only, so with two traits named `Error` it can't be told
    /// which one a `#[derive(Error)]` implements.
    fn derive_of(&self, node: &Node) -> Option<&Node> {
        if node.kind != NodeKind::ExternalTrait {
            return None;
        }
        let traits = self
            .graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::ExternalTrait && n.name == node.name)
            .count();
        if traits != 1 {
            return None;
        }
        self.graph
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Derive && n.name == node.name)
    }

    /// How one item reaches another.
    ///
    /// First, the shortest chain of calls from `from` to `to`: calls, the
    /// impls that may run behind a trait call, functions passed by name, and
    /// for a type at the end, a use of it. That is what "how does main reach
    /// this?" asks. Then the same the other way round. Only when neither
    /// exists, the shortest connection through any link, said to be one:
    /// that two items mention one type says little about how code runs.
    /// Crates and modules contain everything, so that last search tries
    /// without them first.
    pub fn path(&self, from: &str, to: &str) -> Result<String> {
        let start = self.find(from)?;
        let goal = self.find(to)?;
        let mut out = String::new();
        if start.id == goal.id {
            let _ = writeln!(
                out,
                "`{from}` and `{to}` are the same item: {}  ({})",
                start.name,
                location(start)
            );
            return Ok(out);
        }
        if let Some(hops) = self.call_chain(start, goal) {
            let _ = writeln!(
                out,
                "Shortest chain of calls from {} to {}: {} link(s)",
                start.name,
                goal.name,
                hops.len()
            );
            out.push_str(&self.chain_lines(self.chain_start(&hops, goal), &hops));
            return Ok(out);
        }
        if let Some(hops) = self.call_chain(goal, start) {
            let _ = writeln!(
                out,
                "No chain of calls leads from {} to {}, but one leads from {} to {}: {} link(s)",
                start.name,
                goal.name,
                goal.name,
                start.name,
                hops.len()
            );
            out.push_str(&self.chain_lines(self.chain_start(&hops, start), &hops));
            return Ok(out);
        }
        let is_hub = |id: &str| matches!(self.node(id).kind, NodeKind::Crate | NodeKind::Module);
        let hops = self
            .shortest_path(&start.id, &goal.id, |e| !is_hub(&e.from) && !is_hub(&e.to))
            .or_else(|| self.shortest_path(&start.id, &goal.id, |_| true));
        let Some(hops) = hops else {
            bail!("no path between `{from}` and `{to}`");
        };
        let _ = writeln!(
            out,
            "No chain of calls connects {} and {} in either direction. The shortest connection \
             through any kind of link (type uses, impls, containment): {} link(s)",
            start.name,
            goal.name,
            hops.len()
        );
        out.push_str(&self.chain_lines(&start.id, &hops));
        Ok(out)
    }

    /// The shortest chain of calls from `start` to `goal`, as edge
    /// positions. A type or trait starts from its methods, as in `tests`.
    /// When `goal` is a type, a use of it ends the chain.
    fn call_chain(&self, start: &Node, goal: &Node) -> Option<Vec<usize>> {
        let goal_is_type = goal.kind.is_type_like();
        let step = |e: &Edge| {
            matches!(
                e.kind,
                EdgeKind::Calls | EdgeKind::MayCall | EdgeKind::References
            ) || (goal_is_type && e.kind == EdgeKind::UsesType && e.to == goal.id)
        };
        let sources = self.starts_for(start);
        let mut came_from: HashMap<&str, usize> = HashMap::new();
        let mut seen: HashSet<&str> = sources.iter().copied().collect();
        let mut queue: VecDeque<&str> = sources.iter().copied().collect();
        while let Some(current) = queue.pop_front() {
            if current == goal.id {
                let mut hops = Vec::new();
                let mut at = current;
                while let Some(&i) = came_from.get(at) {
                    hops.push(i);
                    at = self.graph.edges[i].from.as_str();
                }
                hops.reverse();
                return Some(hops);
            }
            for &i in self.outgoing.get(current).into_iter().flatten() {
                let e = &self.graph.edges[i];
                if step(e) && seen.insert(e.to.as_str()) {
                    came_from.insert(e.to.as_str(), i);
                    queue.push_back(e.to.as_str());
                }
            }
        }
        None
    }

    /// Where a chain from [`Atlas::call_chain`] starts: a method, when it
    /// started from a type, or `goal` itself for a chain of no links.
    fn chain_start<'a>(&'a self, hops: &[usize], goal: &'a Node) -> &'a str {
        hops.first()
            .map_or(goal.id.as_str(), |&i| self.graph.edges[i].from.as_str())
    }

    /// The links of a path, one per line, from `first` on.
    fn chain_lines(&self, first: &str, hops: &[usize]) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "  {}", self.node(first).name);
        let mut current = first;
        for &i in hops {
            let e = &self.graph.edges[i];
            let kind = kind_label(e.kind);
            let (arrow, next) = if e.from == current {
                (format!("--{kind}-->"), e.to.as_str())
            } else {
                (format!("<--{kind}--"), e.from.as_str())
            };
            let note = match e.kind {
                EdgeKind::MayCall => "  (through the trait)",
                EdgeKind::References if self.node(&e.to).kind.is_callable() => {
                    "  (named, not called)"
                }
                _ => "",
            };
            let _ = writeln!(
                out,
                "    {arrow} {}   {}{note}",
                self.node(next).name,
                edge_location(e)
            );
            current = next;
        }
        out
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

    /// The tests that reach an item through calls, and the `cargo test`
    /// commands that run only them.
    ///
    /// Walks `calls`, `may_call` and `references` links backwards from the
    /// item, as far as they go. For a type it starts from the type's methods;
    /// for a trait, from the trait's methods and every impl of them.
    ///
    /// Rust calls some methods without naming them: `drop`, `from` behind
    /// `?`, `next` in a `for` loop, `fmt` in a format string. For a method
    /// of such an impl that no call reaches, the tests that use its type are
    /// the ones that may run it, and the answer says so.
    pub fn tests(&self, query: &str) -> Result<String> {
        let target = self.find(query)?;
        if !target.kind.is_type_like() && !is_used_by_name(target.kind) {
            bail!(
                "`{}` is a {}; `tests` works on functions, methods, macros, constants, types and traits",
                target.name,
                kind_name(target.kind)
            );
        }
        let kinds = [EdgeKind::Calls, EdgeKind::MayCall, EdgeKind::References];
        let mut walk = self.walk(&self.starts_for(target), Direction::Backward, &kinds);
        let mut tests = self.tests_in(&walk, target);
        let mut implicit = None;
        // A let chain (Rust 2024): both conditions must hold, and the second binds names.
        if tests.is_empty()
            && let Some((trait_name, owner)) = self.implicit_impl(target)
        {
            walk = self.walk(&self.users_of(owner), Direction::Backward, &kinds);
            tests = self.tests_in(&walk, target);
            implicit = Some((trait_name, owner));
        }

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
        if let Some((trait_name, owner)) = implicit {
            let _ = writeln!(
                out,
                "  No call to it appears in the code. {}",
                implicit_explanation(trait_name)
            );
            if tests.is_empty() {
                let _ = writeln!(out, "  (no tests use {} either)", owner.name);
                return Ok(out);
            }
            let _ = writeln!(out, "  These tests use {}, so they may run it:", owner.name);
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
            let how = match (names.is_empty(), implicit) {
                (true, Some((_, owner))) => format!("uses {}", owner.name),
                (true, None) => "direct".to_string(),
                (false, _) => format!("via {}", names.join(" -> ")),
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

        let _ = writeln!(out, "\nRun them:");
        for command in self.test_commands(&tests) {
            let _ = writeln!(out, "  {command}");
        }
        if tests.iter().any(|t| t.parameterized) {
            let _ = writeln!(
                out,
                "  (A parameterized test runs once per case, each named after it, such as \
                 `adds::case_1`, so the command without `--exact` matches names that contain \
                 the ones given.)"
            );
        }
        Ok(out)
    }

    /// The tests a backward walk reached, other than `target`, by file and line.
    fn tests_in<'a>(&'a self, walk: &Walk<'a>, target: &Node) -> Vec<&'a Node> {
        let mut tests: Vec<&Node> = walk
            .keys()
            .map(|id| self.node(id))
            .filter(|n| n.test && n.id != target.id)
            .collect();
        tests.sort_by_key(|n| (n.file.clone(), n.line, n.id.clone()));
        tests
    }

    /// Where a walk along calls starts for `node`: the node itself for
    /// anything that runs or is used by name; for a type, its methods; for a
    /// trait, its methods and every impl of them.
    fn starts_for<'a>(&'a self, node: &'a Node) -> Vec<&'a str> {
        if !node.kind.is_type_like() {
            return vec![node.id.as_str()];
        }
        let methods: Vec<&str> = self
            .edges_out(&node.id)
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
    }

    /// The type's methods, and the functions, consts and statics that name
    /// the type: what may make or handle a value of it.
    fn users_of<'a>(&'a self, owner: &'a Node) -> Vec<&'a str> {
        let mut starts = self.starts_for(owner);
        starts.extend(
            self.edges_in(&owner.id)
                .filter(|e| e.kind == EdgeKind::UsesType)
                .map(|e| e.from.as_str())
                .filter(|id| is_used_by_name(self.node(id).kind)),
        );
        starts.sort_unstable();
        starts.dedup();
        starts
    }

    /// For a method in an impl of a trait from outside the workspace, such
    /// as `<Guard as Drop>::drop`: the trait's name and the type. Rust calls
    /// many such methods without naming them, so no link shows those calls.
    fn implicit_impl<'a>(&'a self, node: &'a Node) -> Option<(&'a str, &'a Node)> {
        if node.kind != NodeKind::Method {
            return None;
        }
        // A method of a workspace trait's impl implements the trait's own
        // method, and calls through the trait are linked as `may_call`.
        if self
            .edges_out(&node.id)
            .any(|e| e.kind == EdgeKind::Implements)
        {
            return None;
        }
        let trait_name = trait_in_name(&node.name)?;
        let owner = self
            .edges_in(&node.id)
            .find(|e| e.kind == EdgeKind::Contains)
            .map(|e| self.node(&e.from))?;
        // The type must implement a trait from outside with that name. A
        // workspace trait's impl can also lack the link above, when syn
        // couldn't read its file, and that impl isn't called implicitly.
        let outside = self.edges_out(&owner.id).any(|e| {
            let to = self.node(&e.to);
            e.kind == EdgeKind::Implements
                && to.kind == NodeKind::ExternalTrait
                && to.name == trait_name
        });
        outside.then_some((trait_name, owner))
    }

    /// The targets of `test`'s package that its file belongs to.
    fn targets_of(&self, test: &Node) -> Vec<&Target> {
        let Some(file) = &test.file else {
            return Vec::new();
        };
        self.file_targets
            .get(file)
            .into_iter()
            .flatten()
            .map(|&i| &self.graph.targets[i])
            .filter(|t| t.package == test.krate)
            .collect()
    }

    /// `cargo test` commands that run exactly `tests`: one per package, or two
    /// when some tests are parameterized.
    ///
    /// Each command picks the test binaries the tests are in (`--lib`,
    /// `--test api`...) when every test's target is known, so Cargo builds
    /// only those. Each test is named by its full path with `--exact`, as
    /// libtest otherwise runs every test whose name contains a filter
    /// (`tests::parse` would also run `tests::parse_twice`). A parameterized
    /// test's cases are named after it (`adds::case_1`), so `--exact` can't
    /// select them, and those are matched by substring in a command of their own.
    fn test_commands(&self, tests: &[&Node]) -> Vec<String> {
        struct Run<'a> {
            flags: BTreeSet<String>,
            every_target_known: bool,
            names: BTreeSet<&'a str>,
        }
        // By package, then exact matches before substring ones.
        let mut runs: BTreeMap<(&str, bool), Run> = BTreeMap::new();
        for test in tests {
            let run = runs
                .entry((test.krate.as_str(), test.parameterized))
                .or_insert_with(|| Run {
                    flags: BTreeSet::new(),
                    every_target_known: true,
                    names: BTreeSet::new(),
                });
            let flags: Vec<String> = self
                .targets_of(test)
                .into_iter()
                .filter_map(target_flag)
                .collect();
            run.every_target_known &= !flags.is_empty();
            run.flags.extend(flags);
            run.names.insert(test.path.as_str());
        }
        let mut commands = Vec::new();
        for ((package, by_substring), run) in runs {
            let mut command = format!("cargo test -p {package}");
            if run.every_target_known {
                for flag in &run.flags {
                    command.push(' ');
                    command.push_str(flag);
                }
            }
            // Past the limit, run every test of those targets instead.
            if run.names.len() <= LIST_LIMIT {
                command.push_str(if by_substring { " --" } else { " -- --exact" });
                for name in &run.names {
                    command.push(' ');
                    command.push_str(name);
                }
            }
            commands.push(command);
        }
        commands
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
            // Naming a function without calling it, not reading a const or static.
            steps.named_only |=
                e.kind == EdgeKind::References && self.node(&e.to).kind.is_callable();
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
                // The index has no occurrence there; see `add_macro_body_calls`.
                (EdgeKind::Calls, Confidence::Syntax) => "  (in the macro's body, matched by path)",
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
    // `std` re-exports `core` and `alloc`: `std::fmt::Write` is `core::fmt::Write`.
    if node.kind == NodeKind::ExternalTrait && matches!(node.krate.as_str(), "core" | "alloc") {
        forms.push(strip_generics(&format!("std::{}", node.path)));
    }
    let without_trait: Vec<String> = forms.iter().filter_map(|f| without_trait(f)).collect();
    forms.extend(without_trait);
    forms
}

/// `spike::json_reader::JsonReader::parse`: the path with its crate in front.
pub(crate) fn qualified(node: &Node) -> String {
    if node.krate.is_empty() || node.kind == NodeKind::Crate {
        node.path.clone()
    } else {
        format!("{}::{}", node.krate.replace('-', "_"), node.path)
    }
}

/// What to ask for to get exactly this node: its path with the crate in
/// front, or for a node known only by its name, its id (`derive:Debug`).
fn selector(node: &Node) -> String {
    let by_name_only = node.kind == NodeKind::Derive
        || (node.kind == NodeKind::ExternalTrait && node.id.starts_with("trait:"));
    if by_name_only {
        node.id.clone()
    } else {
        qualified(node)
    }
}

/// Whether `asked`, the file in a location such as `main.rs:4`, is the
/// indexed file `indexed`: the same path, its end after a `/` (`main.rs` is
/// `src/main.rs` but not `src/domain.rs`), or a longer path that ends with
/// it, such as an absolute one.
fn same_file(indexed: &str, asked: &str) -> bool {
    let asked = asked.replace('\\', "/");
    let asked = asked.strip_prefix("./").unwrap_or(&asked);
    indexed == asked
        || indexed.ends_with(&format!("/{asked}"))
        || asked.ends_with(&format!("/{indexed}"))
}

/// `Debug` read from `#[derive(Debug)]` and the trait `core::fmt::Debug`:
/// the trait, which `impls` answers for both.
fn derive_and_its_trait<'a>(many: &[&'a Node]) -> Option<&'a Node> {
    let [a, b] = many else {
        return None;
    };
    let (derive, the_trait) = match (a.kind, b.kind) {
        (NodeKind::Derive, NodeKind::ExternalTrait) => (*a, *b),
        (NodeKind::ExternalTrait, NodeKind::Derive) => (*b, *a),
        _ => return None,
    };
    (derive.name == the_trait.name).then_some(the_trait)
}

/// `Drop` in `<Guard as Drop>::drop`, `From` in `<E as From<io::Error>>::from`.
fn trait_in_name(name: &str) -> Option<&str> {
    let after = name.split_once(" as ")?.1;
    let end = after.find(['<', '>'])?;
    Some(&after[..end])
}

/// Why the call graph has no calls to a method of a trait from outside the workspace.
fn implicit_explanation(trait_name: &str) -> String {
    format!(
        "`{trait_name}` is a trait from outside the workspace, and Rust calls such methods \
         without naming them (`?` calls `from`, a `for` loop calls `next`, `{{}}` in a format \
         string calls `fmt`, a value going out of scope calls `drop`), so those calls aren't \
         in the code."
    )
}

/// How `cargo test` selects a target's test binary. A build script has none.
fn target_flag(target: &Target) -> Option<String> {
    Some(match target.kind {
        TargetKind::Lib => "--lib".to_string(),
        TargetKind::Bin => format!("--bin {}", target.name),
        TargetKind::Test => format!("--test {}", target.name),
        TargetKind::Example => format!("--example {}", target.name),
        TargetKind::Bench => format!("--bench {}", target.name),
        TargetKind::BuildScript => return None,
    })
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

    /// A node of a test graph, in package `app`; its path is its name.
    fn node(id: &str, name: &str, kind: NodeKind, at: Option<(&str, u32)>) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            path: name.to_string(),
            kind,
            krate: "app".to_string(),
            file: at.map(|(file, _)| file.to_string()),
            line: at.map(|(_, line)| line),
            signature: None,
            test: false,
            parameterized: false,
        }
    }

    fn test_fn(id: &str, path: &str, at: (&str, u32)) -> Node {
        Node {
            test: true,
            ..node(
                id,
                path.rsplit("::").next().unwrap(),
                NodeKind::Function,
                Some(at),
            )
        }
        .with_path(path)
    }

    impl Node {
        fn with_path(mut self, path: &str) -> Node {
            self.path = path.to_string();
            self
        }
    }

    fn link(from: &str, to: &str, kind: EdgeKind, confidence: Confidence, at: (&str, u32)) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind,
            confidence,
            file: Some(at.0.to_string()),
            lines: vec![at.1],
        }
    }

    fn atlas(nodes: Vec<Node>, edges: Vec<Edge>, targets: Vec<Target>) -> Atlas {
        Atlas::new(Graph {
            format_version: FORMAT_VERSION,
            nodes,
            edges,
            targets,
            ..Graph::default()
        })
    }

    /// The spike fixture's shape: `main` names `CsvReader` and calls `run`,
    /// which calls `Loader::load` on a `dyn Loader`.
    fn spike() -> Atlas {
        use Confidence::*;
        use EdgeKind::*;
        let nodes = vec![
            node(
                "main",
                "main",
                NodeKind::Function,
                Some(("src/main.rs", 13)),
            ),
            node("run", "run", NodeKind::Function, Some(("src/main.rs", 9))),
            node(
                "describe",
                "describe",
                NodeKind::Function,
                Some(("src/main.rs", 20)),
            ),
            node(
                "Csv",
                "CsvReader",
                NodeKind::Struct,
                Some(("src/csv.rs", 2)),
            ),
            node(
                "parse",
                "CsvReader::parse",
                NodeKind::Method,
                Some(("src/csv.rs", 10)),
            ),
            node(
                "load",
                "Loader::load",
                NodeKind::TraitMethod,
                Some(("src/loader.rs", 5)),
            ),
            node(
                "csv_load",
                "<CsvReader as Loader>::load",
                NodeKind::Method,
                Some(("src/loader.rs", 9)),
            ),
        ];
        let edges = vec![
            link("main", "Csv", UsesType, Exact, ("src/main.rs", 16)),
            link("main", "run", Calls, Exact, ("src/main.rs", 17)),
            link("run", "load", Calls, Exact, ("src/main.rs", 10)),
            link("run", "csv_load", MayCall, Candidate, ("src/main.rs", 10)),
            link("csv_load", "parse", Calls, Exact, ("src/loader.rs", 9)),
            link("csv_load", "load", Implements, Exact, ("src/loader.rs", 9)),
            link("Csv", "parse", Contains, Exact, ("src/csv.rs", 10)),
            link("Csv", "csv_load", Contains, Exact, ("src/loader.rs", 9)),
            link("describe", "Csv", UsesType, Exact, ("src/main.rs", 21)),
        ];
        atlas(nodes, edges, Vec::new())
    }

    #[test]
    fn path_follows_calls_not_a_shorter_type_mention() {
        let a = spike();
        // `main` names `CsvReader`, which contains `parse`: two links, but not how main gets there.
        let out = a.path("main", "CsvReader::parse").unwrap();
        assert_eq!(
            out,
            "Shortest chain of calls from main to CsvReader::parse: 3 link(s)\n  \
             main\n    \
             --calls--> run   src/main.rs:17\n    \
             --may_call--> <CsvReader as Loader>::load   src/main.rs:10  (through the trait)\n    \
             --calls--> CsvReader::parse   src/loader.rs:9\n"
        );
        let back = a.path("CsvReader::parse", "main").unwrap();
        assert!(
            back.starts_with(
                "No chain of calls leads from CsvReader::parse to main, but one leads from main \
                 to CsvReader::parse: 3 link(s)"
            ),
            "{back}"
        );
        // A type at the end: a use of it ends the chain.
        let to_type = a.path("main", "CsvReader").unwrap();
        assert!(to_type.contains("--uses_type--> CsvReader"), "{to_type}");
        // No calls either way: the connection through any link, said to be one.
        let loose = a.path("describe", "CsvReader::parse").unwrap();
        assert!(
            loose.starts_with("No chain of calls connects describe and CsvReader::parse"),
            "{loose}"
        );
        assert!(loose.contains("--contains--> CsvReader::parse"), "{loose}");
        // A type starts from its methods.
        let from_type = a.path("CsvReader", "CsvReader::parse").unwrap();
        assert!(
            from_type.starts_with("Shortest chain of calls"),
            "{from_type}"
        );
    }

    #[test]
    fn impls_answers_a_derive_and_its_trait_together_and_tells_traits_apart() {
        use Confidence::*;
        use EdgeKind::*;
        let outside = |id: &str, krate: &str, path: &str| Node {
            krate: krate.to_string(),
            path: path.to_string(),
            ..node(
                id,
                path.rsplit("::").next().unwrap(),
                NodeKind::ExternalTrait,
                None,
            )
        };
        let nodes = vec![
            node("text", "Text", NodeKind::Struct, Some(("src/w.rs", 7))),
            node("bytes", "Bytes", NodeKind::Struct, Some(("src/w.rs", 16))),
            Node {
                krate: String::new(),
                ..node("derive:Debug", "Debug", NodeKind::Derive, None)
            },
            outside("core-debug", "core", "fmt::Debug"),
            outside("core-write", "core", "fmt::Write"),
            outside("io-write", "std", "io::Write"),
        ];
        let edges = vec![
            link("text", "derive:Debug", Derives, Syntax, ("src/w.rs", 7)),
            link("bytes", "core-debug", Implements, Exact, ("src/w.rs", 28)),
            link("text", "core-write", Implements, Exact, ("src/w.rs", 9)),
            link("bytes", "io-write", Implements, Exact, ("src/w.rs", 18)),
        ];
        let a = atlas(nodes, edges, Vec::new());

        let debug = a.impls("Debug").unwrap();
        assert!(
            debug.contains("<- Bytes") && debug.contains("<- Text"),
            "{debug}"
        );
        assert!(debug.contains("(derive)"), "{debug}");
        assert!(a.impls("derive:Debug").unwrap().contains("<- Text"));

        let err = a.impls("Write").unwrap_err().to_string();
        assert!(
            err.contains("core::fmt::Write  (outside the workspace)")
                && err.contains("std::io::Write  (outside the workspace)")
                && err.contains("for example `core::fmt::Write`"),
            "{err}"
        );
        let fmt = a.impls("std::fmt::Write").unwrap();
        assert!(fmt.contains("<- Text") && !fmt.contains("Bytes"), "{fmt}");
        let io = a.impls("std::io::Write").unwrap();
        assert!(io.contains("<- Bytes") && !io.contains("Text"), "{io}");
    }

    #[test]
    fn a_location_matches_whole_file_names_only() {
        assert!(same_file("src/main.rs", "main.rs"));
        assert!(same_file("src/main.rs", "src/main.rs"));
        assert!(same_file("src/main.rs", "./src/main.rs"));
        assert!(same_file("src/main.rs", "/home/me/app/src/main.rs"));
        assert!(!same_file("src/domain.rs", "main.rs"));

        let a = atlas(
            vec![node(
                "d",
                "on_line_four",
                NodeKind::Function,
                Some(("src/domain.rs", 4)),
            )],
            Vec::new(),
            Vec::new(),
        );
        let err = a.find("main.rs:4").unwrap_err().to_string();
        assert!(err.contains("nothing is defined at `main.rs:4`"), "{err}");
        assert_eq!(a.find("domain.rs:4").unwrap().id, "d");
    }

    #[test]
    fn test_commands_pick_targets_and_exact_names() {
        use Confidence::*;
        use EdgeKind::*;
        let target = |name: &str, kind: TargetKind, files: &[&str]| Target {
            package: "app".to_string(),
            name: name.to_string(),
            kind,
            files: files.iter().map(|f| f.to_string()).collect(),
        };
        let mut param = test_fn("t_param", "tests::adds", ("src/lib.rs", 20));
        param.parameterized = true;
        let nodes = vec![
            node("f", "f", NodeKind::Function, Some(("src/lib.rs", 1))),
            test_fn("t_lib", "tests::t_lib", ("src/lib.rs", 10)),
            test_fn("t_bin", "cli::tests::t_bin", ("src/cli.rs", 5)),
            test_fn("t_api", "t_api", ("tests/api.rs", 3)),
            param,
        ];
        let edges = ["t_lib", "t_bin", "t_api", "t_param"]
            .into_iter()
            .map(|t| link(t, "f", Calls, Exact, ("src/lib.rs", 2)))
            .collect();
        let targets = vec![
            target("app", TargetKind::Lib, &["src/lib.rs"]),
            target("app", TargetKind::Bin, &["src/main.rs", "src/cli.rs"]),
            target("api", TargetKind::Test, &["tests/api.rs"]),
            target("build-script-build", TargetKind::BuildScript, &["build.rs"]),
        ];
        let a = atlas(nodes, edges, targets);
        let out = a.tests("f").unwrap();
        let run = out.split_once("Run them:\n").unwrap().1;
        assert!(
            run.starts_with(
                "  cargo test -p app --bin app --lib --test api -- --exact cli::tests::t_bin \
                 t_api tests::t_lib\n  cargo test -p app --lib -- tests::adds\n"
            ),
            "{out}"
        );
        assert!(run.contains("parameterized"), "{out}");
    }

    #[test]
    fn without_a_known_target_the_command_runs_the_whole_package() {
        let nodes = vec![
            node("f", "f", NodeKind::Function, Some(("src/lib.rs", 1))),
            test_fn("t", "tests::t", ("src/lost.rs", 3)),
        ];
        let edges = vec![link(
            "t",
            "f",
            EdgeKind::Calls,
            Confidence::Exact,
            ("src/lost.rs", 4),
        )];
        let a = atlas(nodes, edges, Vec::new());
        assert!(
            a.tests("f")
                .unwrap()
                .contains("  cargo test -p app -- --exact tests::t\n")
        );
    }

    #[test]
    fn a_method_rust_calls_implicitly_falls_back_to_the_tests_that_use_its_type() {
        use Confidence::*;
        use EdgeKind::*;
        let nodes = vec![
            node(
                "guard",
                "Guard",
                NodeKind::Struct,
                Some(("src/guard.rs", 3)),
            ),
            node(
                "new",
                "Guard::new",
                NodeKind::Method,
                Some(("src/guard.rs", 8)),
            ),
            node(
                "drop",
                "<Guard as Drop>::drop",
                NodeKind::Method,
                Some(("src/guard.rs", 16)),
            ),
            node("make", "make", NodeKind::Function, Some(("src/lib.rs", 3))),
            test_fn("t", "tests::t", ("src/lib.rs", 10)),
            test_fn("other", "tests::other", ("src/lib.rs", 20)),
            Node {
                krate: "core".to_string(),
                ..node("core-drop", "Drop", NodeKind::ExternalTrait, None)
            },
            // An impl of a workspace trait whose method lost its link to the
            // trait's method (syn couldn't read the file): not called implicitly.
            node("Shape", "Shape", NodeKind::Trait, Some(("src/shape.rs", 1))),
            node(
                "area",
                "<Guard as Shape>::area",
                NodeKind::Method,
                Some(("src/shape.rs", 9)),
            ),
        ];
        let edges = vec![
            link("guard", "new", Contains, Exact, ("src/guard.rs", 8)),
            link("guard", "drop", Contains, Exact, ("src/guard.rs", 16)),
            link("guard", "area", Contains, Exact, ("src/shape.rs", 9)),
            link(
                "guard",
                "core-drop",
                Implements,
                Exact,
                ("src/guard.rs", 15),
            ),
            link("guard", "Shape", Implements, Exact, ("src/shape.rs", 8)),
            link("make", "new", Calls, Exact, ("src/lib.rs", 4)),
            link("make", "guard", UsesType, Exact, ("src/lib.rs", 3)),
            link("t", "make", Calls, Exact, ("src/lib.rs", 11)),
        ];
        let a = atlas(nodes, edges, Vec::new());
        assert!(
            a.tests("Guard::area")
                .unwrap()
                .ends_with("  (no tests reach it)\n")
        );

        let out = a.tests("Guard::drop").unwrap();
        assert!(
            out.contains("No call to it appears in the code. `Drop` is a trait"),
            "{out}"
        );
        assert!(
            out.contains("These tests use Guard, so they may run it:"),
            "{out}"
        );
        assert!(out.contains("t  src/lib.rs:10  via make"), "{out}");
        assert!(!out.contains("other"), "{out}");

        let callers = a.callers("Guard::drop").unwrap();
        assert!(
            callers.contains("(no callers found. `Drop` is a trait from outside the workspace"),
            "{callers}"
        );
        // An ordinary function with no tests gets the plain answer.
        assert!(
            a.tests("make")
                .unwrap()
                .contains("t  src/lib.rs:10  direct")
        );
    }

    #[test]
    fn reads_the_trait_out_of_an_impl_method_name() {
        assert_eq!(trait_in_name("<Guard as Drop>::drop"), Some("Drop"));
        assert_eq!(
            trait_in_name("<MoneyError as From<ParseIntError>>::from"),
            Some("From")
        );
        assert_eq!(trait_in_name("Guard::new"), None);
    }
}
