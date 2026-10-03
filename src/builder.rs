//! Turning rust-analyzer's index, Cargo's metadata and the syntax facts into one graph.
//!
//! The steps, in order:
//!
//! 1. Nodes: one per workspace crate, and one per definition in the index
//!    (functions, methods, types, traits, modules, constants, macros).
//! 2. Impl blocks: syn finds each impl header, and the index says which exact
//!    type and trait the header names. That gives `implements` edges, the type
//!    that owns each method, and the impls that could answer a trait call.
//! 3. Containment: crate > module > type > method.
//! 4. References: each reference in the index belongs to the innermost
//!    function or type whose span contains it. That gives `calls`,
//!    `references`, `may_call` and `uses_type` edges, with no name matching.
//! 5. Derives.
//! 6. Tests and unsafe code: syn finds them, and each is attached to its
//!    item the same way impl headers are.
//!
//! rust-analyzer usually gives each item its own symbol, but not always. `main` in `build.rs` and `main` in
//! `src/main.rs` share one symbol, and so do two structs with the same name
//! declared inside two different functions (rust-analyzer issue #18771).
//! So a symbol can have several definitions, each becomes its own node, and
//! [`Context::resolve`] decides which one a reference means.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::Path;

use scip::types::symbol_information::Kind;
use scip::types::{Document, Index, PositionEncoding, SymbolRole};

use crate::cargo_meta::Workspace;
use crate::model::{
    Confidence, EdgeKind, Graph, GraphBuilder, Node, NodeKind, UnsafeKind, UnsafeSite, edge,
};
use crate::symbols::{self, ParsedSymbol, Shape};
use crate::syntax::{self, FileFacts, NameAt};

/// A 0-based (line, character) position, as the index stores them.
type Pos = (u32, u32);

/// For one file: the symbols the index mentions on each 1-based line.
type SymbolsByLine<'a> = HashMap<u32, Vec<&'a str>>;

/// A start/end pair from the index. The end is exclusive.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: Pos,
    end: Pos,
}

impl Span {
    /// The index stores ranges as `[line, start_char, end_char]` when they sit
    /// on one line, or `[start_line, start_char, end_line, end_char]` otherwise.
    fn from_range(range: &[i32]) -> Option<Span> {
        let n = |i: usize| u32::try_from(range[i]).ok();
        match range.len() {
            3 => Some(Span {
                start: (n(0)?, n(1)?),
                end: (n(0)?, n(2)?),
            }),
            4 => Some(Span {
                start: (n(0)?, n(1)?),
                end: (n(2)?, n(3)?),
            }),
            _ => None,
        }
    }

    fn contains(&self, p: Pos) -> bool {
        self.start <= p && p < self.end
    }
}

/// One place a symbol is defined, and the node that stands for it there.
#[derive(Debug, Clone)]
struct Definition {
    /// Usually the symbol string itself. When one symbol has several
    /// definitions, each gets `symbol @file:line` so the nodes stay apart.
    node_id: String,
    file: String,
    /// 1-based line of the item's name.
    line: u32,
    /// The whole item, e.g. a function from `fn` to its closing brace.
    span: Option<Span>,
}

/// A definition that became a node, with its symbol taken apart.
struct Item {
    symbol: String,
    parsed: ParsedSymbol,
    def: Definition,
}

/// An impl block with its header resolved to nodes where possible.
struct ResolvedImpl {
    file: String,
    start_line: u32,
    end_line: u32,
    /// Node id of the self type.
    self_type: Option<String>,
    /// Node id of the trait, when the trait is defined in the workspace.
    workspace_trait: Option<String>,
}

/// Everything the build needs to look things up while it works.
struct Context<'a> {
    index: &'a Index,
    root: &'a Path,
    /// Every definition of every symbol. Most symbols have exactly one.
    definitions: HashMap<String, Vec<Definition>>,
    /// The definitions that became nodes.
    items: Vec<Item>,
    /// Package names in the workspace, normalized (`grep-cli` becomes `grep_cli`).
    workspace_packages: Vec<String>,
}

