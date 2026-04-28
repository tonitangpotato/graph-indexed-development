//! [`Layout`] — pattern-driven artifact path layout (per ISS-053 §4.4, D2).
//!
//! `Layout` is **data, not code**: a list of [`LayoutPattern`]s that map
//! relative paths (e.g. `issues/ISS-0042/issue.md`) to artifact kinds (e.g.
//! `issue`) plus captured slots (e.g. `{ id: "ISS-0042", seq: "0042" }`).
//! The same pattern is bidirectional — used for both matching (path → kind +
//! slots) and rendering ([`Layout::resolve`] produces a path from a kind +
//! [`SlotMap`]).
//!
//! Patterns use a small, closed-set DSL (§4.4.1):
//!
//! ```text
//! issues/{id:ISS-{seq:04}}/issue.md       — sequenced ID, zero-padded
//! features/{slug}/requirements.md          — kebab-case slug
//! features/{slug}/reviews/{name}.md        — free-form basename
//! issues/{parent_id}/reviews/{name}.md     — nested under parent
//! issues/{parent_id}/{any}.md              — catch-all single segment
//! ```
//!
//! ## Adding a new kind
//!
//! Edit `.gid/layout.yml`, add a pattern. **No code change**. This is the
//! binding test for D2 (kind is a string, layout is data).
//!
//! ## Sequence overflow
//!
//! `{seq:NN}` allocations past `10^NN - 1` return
//! [`LayoutError::SeqExhausted`] — no silent rollover. Callers must widen
//! the layout (e.g. `seq:03` → `seq:04`) before creating new artifacts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::id::ArtifactId;
use super::metadata::MetaSourceHint;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Slot name → captured value. Populated by [`Layout::match_path`] and
/// consumed by [`Layout::resolve`] for path rendering.
pub type SlotMap = BTreeMap<String, String>;

/// Sequence counter scope for `{seq:NN}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeqScope {
    /// Counter scoped to the whole project (e.g., `ISS-N`).
    Project,
    /// Counter scoped to a parent directory (e.g., `r-N` per issue).
    /// `rel` is the relative path of the parent that owns the counter
    /// (typically captured via `{parent_id}` from the pattern).
    Parent { rel: String },
}

/// Default fallback when no pattern matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackRule {
    /// Default kind for unmatched files (e.g. `"note"`).
    pub kind: String,
    /// Metadata format used when creating new fallback files.
    pub metadata_format: MetaSourceHint,
}

/// One pattern in the layout. Glob-like with named captures and ID
/// generators. See module docs for examples and grammar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPattern {
    /// Pattern string, e.g. `"issues/{id:ISS-{seq:04}}/issue.md"`.
    pub pattern: String,
    /// Kind tag emitted on match (e.g. `"issue"`).
    pub kind: String,
    /// Metadata format used when creating new artifacts of this kind.
    pub metadata_format: MetaSourceHint,
    /// Sequence scope for any `{seq:NN}` placeholder in `pattern`.
    /// Defaults to [`SeqScope::Project`] when not specified.
    #[serde(default = "default_seq_scope")]
    pub seq_scope: SeqScope,
}

fn default_seq_scope() -> SeqScope {
    SeqScope::Project
}

impl LayoutPattern {
    /// Extract the inner template of any `{id:TEMPLATE}` placeholder in
    /// this pattern, e.g. for `"issues/{id:ISS-{seq:04}}/issue.md"` returns
    /// `Some("ISS-{seq:04}")`. Used by `ArtifactStore::next_id` to render a
    /// bare ID without a full path.
    pub fn id_template_str(&self) -> Option<String> {
        let bytes = self.pattern.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Find `{id:` then scan to matching `}` accounting for nested `{seq:NN}`.
            if bytes[i..].starts_with(b"{id:") {
                let start = i + 4;
                let mut depth = 1usize;
                let mut j = start;
                while j < bytes.len() {
                    match bytes[j] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(self.pattern[start..j].to_string());
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                return None; // unterminated
            }
            i += 1;
        }
        None
    }
}

/// The full layout — ordered list of patterns plus fallback + relation
/// fields. Patterns are tried in declaration order; first match wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub patterns: Vec<LayoutPattern>,
    pub fallback: FallbackRule,
    /// Frontmatter fields whose values (string or list of strings) are
    /// interpreted as `ArtifactId` references for relation discovery.
    /// Default via [`Layout::default`]. Overridable via `.gid/layout.yml`.
    #[serde(default = "default_relation_fields")]
    pub relation_fields: Vec<String>,
}

