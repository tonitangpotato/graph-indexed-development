//! [`Artifact`] — file = id + metadata + body (per ISS-053 §4.3).
//!
//! Per ISS-053 §4.3, `kind` is derived from [`Layout`] (§4.4) at load time
//! and stored on the in-memory struct so callers (CLI / MCP / native tools)
//! can branch on it without re-running the Layout matcher. On disk, `kind`
//! is **not** present anywhere — the path itself is authoritative (D1).
//!
//! `load_raw` is a low-level helper that takes an explicit `kind` argument;
//! `ArtifactStore::get` (Phase D) is the high-level loader that resolves
//! `kind` automatically via the project's [`Layout`].

use std::path::Path;

use thiserror::Error;

use super::id::ArtifactId;
use super::metadata::{Metadata, MetadataError};

/// In-memory representation of an artifact: identity + metadata + body + kind.
///
/// `kind` is derived (not stored on disk) — set at load time by
/// [`Layout::match_path`](super::layout::Layout::match_path) inside
/// `ArtifactStore::get` / `::list`. For low-level construction (e.g. tests
/// or callers that already know the kind), use [`Artifact::load_raw`] with
/// an explicit `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: String,
    pub metadata: Metadata,
    pub body: String,
}

impl Artifact {
    /// Load an artifact's raw bytes from disk and parse metadata.
    ///
    /// `kind` is supplied by the caller (typically resolved via
    /// `Layout::match_path` upstream — `ArtifactStore::get` does this for
    /// you).
    pub fn load_raw(
        root: &Path,
        id: &ArtifactId,
        kind: impl Into<String>,
    ) -> Result<Self, ArtifactError> {
        let path = root.join(id.as_path());
        let raw = std::fs::read_to_string(&path).map_err(|source| ArtifactError::Io {
            path: path.clone(),
            source,
        })?;
        let (metadata, body) = Metadata::parse(&raw)?;
        Ok(Self {
            id: id.clone(),
            kind: kind.into(),
            metadata,
            body,
        })
    }

    /// Render the artifact to bytes ready for writing to disk.
    ///
    /// Round-trip safety (D4): if `metadata` is unmodified after parsing,
    /// `render()` produces byte-identical output to the original file.
    pub fn render(&self) -> String {
        let mut out = self.metadata.render();
        out.push_str(&self.body);
        out
    }
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Metadata(#[from] MetadataError),
}
