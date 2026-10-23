//! Which Cargo target each file belongs to: the library, a binary, an
//! integration test, an example, a bench or the build script.
//!
//! `cargo test` builds one test binary per target, and a test's name is its
//! module path inside that target. So to print a command that runs exactly
//! the tests that reach an item (`cargo test --test shapes -- --exact ...`),
//! the graph records the files of each target.
//!
//! Cargo names each target's root file (`src/lib.rs`, `tests/shapes.rs`).
//! The other files are found the way rustc finds them: by following
//! `mod name;` declarations from the root to `name.rs` or `name/mod.rs`, or to
//! the file a `#[path = "..."]` attribute names. Only files rust-analyzer
//! indexed count, so a module behind a cfg that is off is left out, as it is
//! from the graph.
//!
//! Not followed: a `#[path]` attribute on an inline module (`#[path = "x"]
//! mod a { mod b; }`), which is rare.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use crate::cargo_meta::Workspace;
use crate::model::{Target, TargetKind};
use crate::syntax::{FileFacts, ModDecl};

/// Every target of every workspace package, with the indexed files it is built from.
pub fn map(
    workspace: &Workspace,
    indexed: &HashSet<&str>,
    facts: &HashMap<String, FileFacts>,
) -> Vec<Target> {
    let mut targets = Vec::new();
    for package in &workspace.packages {
        for target in &package.targets {
            let (Some(kind), Some(root)) = (
                target_kind(&target.kind),
                relative(&workspace.root, &target.src_path),
            ) else {
                continue;
            };
            targets.push(Target {
                package: package.name.clone(),
                name: target.name.clone(),
                kind,
                files: files_of(&root, indexed, facts),
            });
        }
    }
    targets.sort_by(|a, b| (&a.package, a.kind, &a.name).cmp(&(&b.package, b.kind, &b.name)));
    targets
}

/// What Cargo's kinds for a target mean for `cargo test`: `["lib"]`,
/// `["bin"]`, `["test"]`, `["example"]`, `["bench"]`, `["custom-build"]`, or
/// a library flavor such as `["cdylib", "rlib"]` or `["proc-macro"]`.
fn target_kind(kinds: &[String]) -> Option<TargetKind> {
    let has = |kind: &str| kinds.iter().any(|k| k == kind);
    let kind = if has("bin") {
        TargetKind::Bin
    } else if has("test") {
        TargetKind::Test
    } else if has("example") {
        TargetKind::Example
    } else if has("bench") {
        TargetKind::Bench
    } else if has("custom-build") {
        TargetKind::BuildScript
    } else if ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"]
        .iter()
        .any(|k| has(k))
    {
        TargetKind::Lib
    } else {
        return None;
    };
    Some(kind)
}

/// `path` relative to `root`, with `/` between folders.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

/// The indexed files reached from `root` through `mod` declarations, root first.
fn files_of(
    root: &str,
    indexed: &HashSet<&str>,
    facts: &HashMap<String, FileFacts>,
) -> Vec<String> {
    let mut files = vec![root.to_string()];
    let mut seen: HashSet<String> = HashSet::from([root.to_string()]);
    // Each file with whether it owns its folder: see `module_files`.
    let mut queue = VecDeque::from([(root.to_string(), true)]);
    while let Some((file, owns_folder)) = queue.pop_front() {
        let Some(file_facts) = facts.get(&file) else {
            continue;
        };
        for decl in &file_facts.mods {
            let found = module_files(&file, owns_folder, decl)
                .into_iter()
                .find(|candidate| indexed.contains(candidate.as_str()));
            let Some(found) = found else {
                continue;
            };
            if seen.insert(found.clone()) {
                // rustc treats a file loaded through `#[path]` like a `mod.rs`.
                let owns = decl.path_attr.is_some() || file_name(&found) == "mod.rs";
                files.push(found.clone());
                queue.push_back((found, owns));
            }
        }
    }
    files
}

/// The files a `mod` declaration in `file` may load, in the order rustc tries them.
///
/// A crate root, a `mod.rs` and a file loaded through `#[path]` own their
/// folder: their submodules sit next to them. Any other file, `net.rs` say,
/// keeps its submodules in a folder named after it, `net/`.
fn module_files(file: &str, owns_folder: bool, decl: &ModDecl) -> Vec<String> {
    let folder = parent(file);
    let own = if owns_folder {
        folder.to_string()
    } else {
        join(folder, stem(file))
    };
    let inside = decl
        .inline_path
        .iter()
        .fold(own, |path, inline| join(&path, inline));
    match &decl.path_attr {
        // Outside inline modules a `#[path]` is relative to the file's folder;
        // inside them, to the inline modules' folder.
        Some(path) if decl.inline_path.is_empty() => vec![normalize(&join(folder, path))],
        Some(path) => vec![normalize(&join(&inside, path))],
        None => vec![
            join(&inside, &format!("{}.rs", decl.name)),
            join(&inside, &format!("{}/mod.rs", decl.name)),
        ],
    }
}

/// `src` for `src/lib.rs`, `` for `build.rs`.
fn parent(file: &str) -> &str {
    file.rsplit_once('/').map_or("", |(folder, _)| folder)
}

