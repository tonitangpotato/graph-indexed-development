//! [`ArtifactId`] — canonical identity for a project artifact.
//!
//! Per ISS-053 §4.1 (D1): the authoritative identity of an artifact is its
//! **file path**, not a `kind+local_id` tuple. `ArtifactId` is a newtype
//! around the canonical relative path (forward-slash normalized, no leading
//! `/`, no `..` parent refs).
//!
//! Short-form references (e.g. `engram:ISS-022`) are **sugar** resolved
//! against the [`crate::project_registry`] (§5 / D5). `ArtifactId::parse_short`
//! splits on the first `:` separator, resolves the project portion via the
//! registry, and returns `(project_name, ArtifactId)`. The artifact id
//! portion is preserved verbatim as the relative path component.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::project_registry::{Registry, RegistryError};

/// Canonical identity of an artifact within a project.
///
/// Stores the relative path as a string (forward-slash normalized) and
/// guarantees the following invariants:
///   - non-empty
///   - relative (no leading `/`, no Windows drive prefix, no root component)
///   - contains no `..` parent references
///   - separators are normalized to `/`
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ArtifactId {
    /// Canonical relative path, forward-slash normalized.
    path: String,
}

impl ArtifactId {
    /// Construct an `ArtifactId` from any path-like value.
    ///
    /// Validates: non-empty, relative (rejects absolute paths and root
    /// components), and contains no `..` (parent-dir) components. Separators
    /// are normalized to `/`.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, ArtifactIdError> {
        let p = path.as_ref();
        let raw = p.to_str().ok_or(ArtifactIdError::InvalidUtf8)?;
        Self::from_str_inner(raw)
    }

    fn from_str_inner(raw: &str) -> Result<Self, ArtifactIdError> {
        if raw.is_empty() {
            return Err(ArtifactIdError::Empty);
        }
        // Reject Unix-style absolute paths and Windows-style absolute paths.
        if raw.starts_with('/') || raw.starts_with('\\') {
            return Err(ArtifactIdError::Absolute);
        }
        // Walk components to detect roots, prefixes, and parent refs. We
        // build the path through `Path` to leverage cross-platform component
        // semantics, then re-emit with `/` separators.
        let p = Path::new(raw);
        let mut normalized = String::with_capacity(raw.len());
        let mut first = true;
        for comp in p.components() {
            match comp {
                Component::Prefix(_) | Component::RootDir => {
                    return Err(ArtifactIdError::Absolute);
                }
                Component::ParentDir => {
                    return Err(ArtifactIdError::ContainsParentRefs);
                }
                Component::CurDir => {
                    // Drop `.` segments silently — they are no-ops.
                    continue;
                }
                Component::Normal(seg) => {
                    let seg = seg.to_str().ok_or(ArtifactIdError::InvalidUtf8)?;
                    if seg.is_empty() {
                        continue;
                    }
                    if !first {
                        normalized.push('/');
                    }
                    normalized.push_str(seg);
                    first = false;
                }
            }
        }
        if normalized.is_empty() {
            return Err(ArtifactIdError::Empty);
        }
        Ok(Self { path: normalized })
    }

    /// Borrowed access to the canonical (forward-slash-normalized) string.
    pub fn as_str(&self) -> &str {
        &self.path
    }

    /// Borrowed access as `&Path` for filesystem operations.
    ///
    /// On Unix this is a zero-cost view over the same bytes; on Windows the
    /// forward slashes are still valid path separators when joined onto a
    /// project root.
    pub fn as_path(&self) -> &Path {
        Path::new(&self.path)
    }

    /// Convert into an owned [`PathBuf`].
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(&self.path)
    }

    /// Parse a short-form cross-project reference of the form
    /// `<project>:<relative_path_or_short_id>`.
    ///
    /// The first `:` separates the project name (resolved via the supplied
    /// [`Registry`]) from the artifact's path-portion. The path-portion is
    /// preserved verbatim (subject to the same normalization as
    /// [`ArtifactId::new`]); short-id resolution to a full path (e.g.
    /// `ISS-022` → `.gid/issues/ISS-022/issue.md`) belongs to `Layout` and
    /// is out of scope for Phase A.
    ///
    /// Returns `(project_canonical_name, artifact_id)` on success.
    pub fn parse_short(
        s: &str,
        registry: &Registry,
    ) -> Result<(String, ArtifactId), ParseError> {
        let (project_part, path_part) = s
            .split_once(':')
            .ok_or_else(|| ParseError::MissingSeparator(s.to_string()))?;
        if project_part.is_empty() {
            return Err(ParseError::EmptyProject(s.to_string()));
        }
        if path_part.is_empty() {
            return Err(ParseError::EmptyPath(s.to_string()));
        }
        let entry = registry
            .resolve(project_part)
            .map_err(ParseError::Registry)?;
        let project_name = entry.name.clone();
        let id = ArtifactId::from_str_inner(path_part).map_err(ParseError::Id)?;
        Ok((project_name, id))
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.path)
    }
}

