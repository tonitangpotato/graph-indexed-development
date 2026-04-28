//! Artifact model — first-class project artifacts (issues, features, designs,
//! reviews, …) for `gid-core`.
//!
//! Implements the foundation types described in ISS-053 §4:
//!   - [`ArtifactId`] (§4.1) — identity (project + relative path).
//!   - [`Metadata`] (§4.2) — tolerant, round-trip-preserving frontmatter /
//!     markdown-header parser.
//!   - [`Artifact`] (§4.3) — file = id + metadata + body.
//!
//! ## Phasing note
//!
//! Per ISS-053 phasing (Phase A, foundation types only), [`Artifact`] does
//! **not** carry a `kind: String` field yet. `kind` is derived from `Layout`
//! (§4.4), which is implemented in Phase B. The `kind` field will be attached
//! by `ArtifactStore::open` / `::get` (Phase D) once `Layout` exists, in
//! keeping with the design's "kind is derived, not stored on disk"
//! invariant (D1). This is a phasing detail, not a design deviation.

pub mod artifact;
pub mod id;
pub mod layout;
pub mod metadata;
pub mod relation;
pub mod store;

pub use artifact::{Artifact, ArtifactError};
pub use id::{ArtifactId, ArtifactIdError};
pub use layout::{
    FallbackRule, Layout, LayoutError, LayoutPattern, MatchResult, SeqScope, SlotMap,
};
pub use metadata::{FieldValue, MetaSourceHint, Metadata, MetadataError};
pub use relation::{discover, Relation, RelationError, RelationSource};
pub use store::{ArtifactStore, RelationIndex, StoreError};
