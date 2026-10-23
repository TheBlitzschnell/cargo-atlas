//! A light pass over the source text with `syn`.
//!
//! rust-analyzer's index leaves out facts the graph needs:
//!
//! - Which trait an impl block implements, exactly. The index names impl
//!   methods like `impl#[CsvReader][Loader]load().`, with short names only,
//!   and two different traits can both be called `Loader`.
//! - `#[derive(...)]` lists. Derived impls come from macros and never appear
//!   in the index.
//! - Attributes in general, so which functions are tests.
//! - Where `unsafe` code is, and whether each piece has the comment that
//!   explains why it is sound. Comments aren't in syn's tree either, so that
//!   check reads the lines around the `unsafe` keyword.
//!
//! - Out-of-line `mod name;` declarations, so each file can be traced to the
//!   Cargo target (library, binary, integration test...) that builds it.
//! - Calls written in a `macro_rules!` body. The index has no occurrence for
//!   them: the body is a template, and the expanded code points back into it
//!   without a token of its own at the place the macro is used.
//!
//! syn finds where these are. The builder then asks the index which exact
//! symbol the name on that line refers to.
//!
//! syn leaves the inside of a macro call as raw tokens. Many macros wrap
//! plain Rust (tokio's `cfg_rt! { ... }`, `vec![...]`), so the pass parses
//! the inside as items, statements or arguments, and walks it when that
//! works. A `macro_rules!` body is a template, not code, and is skipped; so
//! is anything that isn't plain Rust, such as `tokio::select!` arms.

use std::path::Path;

use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::model::UnsafeKind;

/// A name and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameAt {
    pub name: String,
    /// 1-based line.
    pub line: u32,
    /// 0-based column in bytes, the unit rust-analyzer's index uses. It tells
    /// apart two names on one line, as in `impl fmt::Write for W<dyn io::Write>`.
    pub column: u32,
}

/// A function with a test attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFn {
    pub name: NameAt,
    /// `#[rstest]`, `#[test_case(...)]`: one test per case, each named after the function.
    pub parameterized: bool,
}

/// A `macro_rules!` definition and the calls written in its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroBody {
    pub name: NameAt,
    pub calls: Vec<MacroCall>,
}

/// `$crate::log::write_line(...)` in a macro body: the path, without a
/// leading `$crate`, `crate`, `self` or `super`, and the line it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroCall {
    pub path: Vec<String>,
    pub line: u32,
}

/// An out-of-line module declaration, `mod name;`, whose code is in another file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDecl {
    pub name: String,
    /// Inline modules around the declaration, outermost first:
    /// `mod a { mod b; }` gives `["a"]` for `b`.
    pub inline_path: Vec<String>,
    /// The `#[path = "..."]` attribute, if any.
    pub path_attr: Option<String>,
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

/// One `unsafe` block, fn, impl or trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafeCode {
    pub kind: UnsafeKind,
    /// 1-based line of the `unsafe` keyword.
    pub line: u32,
    /// 0-based column of the `unsafe` keyword, counted in characters.
    pub column: u32,
    /// The name of the fn or trait, or the self type of an impl. `None` for blocks.
    pub name: Option<NameAt>,
    /// The trait in an `unsafe impl`, e.g. `Send`.
    pub trait_name: Option<String>,
    /// Blocks and impls: a `// SAFETY:` comment above. Fns and traits: docs
    /// with a `# Safety` section or a `Safety:` line.
    pub documented: bool,
    /// An `unsafe fn` in a trait impl: the trait documents its contract.
    pub in_trait_impl: bool,
}

/// Everything the pass found in one file.
#[derive(Debug, Default)]
pub struct FileFacts {
    pub impls: Vec<ImplBlock>,
    pub derives: Vec<TypeDerives>,
    /// Functions with a test attribute such as `#[test]` or `#[tokio::test]`.
    pub tests: Vec<TestFn>,
    pub unsafe_code: Vec<UnsafeCode>,
    pub mods: Vec<ModDecl>,
    pub macro_bodies: Vec<MacroBody>,
}