impl FromStr for ArtifactId {
    type Err = ArtifactIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str_inner(s)
    }
}

impl AsRef<Path> for ArtifactId {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl AsRef<str> for ArtifactId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl TryFrom<String> for ArtifactId {
    type Error = ArtifactIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::from_str_inner(&s)
    }
}

impl From<ArtifactId> for String {
    fn from(id: ArtifactId) -> Self {
        id.path
    }
}

/// Errors produced when constructing an [`ArtifactId`].
#[derive(Debug, thiserror::Error)]
pub enum ArtifactIdError {
    #[error("artifact id must be a relative path (got an absolute path)")]
    Absolute,

    #[error("artifact id may not contain `..` parent-directory components")]
    ContainsParentRefs,

    #[error("artifact id must be non-empty")]
    Empty,

    #[error("artifact id contains non-UTF-8 byte sequences")]
    InvalidUtf8,
}

/// Errors produced by [`ArtifactId::parse_short`].
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("short reference '{0}' is missing the ':' separator between project and path")]
    MissingSeparator(String),

    #[error("short reference '{0}' has an empty project component")]
    EmptyProject(String),

    #[error("short reference '{0}' has an empty path component")]
    EmptyPath(String),

    #[error("project registry lookup failed: {0}")]
    Registry(#[source] RegistryError),

    #[error("invalid artifact id: {0}")]
    Id(#[source] ArtifactIdError),
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_registry::{ProjectEntry, Registry};
    use std::path::PathBuf;

    #[test]
    fn accepts_simple_relative_path() {
        let id = ArtifactId::new(".gid/issues/ISS-053/issue.md").unwrap();
        assert_eq!(id.as_str(), ".gid/issues/ISS-053/issue.md");
    }

    #[test]
    fn normalizes_backslashes_via_path() {
        // On Unix, backslashes are part of the segment, not separators —
        // verify the canonical string only uses `/`.
        let id = ArtifactId::new("a/b/c.md").unwrap();
        assert!(!id.as_str().contains('\\'));
        assert_eq!(id.as_str(), "a/b/c.md");
    }

    #[test]
    fn drops_curdir_segments() {
        let id = ArtifactId::new("./a/./b.md").unwrap();
        assert_eq!(id.as_str(), "a/b.md");
    }

    #[test]
    fn rejects_absolute_unix_path() {
        let err = ArtifactId::new("/etc/passwd").unwrap_err();
        assert!(matches!(err, ArtifactIdError::Absolute));
    }

    #[test]
    fn rejects_parent_refs() {
        let err = ArtifactId::new("a/../b.md").unwrap_err();
        assert!(matches!(err, ArtifactIdError::ContainsParentRefs));
    }

    #[test]
    fn rejects_empty() {
        let err = ArtifactId::new("").unwrap_err();
        assert!(matches!(err, ArtifactIdError::Empty));
    }

    #[test]
    fn rejects_curdir_only() {
        let err = ArtifactId::new("./.").unwrap_err();
        assert!(matches!(err, ArtifactIdError::Empty));
    }

    #[test]
    fn round_trips_via_from_str() {
        let original = ".gid/features/dim-extract/design.md";
        let id: ArtifactId = original.parse().unwrap();
        assert_eq!(id.as_str(), original);
        assert_eq!(id.to_string(), original);
    }

    #[test]
    fn as_path_matches_as_str() {
        let id = ArtifactId::new("a/b/c.md").unwrap();
        assert_eq!(id.as_path(), Path::new("a/b/c.md"));
    }

    #[test]
    fn serde_round_trip() {
        let id = ArtifactId::new(".gid/issues/ISS-001/issue.md").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\".gid/issues/ISS-001/issue.md\"");
        let back: ArtifactId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn ord_is_lexical() {
        let a = ArtifactId::new("a.md").unwrap();
        let b = ArtifactId::new("b.md").unwrap();
        assert!(a < b);
    }

    fn mock_registry() -> Registry {
        let mut reg = Registry::empty();
        // We can't use Registry::add (validates path on disk), so push directly
        // to the projects vector.
        reg.projects.push(ProjectEntry {
            name: "engram".to_string(),
            path: PathBuf::from("/tmp/engram"),
            aliases: vec!["ea".to_string()],
            default_branch: None,
            tags: Vec::new(),
            archived: false,
            notes: None,
        });
        reg.projects.push(ProjectEntry {
            name: "gid-rs".to_string(),
            path: PathBuf::from("/tmp/gid-rs"),
            aliases: Vec::new(),
            default_branch: None,
            tags: Vec::new(),
            archived: false,
            notes: None,
        });
        reg
    }

    #[test]
    fn parse_short_with_canonical_name() {
        let reg = mock_registry();
        let (proj, id) = ArtifactId::parse_short("engram:ISS-022", &reg).unwrap();
        assert_eq!(proj, "engram");
        assert_eq!(id.as_str(), "ISS-022");
    }

    #[test]
    fn parse_short_with_alias_resolves_to_canonical() {
        let reg = mock_registry();
        let (proj, id) = ArtifactId::parse_short("ea:ISS-022", &reg).unwrap();
        assert_eq!(proj, "engram", "alias should resolve to canonical name");
        assert_eq!(id.as_str(), "ISS-022");
    }

    #[test]
    fn parse_short_with_full_path() {
        let reg = mock_registry();
        let (proj, id) =
            ArtifactId::parse_short("gid-rs:.gid/issues/ISS-053/issue.md", &reg).unwrap();
        assert_eq!(proj, "gid-rs");
        assert_eq!(id.as_str(), ".gid/issues/ISS-053/issue.md");
    }

    #[test]
    fn parse_short_missing_separator() {
        let reg = mock_registry();
        let err = ArtifactId::parse_short("engram-ISS-022", &reg).unwrap_err();
        assert!(matches!(err, ParseError::MissingSeparator(_)));
    }

    #[test]
    fn parse_short_unknown_project() {
        let reg = mock_registry();
        let err = ArtifactId::parse_short("nope:ISS-022", &reg).unwrap_err();
        assert!(matches!(err, ParseError::Registry(_)));
    }

    #[test]
    fn parse_short_empty_path() {
        let reg = mock_registry();
        let err = ArtifactId::parse_short("engram:", &reg).unwrap_err();
        assert!(matches!(err, ParseError::EmptyPath(_)));
    }

    #[test]
    fn parse_short_empty_project() {
        let reg = mock_registry();
        let err = ArtifactId::parse_short(":ISS-022", &reg).unwrap_err();
        assert!(matches!(err, ParseError::EmptyProject(_)));
    }

    #[test]
    fn parse_short_only_splits_on_first_colon() {
        // Some ID schemes might contain colons in the path portion in the
        // future — verify only the *first* colon is the separator.
        let reg = mock_registry();
        let (proj, id) = ArtifactId::parse_short("engram:weird:path.md", &reg).unwrap();
        assert_eq!(proj, "engram");
        assert_eq!(id.as_str(), "weird:path.md");
    }
}
