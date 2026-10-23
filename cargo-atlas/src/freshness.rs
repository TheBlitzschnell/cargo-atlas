//! Telling whether `graph.json` still matches the code.
//!
//! A build records each file it read: size, modification time and a hash of
//! the content. A check stats every file and hashes only those whose size or
//! time moved, so it takes milliseconds even on large workspaces, and a file
//! that was saved without being changed doesn't count as changed.
//!
//! New files need one more look. A new module shows up as an edit to the file
//! that declares it, but Cargo finds some targets by where their file is:
//! `tests/*.rs`, `src/bin/*.rs`, `examples/*.rs`, `benches/*.rs` (each also as
//! `NAME/main.rs`), `src/main.rs`, `src/lib.rs` and `build.rs`. A new file
//! there changes no stamped file, so the build also records which of those
//! exist, and a check lists them again.

use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::FileStamp;

/// What changed since a build.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Stamped files whose content differs, or that are gone. Sorted.
    pub edited: Vec<String>,
    /// New files where Cargo finds targets on its own, such as `tests/new.rs`. Sorted.
    pub added: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.edited.is_empty() && self.added.is_empty()
    }

    /// `1 file changed (src/lib.rs)`, `1 new file (tests/new.rs)`, or both,
    /// joined with "and".
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.edited.is_empty() {
            let noun = if self.edited.len() == 1 {
                "file"
            } else {
                "files"
            };
            parts.push(format!(
                "{} {noun} changed ({})",
                self.edited.len(),
                some_names(&self.edited)
            ));
        }
        if !self.added.is_empty() {
            let noun = if self.added.len() == 1 {
                "new file"
            } else {
                "new files"
            };
            parts.push(format!(
                "{} {noun} ({})",
                self.added.len(),
                some_names(&self.added)
            ));
        }
        parts.join(" and ")
    }
}

/// `src/a.rs, src/b.rs`, or `src/a.rs, ... and 7 more` past five names.
fn some_names(paths: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut names = paths
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        names.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    names
}

/// Stamps `files` (paths relative to `root`) as they are now.
///
/// `build_started` guards against a race: a file saved while rust-analyzer
/// was reading the workspace may be in the graph in its old form or its new
/// one. Such a file gets an empty hash, so the next check calls it changed.
/// Files that can't be read are left out; the index can name files that no
/// longer exist.
pub fn stamp(root: &Path, files: &[String], build_started: SystemTime) -> Vec<FileStamp> {
    let started_ns = nanos_since_1970(build_started);
    let mut stamps: Vec<FileStamp> = files
        .iter()
        .filter_map(|path| {
            let full = root.join(path);
            let meta = std::fs::metadata(&full).ok()?;
            let mtime_ns = meta.modified().map(nanos_since_1970).unwrap_or(0);
            let hash = if mtime_ns >= started_ns {
                String::new()
            } else {
                hash_hex(&std::fs::read(&full).ok()?)
            };
            Some(FileStamp {
                path: path.clone(),
                len: meta.len(),
                mtime_ns,
                hash,
            })
        })
        .collect();
    stamps.sort_by(|a, b| a.path.cmp(&b.path));
    stamps.dedup_by(|a, b| a.path == b.path);
    stamps
}

/// What changed since the build that wrote `stamps` and `discovered`.
pub fn changed(root: &Path, stamps: &[FileStamp], discovered: &[String]) -> Changes {
    let edited = stamps
        .iter()
        .filter(|stamp| has_changed(root, stamp))
        .map(|stamp| stamp.path.clone())
        .collect();
    let manifests: Vec<&str> = stamps
        .iter()
        .map(|stamp| stamp.path.as_str())
        .filter(|path| file_name(path) == "Cargo.toml")
        .collect();
    let known: HashSet<&str> = discovered.iter().map(String::as_str).collect();
    let added = discover(root, &manifests)
        .into_iter()
        .filter(|path| !known.contains(path.as_str()))
        .collect();
    Changes { edited, added }
}

