//! [`Artifact`] — file = id + metadata + body (per ISS-053 §4.3).
//!
//! ## Phasing
//!
//! Phase A implements the foundation type **without** the `kind: String`
//! field. Per ISS-053 §4.3, `kind` is derived from [`Layout`] (§4.4), and
//! attaching it requires Phase B. The kind field will be set by
//! `ArtifactStore::open` / `::get` (Phase D) once Layout exists.
//!
//! This is a phasing detail, not a design deviation: `Artifact` on disk
//! has no `kind` field anyway (D1 — kind is derived from path patterns).

use std::path::Path;

use thiserror::Error;

use super::id::ArtifactId;
use super::metadata::{Metadata, MetadataError};

/// In-memory representation of an artifact: identity + metadata + body.
///
/// `kind` is **not** stored here — it is derived from the artifact's id by
/// [`crate::artifact::Layout`] (Phase B) and attached by `ArtifactStore`
/// (Phase D).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub id: ArtifactId,
    pub metadata: Metadata,
    pub body: String,
}

impl Artifact {
    /// Load an artifact's raw bytes from disk and parse metadata.
    ///
    /// `kind` resolution happens later — see module docs.
    pub fn load_raw(root: &Path, id: &ArtifactId) -> Result<Self, ArtifactError> {
        let path = root.join(id.as_path());
        let raw = std::fs::read_to_string(&path).map_err(|source| ArtifactError::Io {
            path: path.clone(),
            source,
        })?;
        let (metadata, body) = Metadata::parse(&raw)?;
        Ok(Self {
            id: id.clone(),
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