/// Parses one file. Returns `None` if it can't be read or isn't valid Rust.
pub fn scan_file(path: &Path) -> Option<FileFacts> {
    let source = std::fs::read_to_string(path).ok()?;
    scan_source(&source)
}

/// Parses source text. Returns `None` if it isn't valid Rust.
pub fn scan_source(source: &str) -> Option<FileFacts> {
    let file = syn::parse_file(source).ok()?;
    let mut collector = Collector {
        lines: source.lines().collect(),
        statement_lines: Vec::new(),
        impl_has_trait: Vec::new(),
        inline_mods: Vec::new(),
        facts: FileFacts::default(),
    };
    collector.visit_file(&file);
    Some(collector.facts)
}

struct Collector<'s> {
    /// The source, for reading comments, which syn drops.
    lines: Vec<&'s str>,
    /// First line of each statement the walk is inside, innermost last.
    statement_lines: Vec<u32>,
    /// For each impl block the walk is inside, innermost last: whether it
    /// implements a trait.
    impl_has_trait: Vec<bool>,
    /// Inline modules (`mod a { ... }`) the walk is inside, outermost first.
    inline_mods: Vec<String>,
    facts: FileFacts,
}

impl<'ast> Visit<'ast> for Collector<'_> {
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let span = item.span();
        let self_type = self.last_name_in_type(&item.self_ty);
        let trait_name = item
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last())
            .map(|segment| self.name_at(&segment.ident));
        if let Some(token) = &item.unsafety {
            let (line, column) = line_column(token.span);
            self.facts.unsafe_code.push(UnsafeCode {
                kind: UnsafeKind::Impl,
                line,
                column,
                name: self_type.clone(),
                trait_name: trait_name.as_ref().map(|t| t.name.clone()),
                documented: self.has_safety_comment(line, column),
                in_trait_impl: false,
            });
        }
        self.facts.impls.push(ImplBlock {
            start_line: span.start().line as u32,
            end_line: span.end().line as u32,
            self_type,
            trait_name,
        });
        // Keep walking: impl blocks can hide inside functions inside impl blocks.
        self.impl_has_trait.push(item.trait_.is_some());
        syn::visit::visit_item_impl(self, item);
        self.impl_has_trait.pop();
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

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if let Some(parameterized) = test_attribute(&item.attrs) {
            self.facts.tests.push(TestFn {
                name: self.name_at(&item.sig.ident),
                parameterized,
            });
        }
        self.record_unsafe_fn(&item.attrs, &item.sig, false);
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let name = item.ident.to_string();
        if item.content.is_some() {
            // `mod a { ... }`: out-of-line modules declared inside live in `a/`.
            self.inline_mods.push(name);
            syn::visit::visit_item_mod(self, item);
            self.inline_mods.pop();
        } else {
            self.facts.mods.push(ModDecl {
                name,
                inline_path: self.inline_mods.clone(),
                path_attr: path_attribute(&item.attrs),
            });
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let in_trait_impl = self.impl_has_trait.last() == Some(&true);
        self.record_unsafe_fn(&item.attrs, &item.sig, in_trait_impl);
        syn::visit::visit_impl_item_fn(self, item);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        self.record_unsafe_fn(&item.attrs, &item.sig, false);
        syn::visit::visit_trait_item_fn(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if let Some(token) = &item.unsafety {
            let (line, column) = line_column(token.span);
            self.facts.unsafe_code.push(UnsafeCode {
                kind: UnsafeKind::Trait,
                line,
                column,
                name: Some(self.name_at(&item.ident)),
                trait_name: None,
                documented: has_safety_section(&item.attrs)
                    || self.has_safety_comment(line, column),
                in_trait_impl: false,
            });
        }
        syn::visit::visit_item_trait(self, item);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.walk_macro_contents(mac);
    }

    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if let (true, Some(ident)) = (item.mac.path.is_ident("macro_rules"), &item.ident) {
            self.facts.macro_bodies.push(MacroBody {
                name: self.name_at(ident),
                calls: transcriber_calls(item.mac.tokens.clone()),
            });
        }
        syn::visit::visit_item_macro(self, item);
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        self.statement_lines.push(stmt.span().start().line as u32);
        syn::visit::visit_stmt(self, stmt);
        self.statement_lines.pop();
    }

    fn visit_expr_unsafe(&mut self, block: &'ast syn::ExprUnsafe) {
        let (line, column) = line_column(block.unsafe_token.span);
        // The comment may also sit above the statement the block is part of:
        //
        //     // SAFETY: the second arm only runs when `bytes` is not empty.
        //     match bytes {
        //         [] => 0,
        //         [_, ..] => unsafe { first_unchecked(bytes) },
        //     }
        let above_statement = self
            .statement_lines
            .last()
            .is_some_and(|&start| start != line && self.has_safety_comment(start, 0));
        self.facts.unsafe_code.push(UnsafeCode {
            kind: UnsafeKind::Block,
            line,
            column,
            name: None,
            trait_name: None,
            documented: above_statement || self.has_safety_comment(line, column),
            in_trait_impl: false,
        });
        syn::visit::visit_expr_unsafe(self, block);
    }
}

