//! File-system snapshot utilities for ritual phase post-conditions.
//!
//! Used by the v2 executor to verify that phases like `implement` actually
//! produced file changes. The flow is:
//!
//! 1. Before invoking a skill, snapshot the project tree.
//! 2. Run the skill (LLM agent loop with Write/Edit/Bash tools).
//! 3. Snapshot again, diff, and decide whether the phase satisfied its
//!    post-condition.
//!
//! ISS-025: Without a post-condition, an LLM that produces only commentary
//! (no Write/Edit calls) is indistinguishable from a successful implement
//! phase, because the verify phase runs `cargo build && cargo test` against
//! an unchanged tree and trivially passes.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::ignore::{load_ignore_list, IgnoreList};

/// Files larger than this are fingerprinted by (size, mtime) only — hashing
/// huge binaries (build artifacts that slipped past .gidignore, large data
/// files, etc.) would slow snapshots to the point of unusability. Source
/// code files are always far below this threshold.
const HASH_SIZE_LIMIT_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB

/// Fingerprint of a single file at a point in time.
///
/// Two fingerprints compare equal iff (a) both have the same size and (b)
/// either both have the same content hash, or both fell back to the same
/// (size, mtime) pair (large-file path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    pub size: u64,
    /// Hex-encoded sha256 of the file contents, or `None` if the file
    /// exceeded `HASH_SIZE_LIMIT_BYTES` and we fell back to mtime.
    pub content_hash: Option<String>,
    /// mtime as nanoseconds since UNIX_EPOCH. Only consulted as a tiebreaker
    /// when `content_hash` is `None`.
    pub mtime_nanos: Option<u128>,
}

impl FileFingerprint {
    fn from_metadata_and_contents(meta: &Metadata, path: &Path) -> std::io::Result<Self> {
        let size = meta.len();
        if size > HASH_SIZE_LIMIT_BYTES {
            let mtime_nanos = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos());
            return Ok(Self {
                size,
                content_hash: None,
                mtime_nanos,
            });
        }

        let mut file = fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let content_hash = format!("{:x}", hasher.finalize());
        Ok(Self {
            size,
            content_hash: Some(content_hash),
            mtime_nanos: None,
        })
    }
}

/// A snapshot of every non-ignored file under a root directory.
#[derive(Debug, Clone, Default)]
pub struct FsSnapshot {
    pub root: PathBuf,
    /// Map from path *relative to root* → fingerprint.
    pub files: HashMap<PathBuf, FileFingerprint>,
}

/// Diff between two snapshots taken under the same root.
#[derive(Debug, Clone, Default)]
pub struct FsDiff {
    /// Files present in `after` but not `before`.
    pub added: Vec<PathBuf>,
    /// Files present in both, but with a different fingerprint.
    pub modified: Vec<PathBuf>,
    /// Files present in `before` but not `after`.
    pub deleted: Vec<PathBuf>,
}

impl FsDiff {
    /// Total number of changed paths (added + modified + deleted).
    pub fn total_changes(&self) -> usize {
        self.added.len() + self.modified.len() + self.deleted.len()
    }

    /// Whether any file was added, modified, or deleted.
    pub fn is_empty(&self) -> bool {
        self.total_changes() == 0
    }

    /// Paths an executor should report as "artifacts created or touched"
    /// (added + modified, sorted, deduplicated). Deleted paths are omitted
    /// because the artifact metadata semantics are "what exists now."
    pub fn artifact_paths(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self
            .added
            .iter()
            .chain(self.modified.iter())
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

/// Take a snapshot of every non-ignored regular file under `root`.
///
/// Honors `.gidignore` and `.gitignore` plus the built-in default ignore
/// list (target/, node_modules/, .git/, etc.). Additionally, the
/// `.gid/runtime/` directory is *always* skipped because ritual state files
/// live there and would otherwise be mistaken for "implement output."
///
/// I/O errors on individual files are logged and skipped — a snapshot is
/// best-effort by design; if we can't read a file, treating it as absent
/// will at worst cause a spurious "modified" report on the next snapshot,
/// which is preferable to aborting the whole phase.
pub fn snapshot_dir(root: &Path) -> FsSnapshot {
    let ignores = load_ignore_list(root);
    snapshot_dir_with(root, &ignores)
}

/// Same as [`snapshot_dir`] but with an explicit ignore list — exposed for
/// tests that want to control the ignore set independently of any on-disk
/// `.gidignore` file.
pub fn snapshot_dir_with(root: &Path, ignores: &IgnoreList) -> FsSnapshot {
    let mut files = HashMap::new();

    if !root.exists() {
        return FsSnapshot {
            root: root.to_path_buf(),
            files,
        };
    }

    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip ignored directories at descent time so we don't even
            // walk into target/, node_modules/, etc.
            let path = e.path();
            if path == root {
                return true;
            }
            // Always skip ritual runtime state — it's machine-managed and
            // will mutate during a ritual regardless of LLM behavior.
            if let Ok(rel) = path.strip_prefix(root) {
                if rel.starts_with(".gid/runtime") {
                    return false;
                }
            }
            let is_dir = e.file_type().is_dir();
            !ignores.is_ignored(path) || {
                // is_ignored above takes the absolute path; defensively also
                // check whether the *relative* path matches.
                if let Ok(rel) = path.strip_prefix(root) {
                    !ignores.should_ignore(&rel.to_string_lossy(), is_dir)
                } else {
                    true
                }
            }
        });

    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(?path, "snapshot: failed to stat file: {}", e);
                continue;
            }
        };
        match FileFingerprint::from_metadata_and_contents(&meta, path) {
            Ok(fp) => {
                files.insert(rel, fp);
            }
            Err(e) => {
                tracing::debug!(?path, "snapshot: failed to fingerprint file: {}", e);
            }
        }
    }

    FsSnapshot {
        root: root.to_path_buf(),
        files,
    }
}