impl Context<'_> {
    /// Which definition a reference in `file` on `line` means, and how sure that is.
    ///
    /// - One definition: that one, EXACT.
    /// - Several, one of them in the same file: that one, EXACT.
    /// - Several in the same file: the nearest one above the reference, CANDIDATE.
    /// - Several, none in the same file: the one whose path shares the most
    ///   folders with `file`, CANDIDATE.
    fn resolve(&self, symbol: &str, file: &str, line: u32) -> Option<(&Definition, Confidence)> {
        let defs = self.definitions.get(symbol)?;
        if let [only] = defs.as_slice() {
            return Some((only, Confidence::Exact));
        }
        let same_file: Vec<&Definition> = defs.iter().filter(|d| d.file == file).collect();
        match same_file.as_slice() {
            [only] => Some((only, Confidence::Exact)),
            [] => {
                // Ties go to the alphabetically first file, so the result is stable.
                let best = defs
                    .iter()
                    .max_by_key(|d| (shared_folders(&d.file, file), Reverse(&d.file)))?;
                Some((best, Confidence::Candidate))
            }
            several => {
                let above = several
                    .iter()
                    .filter(|d| d.line <= line)
                    .max_by_key(|d| d.line);
                Some((above.copied().unwrap_or(several[0]), Confidence::Candidate))
            }
        }
    }

    /// Like [`Context::resolve`], but returns the node, if that item is one.
    fn resolve_node<'g>(
        &self,
        graph: &'g GraphBuilder,
        symbol: &str,
        file: &str,
        line: u32,
    ) -> Option<(&'g Node, Confidence)> {
        let (def, confidence) = self.resolve(symbol, file, line)?;
        Some((graph.node(&def.node_id)?, confidence))
    }
}

/// How many leading folders two paths share: `src/a/x.rs` and `src/a/y.rs` share 2.
fn shared_folders(a: &str, b: &str) -> usize {
    let folders = |p: &'_ str| -> Vec<String> {
        let mut parts: Vec<String> = p.split('/').map(str::to_string).collect();
        parts.pop(); // the file name
        parts
    };
    folders(a)
        .iter()
        .zip(folders(b).iter())
        .take_while(|(x, y)| x == y)
        .count()
}

/// Cargo writes `grep-cli`, Rust code says `grep_cli`. Compare them this way.
fn normalize(package: &str) -> String {
    package.replace('-', "_")
}

fn crate_id(package: &str) -> String {
    format!("crate:{}", normalize(package))
}

/// `crate/` is the crate's root module; the crate node stands for it.
fn is_crate_root(parsed: &ParsedSymbol) -> bool {
    parsed.shape == Shape::Module && parsed.modules.is_empty() && parsed.name == "crate"
}