impl Collector<'_> {
    /// Walks the inside of a macro call when it parses as plain Rust.
    fn walk_macro_contents(&mut self, mac: &syn::Macro) {
        let name = mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        // Templates and quoted code for other crates, not code of this one.
        let skipped = [
            "macro_rules",
            "quote",
            "quote_spanned",
            "parse_quote",
            "parse_quote_spanned",
        ];
        if skipped.contains(&name.as_str()) {
            return;
        }
        // The parsed tree only lives inside this function, shorter than the
        // file's tree; `Visit` is implemented for every lifetime, so the
        // calls below walk it with its own.
        if let Ok(file) = mac.parse_body::<syn::File>() {
            self.visit_file(&file);
        } else if let Ok(statements) = mac.parse_body_with(syn::Block::parse_within) {
            for statement in &statements {
                self.visit_stmt(statement);
            }
        } else if let Ok(arguments) =
            mac.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        {
            for argument in &arguments {
                self.visit_expr(argument);
            }
        }
    }

    fn record_derives(&mut self, attrs: &[syn::Attribute], ident: &syn::Ident) {
        let derives = derive_names(attrs);
        if !derives.is_empty() {
            self.facts.derives.push(TypeDerives {
                type_name: self.name_at(ident),
                derives,
            });
        }
    }

    /// An identifier and its position, with the column in bytes.
    fn name_at(&self, ident: &syn::Ident) -> NameAt {
        let start = ident.span().start();
        // proc-macro2 counts columns in characters; the index counts bytes.
        let column = self
            .lines
            .get(start.line.saturating_sub(1))
            .and_then(|text| text.char_indices().nth(start.column))
            .map_or(start.column, |(byte, _)| byte);
        NameAt {
            name: ident.to_string(),
            line: start.line as u32,
            column: column as u32,
        }
    }

    /// `CsvReader` for `CsvReader`, `crate::csv::CsvReader`, `&CsvReader` or `Vec<CsvReader>`'s outer `Vec`.
    fn last_name_in_type(&self, ty: &syn::Type) -> Option<NameAt> {
        match ty {
            syn::Type::Path(p) => p.path.segments.last().map(|s| self.name_at(&s.ident)),
            syn::Type::Reference(r) => self.last_name_in_type(&r.elem),
            syn::Type::Paren(p) => self.last_name_in_type(&p.elem),
            syn::Type::Group(g) => self.last_name_in_type(&g.elem),
            _ => None,
        }
    }

    /// `in_trait_impl`: the fn implements a trait's `unsafe fn`, whose
    /// contract is written on the trait, so it needs no section of its own.
    fn record_unsafe_fn(
        &mut self,
        attrs: &[syn::Attribute],
        sig: &syn::Signature,
        in_trait_impl: bool,
    ) {
        if let Some(token) = &sig.unsafety {
            let (line, column) = line_column(token.span);
            self.facts.unsafe_code.push(UnsafeCode {
                kind: UnsafeKind::Fn,
                line,
                column,
                name: Some(self.name_at(&sig.ident)),
                trait_name: None,
                documented: in_trait_impl
                    || has_safety_section(attrs)
                    || self.has_safety_comment(line, column),
                in_trait_impl,
            });
        }
    }

    /// Whether a comment containing `SAFETY:` explains the code whose
    /// `unsafe` keyword is at `line` (1-based) and `column`.
    ///
    /// Like clippy's `undocumented_unsafe_blocks`, this accepts a run of
    /// comment lines right above, with blank lines and attributes allowed in
    /// between, and matches `SAFETY:` in any case. It also accepts a block
    /// comment before the keyword on the same line: `/* SAFETY: ... */ unsafe {`.
    fn has_safety_comment(&self, line: u32, column: u32) -> bool {
        let Some(index) = (line as usize).checked_sub(1) else {
            return false;
        };
        let before: String = self
            .lines
            .get(index)
            .map(|text| text.chars().take(column as usize).collect())
            .unwrap_or_default();
        if before.contains("/*") && mentions_safety(&before) {
            return true;
        }
        let mut in_block_comment = false;
        for text in self.lines[..index.min(self.lines.len())].iter().rev() {
            let text = text.trim();
            if in_block_comment {
                if mentions_safety(text) {
                    return true;
                }
                in_block_comment = !text.starts_with("/*");
                continue;
            }
            if text.is_empty() || text.starts_with("#[") {
                continue;
            }
            if text.starts_with("//") || text.ends_with("*/") {
                if mentions_safety(text) {
                    return true;
                }
                // The last line of a block comment that started higher up.
                in_block_comment = text.ends_with("*/") && !text.starts_with("/*");
                continue;
            }
            // Code: the comment, if any, belongs to something else.
            return false;
        }
        false
    }
}

