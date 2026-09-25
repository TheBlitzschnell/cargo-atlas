//! Reading rust-analyzer's symbol strings.
//!
//! Every item in the index has a symbol string like this one:
//!
//! ```text
//! rust-analyzer cargo spike 0.1.0 loader/impl#[CsvReader][Loader]load().
//! └── tool ───┘ └mgr┘ └pkg┘ └ver┘ └───────────── descriptors ──────────┘
//! ```
//!
//! The descriptors carry the meaning. Each one ends with a character that says
//! what it is: `loader/` is a module, `impl#` starts an impl block,
//! `[CsvReader]` and `[Loader]` are that impl's self type and trait, and
//! `load().` is a method. So this symbol is `<CsvReader as Loader>::load`.

use scip::symbol::{format_symbol, is_local_symbol, parse_symbol};
use scip::types::Descriptor;
use scip::types::descriptor::Suffix;

/// The header of the impl block an item sits in: `impl Loader for CsvReader`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplHeader {
    pub self_type: String,
    pub trait_name: Option<String>,
}

/// What the last descriptor says the item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Module,
    Type,
    Method,
    /// Fields, constants, statics.
    Term,
    Macro,
    /// The impl block itself, e.g. `impl#[CsvReader][Loader]`.
    Impl,
    /// Type parameters, parameters and anything else we don't map.
    Other,
}

/// A symbol string taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSymbol {
    /// The Cargo package, e.g. `spike`.
    pub package: String,
    /// Modules above the item, outermost first, e.g. `["loader"]`.
    pub modules: Vec<String>,
    /// Set when the item sits inside an impl block.
    pub impl_header: Option<ImplHeader>,
    /// The type or trait directly around the item (not an impl block), e.g. `Loader`.
    pub owner: Option<String>,
    /// The item's own name, e.g. `load`.
    pub name: String,
    pub shape: Shape,
}

impl ParsedSymbol {
    /// The short name people use: `JsonReader::parse`, `<CsvReader as Loader>::load`.
    pub fn display_name(&self) -> String {
        if self.shape == Shape::Impl {
            return match &self.impl_header {
                Some(ImplHeader {
                    self_type,
                    trait_name: Some(t),
                }) => format!("impl {t} for {self_type}"),
                Some(ImplHeader {
                    self_type,
                    trait_name: None,
                }) => format!("impl {self_type}"),
                None => "impl".to_string(),
            };
        }
        match (&self.impl_header, &self.owner) {
            (
                Some(ImplHeader {
                    self_type,
                    trait_name: Some(t),
                }),
                _,
            ) => {
                format!("<{self_type} as {t}>::{}", self.name)
            }
            (
                Some(ImplHeader {
                    self_type,
                    trait_name: None,
                }),
                _,
            ) => {
                format!("{self_type}::{}", self.name)
            }
            (None, Some(owner)) => format!("{owner}::{}", self.name),
            (None, None) => self.name.clone(),
        }
    }

    /// Module path plus display name: `json_reader::JsonReader::parse`.
    pub fn full_path(&self) -> String {
        if self.modules.is_empty() {
            self.display_name()
        } else {
            format!("{}::{}", self.modules.join("::"), self.display_name())
        }
    }
}

fn suffix(d: &Descriptor) -> Suffix {
    d.suffix.enum_value_or_default()
}

/// Takes a symbol string apart. Returns `None` for local symbols
/// (variables inside a function) and for strings that don't parse.
pub fn parse(symbol: &str) -> Option<ParsedSymbol> {
    if is_local_symbol(symbol) {
        return None;
    }
    let parsed = parse_symbol(symbol).ok()?;
    let package = parsed
        .package
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let (last, rest) = parsed.descriptors.split_last()?;

    let mut modules = Vec::new();
    let mut impl_header: Option<ImplHeader> = None;
    let mut owner: Option<String> = None;
    // True right after `impl#`, while we read its `[SelfType][Trait]` brackets.
    let mut reading_impl_brackets = false;

    for d in rest {
        match suffix(d) {
            Suffix::Namespace | Suffix::Package if impl_header.is_none() && owner.is_none() => {
                modules.push(d.name.clone());
            }
            Suffix::Type if d.name == "impl" => {
                impl_header = Some(ImplHeader {
                    self_type: String::new(),
                    trait_name: None,
                });
                reading_impl_brackets = true;
            }
            Suffix::TypeParameter if reading_impl_brackets => {
                add_impl_bracket(impl_header.as_mut(), &d.name);
            }
            _ => {
                reading_impl_brackets = false;
                owner = Some(d.name.clone());
            }
        }
    }

    let (name, shape) = match suffix(last) {
        // The symbol is the impl block itself: its last bracket closes the header.
        Suffix::TypeParameter if reading_impl_brackets => {
            add_impl_bracket(impl_header.as_mut(), &last.name);
            ("impl".to_string(), Shape::Impl)
        }
        Suffix::Type if last.name == "impl" => ("impl".to_string(), Shape::Impl),
        Suffix::Namespace | Suffix::Package => (last.name.clone(), Shape::Module),
        Suffix::Type => (last.name.clone(), Shape::Type),
        Suffix::Method => (last.name.clone(), Shape::Method),
        Suffix::Term => (last.name.clone(), Shape::Term),
        Suffix::Macro => (last.name.clone(), Shape::Macro),
        _ => (last.name.clone(), Shape::Other),
    };

    Some(ParsedSymbol {
        package,
        modules,
        impl_header,
        owner,
        name,
        shape,
    })
}