/// Builds the graph. `produced_by` records the tool versions in the output.
pub fn build_graph(index: &Index, workspace: &Workspace, produced_by: String) -> Graph {
    let mut graph = GraphBuilder::default();
    let definitions = collect_definitions(index);
    graph.stats.duplicate_symbols = definitions
        .iter()
        .filter(|(symbol, defs)| {
            defs.len() > 1 && !symbols::parse(symbol).is_some_and(|p| is_crate_root(&p))
        })
        .count();
    let mut ctx = Context {
        index,
        root: &workspace.root,
        definitions,
        items: Vec::new(),
        workspace_packages: workspace
            .packages
            .iter()
            .map(|p| normalize(&p.name))
            .collect(),
    };

    add_crate_nodes(&mut graph, workspace);
    add_item_nodes(&mut graph, &mut ctx);

    // syn facts for every indexed file, parsed once.
    let mut facts: HashMap<String, FileFacts> = HashMap::new();
    for doc in &index.documents {
        match syntax::scan_file(&ctx.root.join(&doc.relative_path)) {
            Some(f) => {
                facts.insert(doc.relative_path.clone(), f);
            }
            None => graph.stats.files_unparsed_by_syn += 1,
        }
    }
    graph.stats.files_indexed = index.documents.len();

    // For each file, the symbols mentioned on each line: impl headers and
    // derives look up names this way.
    let mut by_line: HashMap<&str, SymbolsByLine> = HashMap::new();
    for doc in &index.documents {
        let lines = by_line.entry(doc.relative_path.as_str()).or_default();
        for occ in &doc.occurrences {
            if let Some(span) = Span::from_range(&occ.range) {
                lines
                    .entry(span.start.0 + 1)
                    .or_default()
                    .push(occ.symbol.as_str());
            }
        }
    }

    let impls = resolve_impl_blocks(&mut graph, &ctx, &facts, &by_line);
    let trait_method_impls = link_impl_methods(&mut graph, &ctx, &impls);
    add_containment(&mut graph, &ctx, &impls);
    let containers = containers_by_file(&graph, &ctx);
    for doc in &index.documents {
        let file_containers = containers
            .get(doc.relative_path.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        add_reference_edges(&mut graph, &ctx, doc, file_containers, &trait_method_impls);
    }
    add_derive_edges(&mut graph, &ctx, &facts, &by_line);
    mark_tests(&mut graph, &ctx, &facts, &by_line);
    graph.unsafe_sites = find_unsafe_owners(&graph, &ctx, &facts, &by_line);

    graph.finish(produced_by)
}

// ---------------------------------------------------------------------------
// Step 1: nodes
// ---------------------------------------------------------------------------

/// Finds every definition of every symbol, and gives each one its node id.
fn collect_definitions(index: &Index) -> HashMap<String, Vec<Definition>> {
    let mut out: HashMap<String, Vec<Definition>> = HashMap::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            let is_definition = occ.symbol_roles & SymbolRole::Definition as i32 != 0;
            if !is_definition || symbols::parse(&occ.symbol).is_none() {
                continue;
            }
            let Some(name_span) = Span::from_range(&occ.range) else {
                continue;
            };
            out.entry(occ.symbol.clone()).or_default().push(Definition {
                node_id: String::new(), // Set below, once we know how many there are.
                file: doc.relative_path.clone(),
                line: name_span.start.0 + 1,
                span: Span::from_range(&occ.enclosing_range),
            });
        }
    }
    for (symbol, defs) in &mut out {
        defs.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        // The same definition can be listed twice when two crates share a file.
        defs.dedup_by(|a, b| a.file == b.file && a.line == b.line);
        let only_one = defs.len() == 1;
        for def in defs.iter_mut() {
            def.node_id = if only_one {
                symbol.clone()
            } else {
                format!("{symbol} @{}:{}", def.file, def.line)
            };
        }
    }
    out
}

fn add_crate_nodes(graph: &mut GraphBuilder, workspace: &Workspace) {
    for package in &workspace.packages {
        graph.add_node(crate_node(&package.name));
    }
    for package in &workspace.packages {
        // Dev-dependencies are left out in version 1: they only matter for tests.
        for dep in package
            .dependencies
            .iter()
            .filter(|d| d.kind.as_deref() != Some("dev"))
        {
            graph.add_node(crate_node(&dep.name));
            graph.add_edge(edge(
                &crate_id(&package.name),
                &crate_id(&dep.name),
                EdgeKind::DependsOn,
                Confidence::Exact,
                None,
                None,
            ));
        }
    }
}

fn crate_node(name: &str) -> Node {
    Node {
        id: crate_id(name),
        name: name.to_string(),
        path: name.to_string(),
        kind: NodeKind::Crate,
        krate: name.to_string(),
        file: None,
        line: None,
        signature: None,
        test: false,
    }
}

/// Maps rust-analyzer's symbol kinds to ours. `None` means "not a node":
/// fields, enum variants, parameters, locals and the like.
fn node_kind(kind: Kind, parsed: &ParsedSymbol) -> Option<NodeKind> {
    let mapped = match kind {
        Kind::Function => NodeKind::Function,
        Kind::Method | Kind::StaticMethod => NodeKind::Method,
        Kind::TraitMethod => NodeKind::TraitMethod,
        Kind::Struct => NodeKind::Struct,
        Kind::Enum => NodeKind::Enum,
        Kind::Union => NodeKind::Union,
        Kind::Trait => NodeKind::Trait,
        Kind::TypeAlias => NodeKind::TypeAlias,
        Kind::Module | Kind::Namespace => NodeKind::Module,
        Kind::Constant => NodeKind::Const,
        Kind::StaticVariable => NodeKind::Static,
        Kind::Macro => NodeKind::Macro,
        // No kind recorded: fall back to the symbol's shape where it is unambiguous.
        Kind::UnspecifiedKind => match parsed.shape {
            Shape::Module => NodeKind::Module,
            Shape::Macro => NodeKind::Macro,
            Shape::Method if parsed.impl_header.is_some() || parsed.owner.is_some() => {
                NodeKind::Method
            }
            Shape::Method => NodeKind::Function,
            _ => return None,
        },
        _ => return None,
    };
    Some(mapped)
}