fn file_name(file: &str) -> &str {
    file.rsplit_once('/').map_or(file, |(_, name)| name)
}

/// `net` for `src/net.rs`.
fn stem(file: &str) -> &str {
    let name = file_name(file);
    name.strip_suffix(".rs").unwrap_or(name)
}

fn join(folder: &str, rest: &str) -> String {
    if folder.is_empty() {
        rest.to_string()
    } else {
        format!("{folder}/{rest}")
    }
}

/// Resolves `.` and `..`: `src/a/../b.rs` becomes `src/b.rs`.
fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, inline_path: &[&str], path_attr: Option<&str>) -> ModDecl {
        ModDecl {
            name: name.to_string(),
            inline_path: inline_path.iter().map(|s| s.to_string()).collect(),
            path_attr: path_attr.map(str::to_string),
        }
    }

    #[test]
    fn module_files_follow_rustc() {
        let plain = decl("net", &[], None);
        // A crate root keeps submodules beside it.
        assert_eq!(
            module_files("src/lib.rs", true, &plain),
            ["src/net.rs", "src/net/mod.rs"]
        );
        // `net.rs` keeps its own in `net/`.
        assert_eq!(
            module_files("src/net.rs", false, &decl("tcp", &[], None)),
            ["src/net/tcp.rs", "src/net/tcp/mod.rs"]
        );
        // Inside `mod inline { ... }`, the inline module is a folder.
        assert_eq!(
            module_files("src/lib.rs", true, &decl("x", &["inline"], None)),
            ["src/inline/x.rs", "src/inline/x/mod.rs"]
        );
        assert_eq!(
            module_files("src/net.rs", false, &decl("x", &["inline"], None)),
            ["src/net/inline/x.rs", "src/net/inline/x/mod.rs"]
        );
        // `#[path]` outside inline modules: relative to the file's folder,
        // even for a file that doesn't own it.
        assert_eq!(
            module_files("src/net.rs", false, &decl("m", &[], Some("../shared/m.rs"))),
            ["shared/m.rs"]
        );
        // `#[path]` inside an inline module: relative to that module's folder.
        assert_eq!(
            module_files("src/net.rs", false, &decl("m", &["a"], Some("m2.rs"))),
            ["src/net/a/m2.rs"]
        );
        // An integration test's root keeps `mod common;` beside it.
        assert_eq!(
            module_files("tests/api.rs", true, &decl("common", &[], None)),
            ["tests/common.rs", "tests/common/mod.rs"]
        );
        assert_eq!(
            module_files("build.rs", true, &plain),
            ["net.rs", "net/mod.rs"]
        );
    }

    #[test]
    fn files_are_found_through_mod_declarations() {
        let mut facts: HashMap<String, FileFacts> = HashMap::new();
        let with_mods = |mods: Vec<ModDecl>| FileFacts {
            mods,
            ..FileFacts::default()
        };
        facts.insert(
            "src/lib.rs".into(),
            with_mods(vec![decl("net", &[], None), decl("gone", &[], None)]),
        );
        facts.insert("src/net.rs".into(), with_mods(vec![decl("tcp", &[], None)]));
        facts.insert(
            "src/net/tcp/mod.rs".into(),
            with_mods(vec![decl("frame", &[], None)]),
        );
        facts.insert(
            "tests/api.rs".into(),
            with_mods(vec![decl("common", &[], None)]),
        );
        let indexed: HashSet<&str> = [
            "src/lib.rs",
            "src/net.rs",
            "src/net/tcp/mod.rs",
            "src/net/tcp/frame.rs",
            "tests/api.rs",
            "tests/common/mod.rs",
        ]
        .into_iter()
        .collect();
        // `gone` isn't indexed (a cfg that is off, say), so it's left out.
        assert_eq!(
            files_of("src/lib.rs", &indexed, &facts),
            [
                "src/lib.rs",
                "src/net.rs",
                "src/net/tcp/mod.rs",
                "src/net/tcp/frame.rs"
            ]
        );
        assert_eq!(
            files_of("tests/api.rs", &indexed, &facts),
            ["tests/api.rs", "tests/common/mod.rs"]
        );
    }

    #[test]
    fn cargo_kinds_map_to_how_cargo_test_selects_them() {
        let kinds = |k: &[&str]| target_kind(&k.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(kinds(&["lib"]), Some(TargetKind::Lib));
        assert_eq!(kinds(&["cdylib", "rlib"]), Some(TargetKind::Lib));
        assert_eq!(kinds(&["proc-macro"]), Some(TargetKind::Lib));
        assert_eq!(kinds(&["bin"]), Some(TargetKind::Bin));
        assert_eq!(kinds(&["test"]), Some(TargetKind::Test));
        assert_eq!(kinds(&["example"]), Some(TargetKind::Example));
        assert_eq!(kinds(&["custom-build"]), Some(TargetKind::BuildScript));
        assert_eq!(kinds(&["something-new"]), None);
    }

    #[test]
    fn normalizes_dots() {
        assert_eq!(normalize("src/a/../b.rs"), "src/b.rs");
        assert_eq!(normalize("./src/./x.rs"), "src/x.rs");
        assert_eq!(normalize("src\\win\\x.rs"), "src/win/x.rs");
    }
}
