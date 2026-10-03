//! A short Markdown summary of the graph: size, link types, hot spots, and gaps.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::model::{Confidence, EdgeKind, Graph, NodeKind};
use crate::query::{confidence_label, kind_label, kind_name};

/// How many rows to show in each "top" list.
const TOP: usize = 10;

pub fn markdown(graph: &Graph) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# cargo-atlas report\n");
    let _ = writeln!(
        out,
        "Produced by {}, with {}.\n",
        graph.produced_by,
        graph.features.describe()
    );

    // Nodes by kind.
    let mut node_counts: BTreeMap<NodeKind, usize> = BTreeMap::new();
    for n in &graph.nodes {
        *node_counts.entry(n.kind).or_default() += 1;
    }
    let _ = writeln!(out, "## Items ({})\n", graph.nodes.len());
    let _ = writeln!(out, "| Kind | Count |\n| --- | --- |");
    for (kind, count) in &node_counts {
        let _ = writeln!(out, "| {} | {count} |", kind_name(*kind));
    }

    // Edges by kind and confidence.
    let mut edge_counts: BTreeMap<(EdgeKind, Confidence), usize> = BTreeMap::new();
    for e in &graph.edges {
        *edge_counts.entry((e.kind, e.confidence)).or_default() += 1;
    }
    let _ = writeln!(out, "\n## Links ({})\n", graph.edges.len());
    let _ = writeln!(out, "| Link | Confidence | Count |\n| --- | --- | --- |");
    for ((kind, confidence), count) in &edge_counts {
        let _ = writeln!(
            out,
            "| {} | {} | {count} |",
            kind_label(*kind),
            confidence_label(*confidence)
        );
    }

    // Most-called functions: distinct callers, not call sites.
    let name_of: HashMap<&str, &str> = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.name.as_str()))
        .collect();
    // `implements` also links impl methods to trait methods; count only whole traits.
    let is_trait: HashMap<&str, bool> = graph
        .nodes
        .iter()
        .map(|n| {
            let whole_trait = matches!(n.kind, NodeKind::Trait | NodeKind::ExternalTrait);
            (n.id.as_str(), whole_trait)
        })
        .collect();
    let mut callers: HashMap<&str, usize> = HashMap::new();
    let mut implementors: HashMap<&str, usize> = HashMap::new();
    for e in &graph.edges {
        match e.kind {
            EdgeKind::Calls => *callers.entry(e.to.as_str()).or_default() += 1,
            EdgeKind::Implements if is_trait.get(e.to.as_str()) == Some(&true) => {
                *implementors.entry(e.to.as_str()).or_default() += 1
            }
            _ => {}
        }
    }
    top_list(
        &mut out,
        "Most-called functions (distinct callers)",
        &callers,
        &name_of,
    );
    top_list(&mut out, "Most-implemented traits", &implementors, &name_of);

    let tests = graph.nodes.iter().filter(|n| n.test).count();
    let undocumented = graph.unsafe_sites.iter().filter(|s| !s.documented).count();
    let _ = writeln!(out, "\n## Tests and unsafe code\n");
    let _ = writeln!(
        out,
        "- Test functions (marked `#[test]` or similar): {tests}. \
         `cargo atlas tests ITEM` lists the ones that reach an item."
    );
    let _ = writeln!(
        out,
        "- Unsafe code: {} sites, {undocumented} without a `// SAFETY:` comment or \
         `# Safety` section. `cargo atlas unsafe` lists them.",
        graph.unsafe_sites.len()
    );

    let s = &graph.stats;
    let _ = writeln!(out, "\n## Coverage\n");
    let _ = writeln!(out, "- Files indexed by rust-analyzer: {}", s.files_indexed);
    let _ = writeln!(
        out,
        "- Files syn could not parse (no impl, derive, test or unsafe facts): {}",
        s.files_unparsed_by_syn
    );
    let _ = writeln!(
        out,
        "- References to code outside the workspace (std, dependencies), not in the graph: {}",
        s.external_references
    );
    let _ = writeln!(
        out,
        "- References outside any function or type (`use` lines, impl headers): {}",
        s.unattributed_references
    );
    let _ = writeln!(
        out,
        "- Symbols rust-analyzer gave to more than one item (each still has its own node; \
         links to them that had to be guessed are marked CANDIDATE): {}",
        s.duplicate_symbols
    );
    if s.std_sources_found {
        let _ = writeln!(
            out,
            "- Macros: rust-analyzer expands the ones it can load, such as `println!` and your own \
             `macro_rules!`. Proc macros must compile first; code they generate is missing if they don't."
        );
    } else {
        let _ = writeln!(
            out,
            "- Warning: the standard library's source was missing, so calls inside std macros \
             such as `println!` are missing. Fix: `rustup component add rust-src`, then rebuild."
        );
    }
    out
}

fn top_list(
    out: &mut String,
    title: &str,
    counts: &HashMap<&str, usize>,
    name_of: &HashMap<&str, &str>,
) {
    let mut rows: Vec<(&str, usize)> = counts.iter().map(|(id, c)| (*id, *c)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let _ = writeln!(out, "\n## {title}\n");
    if rows.is_empty() {
        let _ = writeln!(out, "None.");
        return;
    }
    let _ = writeln!(out, "| Item | Count |\n| --- | --- |");
    for (id, count) in rows.into_iter().take(TOP) {
        let _ = writeln!(
            out,
            "| `{}` | {count} |",
            name_of.get(id).copied().unwrap_or(id)
        );
    }
}