fn add_item_nodes(graph: &mut GraphBuilder, ctx: &mut Context) {
    // Kinds and signatures live in each document's symbol list.
    let mut info: HashMap<&str, (Kind, &str)> = HashMap::new();
    for doc in &ctx.index.documents {
        for si in &doc.symbols {
            let kind = si.kind.enum_value_or_default();
            let signature = si
                .signature_documentation
                .as_ref()
                .map(|d| d.text.as_str())
                .unwrap_or("");
            info.insert(si.symbol.as_str(), (kind, signature));
        }
    }

    // Sorted, so nodes are always created in the same order.
    let mut all_symbols: Vec<&String> = ctx.definitions.keys().collect();
    all_symbols.sort();
    for symbol in all_symbols {
        let Some(parsed) = symbols::parse(symbol) else {
            continue;
        };
        if matches!(parsed.shape, Shape::Impl | Shape::Other) || is_crate_root(&parsed) {
            continue;
        }
        let (kind, signature) = info
            .get(symbol.as_str())
            .copied()
            .unwrap_or((Kind::UnspecifiedKind, ""));
        let Some(kind) = node_kind(kind, &parsed) else {
            continue;
        };
        for def in &ctx.definitions[symbol] {
            graph.add_node(Node {
                id: def.node_id.clone(),
                name: parsed.display_name(),
                path: parsed.full_path(),
                kind,
                krate: parsed.package.clone(),
                file: Some(def.file.clone()),
                line: Some(def.line),
                signature: (!signature.is_empty()).then(|| signature.to_string()),
                test: false,
            });
            ctx.items.push(Item {
                symbol: symbol.clone(),
                parsed: parsed.clone(),
                def: def.clone(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Step 2: impl blocks
// ---------------------------------------------------------------------------

/// The node a name on a given line refers to, according to the index.
/// `want` filters by kind, so `Loader` the trait is not confused with a module.
fn resolve_name_on_line(
    graph: &GraphBuilder,
    ctx: &Context,
    on_line: Option<&SymbolsByLine>,
    file: &str,
    at: &NameAt,
    want: fn(NodeKind) -> bool,
) -> Option<String> {
    on_line?
        .get(&at.line)?
        .iter()
        .filter(|symbol| symbols::parse(symbol).is_some_and(|p| p.name == at.name))
        .filter_map(|symbol| ctx.resolve_node(graph, symbol, file, at.line))
        .find(|(node, _)| want(node.kind))
        .map(|(node, _)| node.id.clone())
}

fn resolve_impl_blocks(
    graph: &mut GraphBuilder,
    ctx: &Context,
    facts: &HashMap<String, FileFacts>,
    by_line: &HashMap<&str, SymbolsByLine>,
) -> Vec<ResolvedImpl> {
    let mut resolved = Vec::new();
    for doc in &ctx.index.documents {
        let file = doc.relative_path.as_str();
        let Some(file_facts) = facts.get(file) else {
            continue;
        };
        let on_line = by_line.get(file);
        for block in &file_facts.impls {
            let self_type = block.self_type.as_ref().and_then(|at| {
                resolve_name_on_line(graph, ctx, on_line, file, at, NodeKind::is_type_like)
            });
            let workspace_trait = block.trait_name.as_ref().and_then(|at| {
                resolve_name_on_line(graph, ctx, on_line, file, at, |k| k == NodeKind::Trait)
            });

            if let (Some(ty), Some(trait_at)) = (&self_type, &block.trait_name) {
                // A trait from outside the workspace (Display, Iterator...) has no
                // symbol in the index, so we only know its name: SYNTAX confidence.
                let (target, confidence) = match &workspace_trait {
                    Some(t) => (t.clone(), Confidence::Exact),
                    None => {
                        let id = format!("trait:{}", trait_at.name);
                        graph.add_node(named_node(&id, &trait_at.name, NodeKind::ExternalTrait));
                        (id, Confidence::Syntax)
                    }
                };
                graph.add_edge(edge(
                    ty,
                    &target,
                    EdgeKind::Implements,
                    confidence,
                    Some(file),
                    Some(block.start_line),
                ));
            }

            resolved.push(ResolvedImpl {
                file: file.to_string(),
                start_line: block.start_line,
                end_line: block.end_line,
                self_type,
                workspace_trait,
            });
        }
    }
    resolved
}

/// A node known only by name: derives and traits from outside the workspace.
fn named_node(id: &str, name: &str, kind: NodeKind) -> Node {
    Node {
        id: id.to_string(),
        name: name.to_string(),
        path: name.to_string(),
        kind,
        krate: String::new(),
        file: None,
        line: None,
        signature: None,
        test: false,
    }
}

/// The innermost impl block around a line of a file.
fn impl_around<'a>(impls: &'a [ResolvedImpl], file: &str, line: u32) -> Option<&'a ResolvedImpl> {
    impls
        .iter()
        .filter(|b| b.file == file && b.start_line <= line && line <= b.end_line)
        .max_by_key(|b| b.start_line)
}

/// Links each method in a trait impl to the trait method it implements.
/// Returns, for each trait method, the impl methods that could run when it is called.
fn link_impl_methods(
    graph: &mut GraphBuilder,
    ctx: &Context,
    impls: &[ResolvedImpl],
) -> HashMap<String, Vec<String>> {
    // Node id -> symbol, to name a trait's methods from the trait's own symbol.
    let symbol_of: HashMap<&str, &str> = ctx
        .items
        .iter()
        .map(|i| (i.def.node_id.as_str(), i.symbol.as_str()))
        .collect();

    let mut trait_method_impls: HashMap<String, Vec<String>> = HashMap::new();
    for item in &ctx.items {
        if item.parsed.impl_header.is_none() || item.parsed.shape != Shape::Method {
            continue;
        }
        let Some(block) = impl_around(impls, &item.def.file, item.def.line) else {
            continue;
        };
        let Some(trait_id) = &block.workspace_trait else {
            continue;
        };
        let (Some(trait_symbol), Some(trait_node)) =
            (symbol_of.get(trait_id.as_str()), graph.node(trait_id))
        else {
            continue;
        };
        // `...loader/Loader#` + `load().` names the trait's own `load`, defined next to the trait.
        let trait_method_symbol = format!("{trait_symbol}{}().", item.parsed.name);
        let trait_file = trait_node.file.clone().unwrap_or_default();
        let trait_line = trait_node.line.unwrap_or(0);
        let Some(trait_method) = ctx
            .resolve_node(graph, &trait_method_symbol, &trait_file, trait_line)
            .map(|(node, _)| node.id.clone())
        else {
            continue;
        };
        graph.add_edge(edge(
            &item.def.node_id,
            &trait_method,
            EdgeKind::Implements,
            Confidence::Exact,
            Some(&item.def.file),
            Some(item.def.line),
        ));
        trait_method_impls
            .entry(trait_method)
            .or_default()
            .push(item.def.node_id.clone());
    }
    for methods in trait_method_impls.values_mut() {
        methods.sort();
    }
    trait_method_impls
}

// ---------------------------------------------------------------------------
// Step 3: containment
// ---------------------------------------------------------------------------

fn add_containment(graph: &mut GraphBuilder, ctx: &Context, impls: &[ResolvedImpl]) {
    for item in &ctx.items {
        let def = &item.def;
        let parent = if item.parsed.impl_header.is_some() {
            // Methods belong to the impl's self type, which may live in another module.
            impl_around(impls, &def.file, def.line).and_then(|b| b.self_type.clone())
        } else {
            match symbols::parent(&item.symbol) {
                Some(p) if symbols::parse(&p).is_some_and(|pp| is_crate_root(&pp)) => {
                    Some(crate_id(&item.parsed.package))
                }
                Some(p) => ctx
                    .resolve_node(graph, &p, &def.file, def.line)
                    .map(|(node, _)| node.id.clone()),
                None => Some(crate_id(&item.parsed.package)),
            }
        };
        if let Some(parent) = parent.filter(|p| graph.has_node(p) && *p != def.node_id) {
            graph.add_edge(edge(
                &parent,
                &def.node_id,
                EdgeKind::Contains,
                Confidence::Exact,
                Some(&def.file),
                Some(def.line),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Step 4: references
// ---------------------------------------------------------------------------

/// For each file, the functions and types defined there, with their full spans.
fn containers_by_file<'c>(
    graph: &GraphBuilder,
    ctx: &'c Context,
) -> HashMap<&'c str, Vec<(Span, &'c str)>> {
    let mut out: HashMap<&str, Vec<(Span, &str)>> = HashMap::new();
    for item in &ctx.items {
        let Some(span) = item.def.span else {
            continue;
        };
        let is_container = graph
            .node(&item.def.node_id)
            .is_some_and(|n| n.kind.is_callable() || n.kind.is_type_like());
        if is_container {
            out.entry(item.def.file.as_str())
                .or_default()
                .push((span, item.def.node_id.as_str()));
        }
    }
    out
}

/// Converts a character offset from the index into a byte index into `line`.
fn byte_index(line: &str, offset: u32, encoding: PositionEncoding) -> usize {
    let offset = offset as usize;
    match encoding {
        PositionEncoding::UTF8CodeUnitOffsetFromLineStart => {
            let mut i = offset.min(line.len());
            while !line.is_char_boundary(i) {
                i -= 1;
            }
            i
        }
        PositionEncoding::UTF16CodeUnitOffsetFromLineStart => {
            let mut units = 0;
            for (i, ch) in line.char_indices() {
                if units >= offset {
                    return i;
                }
                units += ch.len_utf16();
            }
            line.len()
        }
        _ => line
            .char_indices()
            .nth(offset)
            .map(|(i, _)| i)
            .unwrap_or(line.len()),
    }
}

/// The source text right after a name, skipping spaces. Moves to the next
/// line when the name ends its line, for code formatted as `name\n(`.
fn text_after<'s>(lines: &[&'s str], end: Pos, encoding: PositionEncoding) -> &'s str {
    let Some(line) = lines.get(end.0 as usize) else {
        return "";
    };
    let rest = line[byte_index(line, end.1, encoding)..].trim_start();
    if rest.is_empty() {
        lines
            .get(end.0 as usize + 1)
            .map(|l| l.trim_start())
            .unwrap_or("")
    } else {
        rest
    }
}

/// `name(` and `name::<T>(` are calls. `map(Self::parse)` only names `parse`.
fn is_call(after: &str) -> bool {
    after.starts_with('(') || after.starts_with("::<")
}

fn add_reference_edges(
    graph: &mut GraphBuilder,
    ctx: &Context,
    doc: &Document,
    containers: &[(Span, &str)],
    trait_method_impls: &HashMap<String, Vec<String>>,
) {
    let file = doc.relative_path.as_str();
    let source = std::fs::read_to_string(ctx.root.join(file)).unwrap_or_default();
    let lines: Vec<&str> = source.lines().collect();
    let encoding = doc.position_encoding.enum_value_or_default();

    for occ in &doc.occurrences {
        if occ.symbol_roles & SymbolRole::Definition as i32 != 0 {
            continue;
        }
        let Some(span) = Span::from_range(&occ.range) else {
            continue;
        };
        let line = span.start.0 + 1;
        let Some((target_id, target_kind, confidence)) = ctx
            .resolve_node(graph, &occ.symbol, file, line)
            .map(|(node, c)| (node.id.clone(), node.kind, c))
        else {
            // Not a node. Count it if it points outside the workspace.
            let outside = symbols::parse(&occ.symbol)
                .is_some_and(|p| !ctx.workspace_packages.contains(&normalize(&p.package)));
            if outside {
                graph.stats.external_references += 1;
            }
            continue;
        };
        // The innermost container: the one that starts last among those that contain it.
        let Some(from) = containers
            .iter()
            .filter(|(s, _)| s.contains(span.start))
            .max_by_key(|(s, _)| s.start)
            .map(|(_, id)| id.to_string())
        else {
            graph.stats.unattributed_references += 1;
            continue;
        };

        let after = text_after(&lines, span.end, encoding);
        let kind = match target_kind {
            k if k.is_callable() && is_call(after) => EdgeKind::Calls,
            k if k.is_callable() => EdgeKind::References,
            NodeKind::Macro if after.starts_with('!') => EdgeKind::Calls,
            k if k.is_type_like() && from != target_id => EdgeKind::UsesType,
            NodeKind::Const | NodeKind::Static | NodeKind::Macro => EdgeKind::References,
            _ => continue,
        };
        graph.add_edge(edge(
            &from,
            &target_id,
            kind,
            confidence,
            Some(file),
            Some(line),
        ));
        // A call to a trait method may run any impl of it.
        if kind == EdgeKind::Calls {
            for candidate in trait_method_impls.get(&target_id).into_iter().flatten() {
                graph.add_edge(edge(
                    &from,
                    candidate,
                    EdgeKind::MayCall,
                    Confidence::Candidate,
                    Some(file),
                    Some(line),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Step 5: derives
// ---------------------------------------------------------------------------

fn add_derive_edges(
    graph: &mut GraphBuilder,
    ctx: &Context,
    facts: &HashMap<String, FileFacts>,
    by_line: &HashMap<&str, SymbolsByLine>,
) {
    for doc in &ctx.index.documents {
        let file = doc.relative_path.as_str();
        let Some(file_facts) = facts.get(file) else {
            continue;
        };
        for type_derives in &file_facts.derives {
            let Some(type_id) = resolve_name_on_line(
                graph,
                ctx,
                by_line.get(file),
                file,
                &type_derives.type_name,
                NodeKind::is_type_like,
            ) else {
                continue;
            };
            for derive in &type_derives.derives {
                let id = format!("derive:{derive}");
                graph.add_node(named_node(&id, derive, NodeKind::Derive));
                graph.add_edge(edge(
                    &type_id,
                    &id,
                    EdgeKind::Derives,
                    Confidence::Syntax,
                    Some(file),
                    Some(type_derives.type_name.line),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Step 6: tests and unsafe code
// ---------------------------------------------------------------------------

fn mark_tests(
    graph: &mut GraphBuilder,
    ctx: &Context,
    facts: &HashMap<String, FileFacts>,
    by_line: &HashMap<&str, SymbolsByLine>,
) {
    for doc in &ctx.index.documents {
        let file = doc.relative_path.as_str();
        let Some(file_facts) = facts.get(file) else {
            continue;
        };
        for test in &file_facts.tests {
            let found = resolve_name_on_line(
                graph,
                ctx,
                by_line.get(file),
                file,
                test,
                NodeKind::is_callable,
            );
            if let Some(node) = found.and_then(|id| graph.node_mut(&id)) {
                node.test = true;
            }
        }
    }
}

/// Every unsafe site, with the node it belongs to:
///
/// - a block: the innermost function (or const or static) around it;
/// - an `unsafe fn` or trait: itself;
/// - an `unsafe impl`: its self type, so `RawBuf` lists `unsafe impl Send for RawBuf`.
fn find_unsafe_owners(
    graph: &GraphBuilder,
    ctx: &Context,
    facts: &HashMap<String, FileFacts>,
    by_line: &HashMap<&str, SymbolsByLine>,
) -> Vec<UnsafeSite> {
    // Items that can hold an `unsafe` block, with their spans, by file.
    let mut holders: HashMap<&str, Vec<(Span, &str)>> = HashMap::new();
    for item in &ctx.items {
        let can_hold_code = graph.node(&item.def.node_id).is_some_and(|n| {
            n.kind.is_callable() || matches!(n.kind, NodeKind::Const | NodeKind::Static)
        });
        if let (true, Some(span)) = (can_hold_code, item.def.span) {
            holders
                .entry(item.def.file.as_str())
                .or_default()
                .push((span, item.def.node_id.as_str()));
        }
    }

    let mut sites = Vec::new();
    for doc in &ctx.index.documents {
        let file = doc.relative_path.as_str();
        let Some(file_facts) = facts.get(file) else {
            continue;
        };
        let on_line = by_line.get(file);
        for code in &file_facts.unsafe_code {
            let named = |want: fn(NodeKind) -> bool| {
                let at = code.name.as_ref()?;
                resolve_name_on_line(graph, ctx, on_line, file, at, want)
            };
            let item = match code.kind {
                UnsafeKind::Block => {
                    // syn counts columns in characters and the index may count
                    // bytes or UTF-16 units; they agree on ASCII lines, and the
                    // column only matters when two items share a line.
                    let position = (code.line - 1, code.column);
                    holders
                        .get(file)
                        .into_iter()
                        .flatten()
                        .filter(|(span, _)| span.contains(position))
                        .max_by_key(|(span, _)| span.start)
                        .map(|(_, id)| id.to_string())
                }
                UnsafeKind::Fn => named(NodeKind::is_callable),
                UnsafeKind::Trait => named(|k| k == NodeKind::Trait),
                UnsafeKind::Impl => named(NodeKind::is_type_like),
            };
            sites.push(UnsafeSite {
                file: file.to_string(),
                line: code.line,
                kind: code.kind,
                item,
                trait_name: code.trait_name.clone(),
                documented: code.documented,
                in_trait_impl: code.in_trait_impl,
            });
        }
    }
    sites
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_contain_positions_up_to_the_end() {
        let span = Span::from_range(&[8, 0, 10, 1]).unwrap();
        assert!(span.contains((9, 4)));
        assert!(!span.contains((10, 1)));
        assert!(Span::from_range(&[1, 2]).is_none());
    }

    #[test]
    fn detects_call_syntax() {
        let lines = [
            "    let rows = r.parse();",
            "    x.map(Self::parse)",
            "    v.collect::<Vec<_>>()",
            "    twice!(1)",
        ];
        let utf8 = PositionEncoding::UTF8CodeUnitOffsetFromLineStart;
        assert!(is_call(text_after(&lines, (0, 22), utf8))); // after `parse`
        assert!(!is_call(text_after(&lines, (1, 21), utf8))); // `parse)` is not a call
        assert!(is_call(text_after(&lines, (2, 13), utf8))); // `collect::<`
        assert!(text_after(&lines, (3, 9), utf8).starts_with('!')); // a macro call
    }

    #[test]
    fn converts_utf16_offsets() {
        // "é" is 1 UTF-16 unit but 2 UTF-8 bytes.
        assert_eq!(
            byte_index(
                "é(x)",
                1,
                PositionEncoding::UTF16CodeUnitOffsetFromLineStart
            ),
            2
        );
    }

    #[test]
    fn counts_shared_folders() {
        assert_eq!(shared_folders("src/a/x.rs", "src/a/y.rs"), 2);
        assert_eq!(shared_folders("build.rs", "src/main.rs"), 0);
    }

    fn def(file: &str, line: u32) -> Definition {
        Definition {
            node_id: format!("s @{file}:{line}"),
            file: file.to_string(),
            line,
            span: None,
        }
    }

    #[test]
    fn duplicate_symbols_resolve_to_the_right_definition() {
        let index = Index::new();
        let ctx = Context {
            index: &index,
            root: Path::new("."),
            definitions: HashMap::from([(
                "s".to_string(),
                vec![
                    def("build.rs", 1),
                    def("src/main.rs", 4),
                    def("src/lib.rs", 10),
                    def("src/lib.rs", 30),
                ],
            )]),
            items: Vec::new(),
            workspace_packages: Vec::new(),
        };
        let pick = |file: &str, line| {
            let (d, c) = ctx.resolve("s", file, line).unwrap();
            (d.file.clone(), d.line, c)
        };
        // One definition in the same file: certain.
        assert_eq!(
            pick("src/main.rs", 9),
            ("src/main.rs".into(), 4, Confidence::Exact)
        );
        // Two in the same file: the nearest one above, flagged as a guess.
        assert_eq!(
            pick("src/lib.rs", 35),
            ("src/lib.rs".into(), 30, Confidence::Candidate)
        );
        assert_eq!(
            pick("src/lib.rs", 12),
            ("src/lib.rs".into(), 10, Confidence::Candidate)
        );
        // None in the same file: the closest folder wins, flagged as a guess.
        assert_eq!(pick("src/other.rs", 1).2, Confidence::Candidate);
        assert_eq!(pick("src/other.rs", 1).0, "src/lib.rs");
    }
}