/// The first bracket after `impl#` is the self type, the second the trait.
fn add_impl_bracket(header: Option<&mut ImplHeader>, name: &str) {
    if let Some(h) = header {
        if h.self_type.is_empty() {
            h.self_type = name.to_string();
        } else {
            h.trait_name = Some(name.to_string());
        }
    }
}

/// The symbol one level up: `loader/Loader#load().` becomes `loader/Loader#`.
/// Returns `None` for top-level symbols and local symbols.
pub fn parent(symbol: &str) -> Option<String> {
    if is_local_symbol(symbol) {
        return None;
    }
    let mut parsed = parse_symbol(symbol).ok()?;
    parsed.descriptors.pop()?;
    // Leaving an impl's brackets behind would name half a header; drop them too.
    while parsed
        .descriptors
        .last()
        .is_some_and(|d| suffix(d) == Suffix::TypeParameter)
    {
        parsed.descriptors.pop();
    }
    if parsed.descriptors.last().is_some_and(|d| d.name == "impl") {
        return None; // The parent is an impl block; the builder resolves those separately.
    }
    if parsed.descriptors.is_empty() {
        return None;
    }
    Some(format_symbol(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "rust-analyzer cargo spike 0.1.0 ";

    fn p(descriptors: &str) -> ParsedSymbol {
        parse(&format!("{PREFIX}{descriptors}")).expect("symbol should parse")
    }

    #[test]
    fn inherent_method() {
        let s = p("json_reader/impl#[JsonReader]parse().");
        assert_eq!(s.package, "spike");
        assert_eq!(s.modules, vec!["json_reader"]);
        assert_eq!(s.shape, Shape::Method);
        assert_eq!(s.display_name(), "JsonReader::parse");
        assert_eq!(s.full_path(), "json_reader::JsonReader::parse");
    }

    #[test]
    fn trait_impl_method() {
        let s = p("loader/impl#[CsvReader][Loader]load().");
        assert_eq!(
            s.impl_header,
            Some(ImplHeader {
                self_type: "CsvReader".into(),
                trait_name: Some("Loader".into())
            })
        );
        assert_eq!(s.display_name(), "<CsvReader as Loader>::load");
    }

    #[test]
    fn trait_method_and_trait() {
        assert_eq!(p("loader/Loader#load().").display_name(), "Loader::load");
        let t = p("loader/Loader#");
        assert_eq!(
            (t.shape, t.display_name()),
            (Shape::Type, "Loader".to_string())
        );
    }

    #[test]
    fn free_function_and_module() {
        assert_eq!(p("main().").display_name(), "main");
        let m = p("loader/");
        assert_eq!((m.shape, m.name.as_str()), (Shape::Module, "loader"));
    }

    #[test]
    fn impl_block_itself() {
        let s = p("loader/impl#[CsvReader][Loader]");
        assert_eq!(s.shape, Shape::Impl);
        assert_eq!(s.display_name(), "impl Loader for CsvReader");
    }

    #[test]
    fn local_symbols_are_skipped() {
        assert_eq!(parse("local 3"), None);
    }

    #[test]
    fn parents() {
        let parent_of = |d: &str| parent(&format!("{PREFIX}{d}"));
        assert_eq!(
            parent_of("loader/Loader#load()."),
            Some(format!("{PREFIX}loader/Loader#"))
        );
        assert_eq!(
            parent_of("loader/Loader#"),
            Some(format!("{PREFIX}loader/"))
        );
        assert_eq!(parent_of("main()."), None);
        // Items in impl blocks have no symbol parent; the builder uses the impl header.
        assert_eq!(parent_of("loader/impl#[CsvReader][Loader]load()."), None);
    }
}
