//! Telling whether `graph.json` still matches the code.
//!
//! A build records each file it read: size, modification time and a hash of
//! the content. A check stats every file and hashes only those whose size or
//! time moved, so it takes milliseconds even on large workspaces, and a file
//! that was saved without being changed doesn't count as changed.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::FileStamp;

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

/// The files whose content differs from their stamp, or that are gone. Sorted.
pub fn changed(root: &Path, stamps: &[FileStamp]) -> Vec<String> {
    stamps
        .iter()
        .filter(|stamp| has_changed(root, stamp))
        .map(|stamp| stamp.path.clone())
        .collect()
}

/// `1 file changed (src/lib.rs)`, or `12 files changed (src/a.rs, ... and 7 more)`.
pub fn describe(changed: &[String]) -> String {
    const SHOWN: usize = 5;
    let noun = if changed.len() == 1 { "file" } else { "files" };
    let mut names = changed
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if changed.len() > SHOWN {
        names.push_str(&format!(" and {} more", changed.len() - SHOWN));
    }
    format!("{} {noun} changed ({names})", changed.len())
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
        assert!(changed(&dir, &stamps).is_empty());

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
        assert_eq!(changed(&dir, &stamps), vec!["deleted.rs", "edited.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_saved_during_the_build_counts_as_changed() {
        let dir = temp_dir("race");
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        let long_ago = UNIX_EPOCH + Duration::from_secs(1);
        let stamps = stamp(&dir, &["a.rs".to_string()], long_ago);
        assert_eq!(stamps[0].hash, "");
        assert_eq!(changed(&dir, &stamps), vec!["a.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