/// `SAFETY:` anywhere in the comment, in any case, or a `# Safety` heading
/// (`// # Safety`, `/// # Safety`), which some code uses instead.
fn mentions_safety(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    if upper.contains("SAFETY:") {
        return true;
    }
    let body =
        upper.trim_start_matches(|c: char| matches!(c, '/' | '*' | '!') || c.is_whitespace());
    body.starts_with('#') && body.trim_start_matches('#').trim() == "SAFETY"
}

/// Whether the doc comment covers safety: a `# Safety` section at any
/// heading level, or a line with `Safety:`. Reading the attributes as well as
/// the comment lines catches docs written as `#[doc = "..."]`.
fn has_safety_section(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(string_value)
        .any(|text| text.lines().any(mentions_safety))
}

/// The string in an attribute written `#[name = "..."]`, such as one `///`
/// line, which syn sees as `#[doc = "..."]`, or `#[path = "..."]`.
fn string_value(attr: &syn::Attribute) -> Option<String> {
    let syn::Meta::NameValue(name_value) = &attr.meta else {
        return None;
    };
    let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(text),
        ..
    }) = &name_value.value
    else {
        return None;
    };
    Some(text.value())
}

/// Whether a function is a test, from its attributes: `Some(false)` for one
/// that runs once, such as `#[test]`, `#[tokio::test]`, `#[sqlx::test]` or
/// `#[wasm_bindgen_test]`; `Some(true)` for one that runs once per case,
/// `#[rstest]`, `#[test_case(...)]` or `#[test_matrix(...)]`. `#[cfg(test)]`
/// is not a test attribute: it marks code that only exists in test builds.
fn test_attribute(attrs: &[syn::Attribute]) -> Option<bool> {
    let mut found = None;
    for attr in attrs {
        let Some(last) = attr.path().segments.last() else {
            continue;
        };
        let name = last.ident.to_string();
        if matches!(name.as_str(), "rstest" | "test_case" | "test_matrix") {
            return Some(true);
        }
        if name == "test" || name.ends_with("_test") || name == "quickcheck" {
            found = Some(false);
        }
    }
    found
}