/// The files in the places Cargo finds targets on its own, for the packages
/// whose `Cargo.toml` paths (relative to `root`) are given. Sorted.
pub fn discover(root: &Path, manifests: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for manifest in manifests {
        let package = manifest.rsplit_once('/').map_or("", |(folder, _)| folder);
        let at = |rel: &str| {
            if package.is_empty() {
                rel.to_string()
            } else {
                format!("{package}/{rel}")
            }
        };
        for single in ["build.rs", "src/lib.rs", "src/main.rs"] {
            if root.join(at(single)).is_file() {
                found.push(at(single));
            }
        }
        for folder in ["src/bin", "tests", "examples", "benches"] {
            let Ok(entries) = std::fs::read_dir(root.join(at(folder))) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let path = entry.path();
                if path.is_file() && name.ends_with(".rs") {
                    found.push(at(&format!("{folder}/{name}")));
                } else if path.join("main.rs").is_file() {
                    found.push(at(&format!("{folder}/{name}/main.rs")));
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn has_changed(root: &Path, stamp: &FileStamp) -> bool {
    let full = root.join(&stamp.path);
    let Ok(meta) = std::fs::metadata(&full) else {
        return true;
    };
    let mtime_ns = meta.modified().map(nanos_since_1970).unwrap_or(0);
    if meta.len() == stamp.len && mtime_ns == stamp.mtime_ns && !stamp.hash.is_empty() {
        return false;
    }
    // Size or time moved: compare the content itself.
    match std::fs::read(&full) {
        Ok(bytes) => stamp.hash.is_empty() || hash_hex(&bytes) != stamp.hash,
        Err(_) => true,
    }
}

fn nanos_since_1970(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// FNV-1a, 64 bits. Not a cryptographic hash; it only needs to notice edits,
/// and unlike `std`'s hasher its output never changes between Rust releases.
fn hash_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A fresh, empty folder under the system's temp folder.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cargo-atlas-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn known_hash_values() {
        // Reference values for FNV-1a 64.
        assert_eq!(hash_hex(b""), "cbf29ce484222325");
        assert_eq!(hash_hex(b"a"), "af63dc4c8601ec8c");
    }

    #[test]
    fn edits_and_deletions_count_but_saving_the_same_text_does_not() {
        let dir = temp_dir("freshness");
        for name in ["same.rs", "edited.rs", "deleted.rs"] {
            std::fs::write(dir.join(name), "fn main() {}\n").unwrap();
        }
        let files: Vec<String> = ["same.rs", "edited.rs", "deleted.rs", "never_existed.rs"]
            .map(String::from)
            .to_vec();
        let later = SystemTime::now() + Duration::from_secs(60);
        let stamps = stamp(&dir, &files, later);
        assert_eq!(stamps.len(), 3, "a missing file gets no stamp");
        assert!(changed(&dir, &stamps, &[]).is_empty());

        // Same text, new modification time: not a change.
        std::fs::write(dir.join("same.rs"), "fn main() {}\n").unwrap();
        let file = std::fs::File::options()
            .write(true)
            .open(dir.join("same.rs"))
            .unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        std::fs::write(dir.join("edited.rs"), "fn main() { run() }\n").unwrap();
        std::fs::remove_file(dir.join("deleted.rs")).unwrap();
        assert_eq!(
            changed(&dir, &stamps, &[]).edited,
            vec!["deleted.rs", "edited.rs"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_saved_during_the_build_counts_as_changed() {
        let dir = temp_dir("race");
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let long_ago = UNIX_EPOCH + Duration::from_secs(1);
        let stamps = stamp(&dir, &["a.rs".to_string()], long_ago);
        assert_eq!(stamps[0].hash, "");
        assert_eq!(changed(&dir, &stamps, &[]).edited, vec!["a.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_target_file_counts_as_added_but_a_new_module_file_does_not() {
        let dir = temp_dir("added");
        for folder in ["app/src", "app/tests/it", "lib/src"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        for file in [
            "Cargo.toml",
            "app/Cargo.toml",
            "app/src/main.rs",
            "app/tests/old.rs",
            "lib/Cargo.toml",
            "lib/src/lib.rs",
        ] {
            std::fs::write(dir.join(file), "\n").unwrap();
        }
        let later = SystemTime::now() + Duration::from_secs(60);
        let files: Vec<String> = ["Cargo.toml", "app/Cargo.toml", "lib/Cargo.toml"]
            .map(String::from)
            .to_vec();
        let stamps = stamp(&dir, &files, later);
        let discovered = discover(&dir, &["Cargo.toml", "app/Cargo.toml", "lib/Cargo.toml"]);
        assert_eq!(
            discovered,
            ["app/src/main.rs", "app/tests/old.rs", "lib/src/lib.rs"]
        );
        assert!(changed(&dir, &stamps, &discovered).is_empty());

        // A module file is only used once some file says `mod`, and that file is stamped.
        std::fs::write(dir.join("lib/src/helper.rs"), "\n").unwrap();
        // New targets, which nothing stamped would show.
        std::fs::write(dir.join("app/tests/new.rs"), "\n").unwrap();
        std::fs::write(dir.join("app/tests/it/main.rs"), "\n").unwrap();
        std::fs::write(dir.join("lib/build.rs"), "\n").unwrap();
        let changes = changed(&dir, &stamps, &discovered);
        assert_eq!(
            changes.added,
            ["app/tests/it/main.rs", "app/tests/new.rs", "lib/build.rs"]
        );
        assert_eq!(
            changes.describe(),
            "3 new files (app/tests/it/main.rs, app/tests/new.rs, lib/build.rs)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn describes_edits_and_additions() {
        let changes = Changes {
            edited: vec!["src/lib.rs".into()],
            added: vec!["tests/new.rs".into()],
        };
        assert_eq!(
            changes.describe(),
            "1 file changed (src/lib.rs) and 1 new file (tests/new.rs)"
        );
        let many = Changes {
            edited: (0..7).map(|i| format!("src/{i}.rs")).collect(),
            added: vec![],
        };
        assert_eq!(
            many.describe(),
            "7 files changed (src/0.rs, src/1.rs, src/2.rs, src/3.rs, src/4.rs and 2 more)"
        );
    }
}