fn default_relation_fields() -> Vec<String> {
    vec![
        "related".to_string(),
        "blocks".to_string(),
        "blocked_by".to_string(),
        "supersedes".to_string(),
        "derives_from".to_string(),
        "applies_to".to_string(),
        "references".to_string(),
        "depends_on".to_string(),
        "satisfies".to_string(),
    ]
}

/// Result of matching a path against a layout: the matched kind plus the
/// captured slots. Returned by [`Layout::match_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    pub kind: String,
    pub slots: SlotMap,
    /// `true` if the match came from [`FallbackRule`] rather than an
    /// explicit pattern. Callers may treat fallback matches differently
    /// (e.g. `gid artifact list --strict` excludes them).
    pub fallback: bool,
}

/// Errors produced by [`Layout::resolve`] and sequence-allocation paths.
#[derive(Debug, Error)]
pub enum LayoutError {
    /// `kind` did not match any [`LayoutPattern`] in the layout.
    #[error("unknown kind: {0}")]
    UnknownKind(String),

    /// A required slot was not present in the [`SlotMap`].
    #[error("missing slot {slot:?} for kind {kind:?}")]
    MissingSlot { kind: String, slot: String },

    /// `{seq:NN}` would allocate past `10^NN - 1`. Widen the layout.
    #[error("sequence exhausted for pattern {pattern:?}: max value {max}")]
    SeqExhausted { pattern: String, max: u64 },

    /// Pattern itself is malformed (caught at layout-load time, not at
    /// match time, but kept here to consolidate error reporting).
    #[error("malformed pattern {pattern:?}: {message}")]
    MalformedPattern { pattern: String, message: String },
}

impl Default for Layout {
    /// The default layout, covering today's empirical `.gid/` reality
    /// across `engram`, `gid-rs`, `rustclaw` (sampled 2026-04-26 per §2).
    /// Projects override by writing `.gid/layout.yml`.
    fn default() -> Self {
        Self {
            patterns: default_patterns(),
            fallback: FallbackRule {
                kind: "note".to_string(),
                metadata_format: MetaSourceHint::None,
            },
            relation_fields: default_relation_fields(),
        }
    }
}

fn default_patterns() -> Vec<LayoutPattern> {
    use MetaSourceHint::*;
    vec![
        // Issues — sequenced ISS-NNNN
        LayoutPattern {
            pattern: "issues/{id:ISS-{seq:04}}/issue.md".to_string(),
            kind: "issue".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        // Issue-attached design / requirements / verify-report etc.
        LayoutPattern {
            pattern: "issues/{parent_id}/design.md".to_string(),
            kind: "design".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "issues/{parent_id}/requirements.md".to_string(),
            kind: "requirements".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        // Issue reviews — sequenced r-N within parent issue dir
        LayoutPattern {
            pattern: "issues/{parent_id}/reviews/{name}.md".to_string(),
            kind: "review".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Parent {
                rel: "issues/{parent_id}/reviews".to_string(),
            },
        },
        // Catch-all for issue-attached docs (verify-report, handoff, etc.)
        LayoutPattern {
            pattern: "issues/{parent_id}/{any}.md".to_string(),
            kind: "issue-doc".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        // Features
        LayoutPattern {
            pattern: "features/{slug}/requirements.md".to_string(),
            kind: "requirements".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "features/{slug}/design.md".to_string(),
            kind: "design".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "features/{slug}/reviews/{name}.md".to_string(),
            kind: "review".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Parent {
                rel: "features/{slug}/reviews".to_string(),
            },
        },
        LayoutPattern {
            pattern: "features/{slug}/{any}.md".to_string(),
            kind: "feature-doc".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        // Top-level docs (.gid/docs/*) and reviews (.gid/reviews/*)
        LayoutPattern {
            pattern: "docs/{name}.md".to_string(),
            kind: "doc".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "reviews/{name}.md".to_string(),
            kind: "review".to_string(),
            metadata_format: Frontmatter,
            seq_scope: SeqScope::Project,
        },
        // Ad-hoc subdir notes (e.g., .gid/sqlite-migration/, .gid/incremental-extract/)
        LayoutPattern {
            pattern: "{slug}/{any}.md".to_string(),
            kind: "note".to_string(),
            metadata_format: MetaSourceHint::None,
            seq_scope: SeqScope::Project,
        },
    ]
}

impl Layout {
    /// Configured relation field names — frontmatter fields whose values are
    /// interpreted as `ArtifactId` references during relation discovery.
    pub fn relation_fields(&self) -> &[String] {
        &self.relation_fields
    }
}
