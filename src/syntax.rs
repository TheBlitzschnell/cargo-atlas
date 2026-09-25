//! A light pass over the source text with `syn`.
//!
//! rust-analyzer's index leaves out two facts the graph needs:
//!
//! - Which trait an impl block implements, exactly. The index names impl
//!   methods like `impl#[CsvReader][Loader]load().`, with short names only,
//!   and two different traits can both be called `Loader`.
//! - `#[derive(...)]` lists. Derived impls come from macros and never appear
//!   in the index.
//!
//! syn finds where impl headers and derives are. The builder then asks the
//! index which exact symbol the name on that line refers to.

use std::path::Path;

use syn::spanned::Spanned;
use syn::visit::Visit;

/// A name and the 1-based line it sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameAt {
    pub name: String,
    pub line: u32,
}

/// One `impl ... { }` block.
#[derive(Debug, Clone)]
pub struct ImplBlock {
    /// First and last line of the whole block, 1-based.
    pub start_line: u32,
    pub end_line: u32,
    /// `CsvReader` in `impl Loader for CsvReader`. `None` for shapes we don't read, like tuples.
    pub self_type: Option<NameAt>,
    /// `Loader` in `impl Loader for CsvReader`. `None` for inherent impls.
    pub trait_name: Option<NameAt>,
}

/// The derives on one struct, enum or union.
#[derive(Debug, Clone)]
pub struct TypeDerives {
    pub type_name: NameAt,
    /// Last path segment of each derive: `serde::Serialize` becomes `Serialize`.
    pub derives: Vec<String>,
}

/// Everything the pass found in one file.
#[derive(Debug, Default)]
pub struct FileFacts {
    pub impls: Vec<ImplBlock>,
    pub derives: Vec<TypeDerives>,
}

/// Parses one file. Returns `None` if it can't be read or isn't valid Rust.
pub fn scan_file(path: &Path) -> Option<FileFacts> {
    let source = std::fs::read_to_string(path).ok()?;
    let file = syn::parse_file(&source).ok()?;
    let mut collector = Collector::default();
    collector.visit_file(&file);
    Some(collector.facts)
}

#[derive(Default)]
struct Collector {
    facts: FileFacts,
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let span = item.span();
        self.facts.impls.push(ImplBlock {
            start_line: span.start().line as u32,
            end_line: span.end().line as u32,
            self_type: last_name_in_type(&item.self_ty),
            trait_name: item
                .trait_
                .as_ref()
                .and_then(|(_, path, _)| path.segments.last())
                .map(|segment| name_at(&segment.ident)),
        });
        // Keep walking: impl blocks can hide inside functions inside impl blocks.
        syn::visit::visit_item_impl(self, item);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.record_derives(&item.attrs, &item.ident);
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.record_derives(&item.attrs, &item.ident);
        syn::visit::visit_item_enum(self, item);
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        self.record_derives(&item.attrs, &item.ident);
        syn::visit::visit_item_union(self, item);
    }
}

impl Collector {
    fn record_derives(&mut self, attrs: &[syn::Attribute], ident: &syn::Ident) {
        let derives = derive_names(attrs);
        if !derives.is_empty() {
            self.facts.derives.push(TypeDerives {
                type_name: name_at(ident),
                derives,
            });
        }
    }
}

fn name_at(ident: &syn::Ident) -> NameAt {
    NameAt {
        name: ident.to_string(),
        line: ident.span().start().line as u32,
    }
}

/// `CsvReader` for `CsvReader`, `crate::csv::CsvReader`, `&CsvReader` or `Vec<CsvReader>`'s outer `Vec`.
fn last_name_in_type(ty: &syn::Type) -> Option<NameAt> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| name_at(&s.ident)),
        syn::Type::Reference(r) => last_name_in_type(&r.elem),
        syn::Type::Paren(p) => last_name_in_type(&p.elem),
        syn::Type::Group(g) => last_name_in_type(&g.elem),
        _ => None,
    }
}

/// Reads `#[derive(Debug, Clone, serde::Serialize)]` into `["Debug", "Clone", "Serialize"]`.
fn derive_names(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut names = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("derive")) {
        // A malformed derive list is skipped rather than failing the whole file.
        let _ = attr.parse_nested_meta(|meta| {
            if let Some(segment) = meta.path.segments.last() {
                names.push(segment.ident.to_string());
            }
            Ok(())
        });
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(source: &str) -> FileFacts {
        let file = syn::parse_file(source).expect("test source should parse");
        let mut collector = Collector::default();
        collector.visit_file(&file);
        collector.facts
    }

    #[test]
    fn finds_trait_impl_header_names_and_lines() {
        let facts = scan("struct A;\ntrait T {}\n\nimpl T for A {\n}\n");
        let block = &facts.impls[0];
        assert_eq!((block.start_line, block.end_line), (4, 5));
        assert_eq!(
            block.trait_name,
            Some(NameAt {
                name: "T".into(),
                line: 4
            })
        );
        assert_eq!(
            block.self_type,
            Some(NameAt {
                name: "A".into(),
                line: 4
            })
        );
    }

    #[test]
    fn inherent_impl_has_no_trait() {
        let facts = scan("struct A;\nimpl A { fn f(&self) {} }\n");
        assert_eq!(facts.impls[0].trait_name, None);
    }

    #[test]
    fn reads_derive_lists() {
        let facts = scan("#[derive(Debug, Clone)]\n#[derive(serde::Serialize)]\nstruct A;\n");
        assert_eq!(
            facts.derives[0].derives,
            vec!["Debug", "Clone", "Serialize"]
        );
        assert_eq!(facts.derives[0].type_name.line, 3);
    }
}