/// Diff two snapshots. Both snapshots must have been taken under the same
/// root; if they aren't, the diff is computed against the relative paths
/// regardless and the caller is responsible for the mismatch.
pub fn diff_snapshots(before: &FsSnapshot, after: &FsSnapshot) -> FsDiff {
    let mut diff = FsDiff::default();

    for (path, after_fp) in &after.files {
        match before.files.get(path) {
            None => diff.added.push(path.clone()),
            Some(before_fp) if before_fp != after_fp => diff.modified.push(path.clone()),
            Some(_) => {}
        }
    }
    for path in before.files.keys() {
        if !after.files.contains_key(path) {
            diff.deleted.push(path.clone());
        }
    }

    diff.added.sort();
    diff.modified.sort();
    diff.deleted.sort();
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, contents: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(contents).unwrap();
    }

    #[test]
    fn empty_dir_yields_empty_snapshot() {
        let tmp = TempDir::new().unwrap();
        let snap = snapshot_dir(tmp.path());
        assert!(snap.files.is_empty());
    }

    #[test]
    fn snapshot_then_no_changes_yields_empty_diff() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/lib.rs", b"fn main() {}");
        write(tmp.path(), "README.md", b"# hi");

        let before = snapshot_dir(tmp.path());
        // No changes between snapshots.
        let after = snapshot_dir(tmp.path());
        let diff = diff_snapshots(&before, &after);
        assert!(diff.is_empty(), "expected no diff, got {:?}", diff);
    }

    #[test]
    fn detects_added_file() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/lib.rs", b"fn main() {}");
        let before = snapshot_dir(tmp.path());

        write(tmp.path(), "src/new.rs", b"fn new() {}");
        let after = snapshot_dir(tmp.path());

        let diff = diff_snapshots(&before, &after);
        assert_eq!(diff.added, vec![PathBuf::from("src/new.rs")]);
        assert!(diff.modified.is_empty());
        assert!(diff.deleted.is_empty());
    }

    #[test]
    fn detects_modified_file_by_content() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/lib.rs", b"fn main() {}");
        let before = snapshot_dir(tmp.path());

        // Rewrite with different contents, same length is fine because we
        // hash content.
        write(tmp.path(), "src/lib.rs", b"fn main(){};");
        let after = snapshot_dir(tmp.path());

        let diff = diff_snapshots(&before, &after);
        assert_eq!(diff.modified, vec![PathBuf::from("src/lib.rs")]);
        assert!(diff.added.is_empty());
        assert!(diff.deleted.is_empty());
    }

    #[test]
    fn detects_deleted_file() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/lib.rs", b"fn main() {}");
        write(tmp.path(), "src/extra.rs", b"// junk");
        let before = snapshot_dir(tmp.path());

        fs::remove_file(tmp.path().join("src/extra.rs")).unwrap();
        let after = snapshot_dir(tmp.path());

        let diff = diff_snapshots(&before, &after);
        assert_eq!(diff.deleted, vec![PathBuf::from("src/extra.rs")]);
        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn ignores_target_directory() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/lib.rs", b"fn main() {}");
        write(tmp.path(), "target/debug/build.o", b"binary garbage");
        let snap = snapshot_dir(tmp.path());

        let paths: Vec<_> = snap.files.keys().collect();
        assert!(
            paths.iter().any(|p| p.to_string_lossy() == "src/lib.rs"),
            "expected src/lib.rs in snapshot, got {:?}",
            paths
        );
        assert!(
            !paths.iter().any(|p| p.to_string_lossy().starts_with("target")),
            "target/ should be ignored, got {:?}",
            paths
        );
    }

    #[test]
    fn ignores_gid_runtime_directory() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/lib.rs", b"fn main() {}");
        write(tmp.path(), ".gid/runtime/rituals/r-abc.json", b"{}");
        let snap = snapshot_dir(tmp.path());

        let paths: Vec<String> = snap.files.keys().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(
            !paths.iter().any(|p| p.starts_with(".gid/runtime")),
            ".gid/runtime should be ignored, got {:?}",
            paths
        );
    }

    #[test]
    fn artifact_paths_combines_added_and_modified_sorted() {
        let diff = FsDiff {
            added: vec![PathBuf::from("b.rs"), PathBuf::from("a.rs")],
            modified: vec![PathBuf::from("c.rs")],
            deleted: vec![PathBuf::from("d.rs")],
        };
        let paths = diff.artifact_paths();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("a.rs"),
                PathBuf::from("b.rs"),
                PathBuf::from("c.rs"),
            ]
        );
    }

    #[test]
    fn nonexistent_root_yields_empty_snapshot() {
        let tmp = TempDir::new().unwrap();
        let bogus = tmp.path().join("does-not-exist");
        let snap = snapshot_dir(&bogus);
        assert!(snap.files.is_empty());
    }
}