/// The file named by `#[path = "..."]`, which overrides where a module's code is.
fn path_attribute(attrs: &[syn::Attribute]) -> Option<String> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("path"))
        .find_map(string_value)
}

/// The calls in a `macro_rules!` body's rules, `(matcher) => { transcriber };`.
/// Only transcribers are code; a matcher may hold literal tokens like `call(`.
fn transcriber_calls(body: proc_macro2::TokenStream) -> Vec<MacroCall> {
    use proc_macro2::TokenTree;
    let trees: Vec<TokenTree> = body.into_iter().collect();
    let mut calls = Vec::new();
    for window in trees.windows(3) {
        let [
            TokenTree::Punct(eq),
            TokenTree::Punct(gt),
            TokenTree::Group(transcriber),
        ] = window
        else {
            continue;
        };
        if eq.as_char() == '=' && gt.as_char() == '>' {
            calls_in_tokens(transcriber.stream(), &mut calls);
        }
    }
    calls
}

/// Calls written in a macro body: a name or path followed by `(...)`, such
/// as `write_line(...)` or `$crate::log::write_line(...)`. Left out: method
/// calls (`.name(...)`), whose target depends on a type the tokens don't
/// show; macro variables (`$f(...)`); and the names after `fn`, `struct`,
/// `enum` or `union`, which define rather than call.
fn calls_in_tokens(tokens: proc_macro2::TokenStream, calls: &mut Vec<MacroCall>) {
    use proc_macro2::{Delimiter, TokenTree};
    let punct = |tree: Option<&TokenTree>, ch: char| matches!(tree, Some(TokenTree::Punct(p)) if p.as_char() == ch);
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    while i < trees.len() {
        let ident = match &trees[i] {
            TokenTree::Group(group) => {
                calls_in_tokens(group.stream(), calls);
                i += 1;
                continue;
            }
            TokenTree::Ident(ident) => ident,
            _ => {
                i += 1;
                continue;
            }
        };
        let before = i.checked_sub(1).map(|b| &trees[b]);
        let first = ident.to_string();
        let skip = punct(before, '.')
            || (punct(before, '$') && first != "crate")
            || matches!(before, Some(TokenTree::Ident(k)) if ["fn", "struct", "enum", "union"].contains(&k.to_string().as_str()));
        // Read `a::b::c`: each `::` is two `:` tokens.
        let mut path = vec![first];
        let mut last = ident;
        let mut j = i + 1;
        while punct(trees.get(j), ':') && punct(trees.get(j + 1), ':') {
            let Some(TokenTree::Ident(next)) = trees.get(j + 2) else {
                break;
            };
            path.push(next.to_string());
            last = next;
            j += 3;
        }
        let called = matches!(trees.get(j), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis);
        while path
            .first()
            .is_some_and(|s| matches!(s.as_str(), "crate" | "self" | "super"))
        {
            path.remove(0);
        }
        let keyword = path.len() == 1
            && matches!(
                path[0].as_str(),
                "if" | "while" | "match" | "return" | "in" | "for" | "loop" | "let" | "move"
            );
        if called && !skip && !keyword && !path.is_empty() {
            calls.push(MacroCall {
                path,
                line: last.span().start().line as u32,
            });
        }
        // The argument group, if any, is walked on the next pass.
        i = j;
    }
}

/// 1-based line and 0-based column of a token.
fn line_column(span: proc_macro2::Span) -> (u32, u32) {
    let start = span.start();
    (start.line as u32, start.column as u32)
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
        scan_source(source).expect("test source should parse")
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
                line: 4,
                column: 5
            })
        );
        assert_eq!(
            block.self_type,
            Some(NameAt {
                name: "A".into(),
                line: 4,
                column: 11
            })
        );
    }

    #[test]
    fn columns_are_counted_in_bytes_like_the_index() {
        // "é" is one character but two bytes; the index counts bytes.
        let facts = scan("struct É;\ntrait Ü {}\nimpl/*é*/ Ü for É {}\n");
        let block = &facts.impls[0];
        assert_eq!(block.trait_name.as_ref().map(|t| t.column), Some(11));
        assert_eq!(block.self_type.as_ref().map(|t| t.column), Some(18));
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

    #[test]
    fn finds_tests_by_attribute() {
        let source = "\
#[test]
fn plain() {}

#[tokio::test(flavor = \"multi_thread\")]
async fn with_tokio() {}

#[cfg(test)]
fn only_in_test_builds() {}

#[rstest]
fn with_rstest() {}

#[test_case(1 ; \"one\")]
#[test_case(2 ; \"two\")]
fn with_cases(n: u32) {}
";
        let found: Vec<(String, u32, bool)> = scan(source)
            .tests
            .into_iter()
            .map(|t| (t.name.name, t.name.line, t.parameterized))
            .collect();
        assert_eq!(
            found,
            vec![
                ("plain".to_string(), 2, false),
                ("with_tokio".to_string(), 5, false),
                ("with_rstest".to_string(), 11, true),
                ("with_cases".to_string(), 15, true),
            ]
        );
    }

    #[test]
    fn finds_calls_written_in_macro_bodies() {
        let source = "\
macro_rules! log_err {
    (call($e:expr)) => {
        $crate::log::write_line(&format!(\"{}\", $e))
    };
    ($f:ident) => {{
        helper();
        x.method();
        $f(1);
        fn defined_here() {}
        Some(other::path::thing(2))
    }};
}
";
        let facts = scan(source);
        let body = &facts.macro_bodies[0];
        assert_eq!(body.name.name, "log_err");
        let calls: Vec<(String, u32)> = body
            .calls
            .iter()
            .map(|c| (c.path.join("::"), c.line))
            .collect();
        // Not `call` (in a matcher), `method` (a method), `$f` (a macro
        // variable), `defined_here` (a definition) or `format` (a macro).
        assert_eq!(
            calls,
            vec![
                ("log::write_line".to_string(), 3),
                ("helper".to_string(), 6),
                ("Some".to_string(), 10),
                ("other::path::thing".to_string(), 10),
            ]
        );
    }

    #[test]
    fn records_out_of_line_modules_with_their_inline_path_and_path_attribute() {
        let source = "\
mod plain;
mod inline {
    mod nested;
    #[path = \"elsewhere/x.rs\"]
    mod moved;
    fn f() {}
}
mod with_body { fn g() {} }
cfg_feature! {
    mod from_a_macro;
}
";
        let mods: Vec<(String, Vec<String>, Option<String>)> = scan(source)
            .mods
            .into_iter()
            .map(|m| (m.name, m.inline_path, m.path_attr))
            .collect();
        assert_eq!(
            mods,
            vec![
                ("plain".to_string(), vec![], None),
                ("nested".to_string(), vec!["inline".to_string()], None),
                (
                    "moved".to_string(),
                    vec!["inline".to_string()],
                    Some("elsewhere/x.rs".to_string())
                ),
                ("from_a_macro".to_string(), vec![], None),
            ]
        );
    }

    #[test]
    fn looks_inside_macro_calls_that_hold_plain_rust() {
        let source = "\
cfg_feature! {
    #[test]
    fn inside_an_item_macro() {}

    pub fn reads(p: *const u8) -> u8 {
        // SAFETY: p is valid.
        unsafe { *p }
    }
}

fn in_arguments(p: *const u8) {
    println!(\"{}\", unsafe { *p });
}

macro_rules! template {
    () => {
        unsafe { std::hint::unreachable_unchecked() }
    };
}
";
        let facts = scan(source);
        assert_eq!(facts.tests.len(), 1);
        assert_eq!(facts.tests[0].name.name, "inside_an_item_macro");
        let sites: Vec<(u32, bool)> = facts
            .unsafe_code
            .iter()
            .map(|u| (u.line, u.documented))
            .collect();
        // The `macro_rules!` body is a template: its block is not counted.
        assert_eq!(sites, vec![(7, true), (12, false)]);
    }

    /// (kind, line, documented) for each unsafe site.
    fn unsafe_sites(source: &str) -> Vec<(UnsafeKind, u32, bool)> {
        scan(source)
            .unsafe_code
            .into_iter()
            .map(|u| (u.kind, u.line, u.documented))
            .collect()
    }

    #[test]
    fn a_safety_comment_right_above_documents_a_block() {
        let source = "\
fn f(p: *const u8) -> u8 {
    // SAFETY: p is valid.
    let a = unsafe { *p };

    // Safety: the same pointer, and the case doesn't matter.

    let b = unsafe { *p };
    // This comment says nothing about safety.
    let c = unsafe { *p };
    a + b + c
}
";
        assert_eq!(
            unsafe_sites(source),
            vec![
                (UnsafeKind::Block, 3, true),
                (UnsafeKind::Block, 7, true),
                (UnsafeKind::Block, 9, false),
            ]
        );
    }

    #[test]
    fn a_comment_above_the_whole_statement_counts_but_not_one_above_other_code() {
        let source = "\
fn f(p: *const u8, ok: bool) -> u8 {
    // SAFETY: only read when ok is true.
    match ok {
        true => unsafe { *p },
        false => 0,
    }
}

fn g(p: *const u8) -> u8 {
    // SAFETY: this comment belongs to the line below it.
    let x = 1;
    x + unsafe { *p }
}

fn h(p: *const u8) -> u8 {
    /* SAFETY: inline. */ unsafe { *p }
}

fn i(p: *const u8) -> u8 {
    /*
     * SAFETY: in a block comment.
     */
    unsafe { *p }
}
";
        assert_eq!(
            unsafe_sites(source),
            vec![
                (UnsafeKind::Block, 4, true),
                (UnsafeKind::Block, 12, false),
                (UnsafeKind::Block, 16, true),
                (UnsafeKind::Block, 23, true),
            ]
        );
    }

    #[test]
    fn unsafe_fns_and_traits_need_a_safety_section() {
        let source = "\
/// Reads a byte.
///
/// # Safety
///
/// `p` must be valid.
pub unsafe fn documented(p: *const u8) -> u8 {
    // SAFETY: the caller promises p is valid.
    unsafe { *p }
}

/// Nothing about safety here.
pub unsafe fn undocumented() {}

/// ## SAFETY
/// Implementers must be honest.
pub unsafe trait Honest {
    unsafe fn method(&self);
}

struct S;

// SAFETY: S has no fields.
unsafe impl Send for S {}

unsafe impl Sync for S {}

impl S {
    unsafe fn inherent(&self) {}
}

impl Honest for S {
    unsafe fn method(&self) {}
}

// # Safety
//
// A heading instead of `SAFETY:` counts too.
unsafe impl Sync for Honest2 {}

/// Safety: a line instead of a section counts too.
unsafe fn with_a_safety_line() {}
";
        assert_eq!(
            unsafe_sites(source),
            vec![
                (UnsafeKind::Fn, 6, true),
                (UnsafeKind::Block, 8, true),
                (UnsafeKind::Fn, 12, false),
                (UnsafeKind::Trait, 16, true),
                (UnsafeKind::Fn, 17, false),
                (UnsafeKind::Impl, 23, true),
                (UnsafeKind::Impl, 25, false),
                (UnsafeKind::Fn, 28, false),
                // Implements `Honest::method`, whose docs carry the contract.
                (UnsafeKind::Fn, 32, true),
                (UnsafeKind::Impl, 38, true),
                (UnsafeKind::Fn, 41, true),
            ]
        );
        assert!(scan(source).unsafe_code[8].in_trait_impl);
        let facts = scan(source);
        let send = &facts.unsafe_code[5];
        assert_eq!(send.trait_name.as_deref(), Some("Send"));
        assert_eq!(send.name.as_ref().map(|n| n.name.as_str()), Some("S"));
    }
}
