//! Relation discovery — derive [`Relation`]s from artifact contents (ISS-053 §4.5).
//!
//! Per **D3** (relations are 100% derived from artifact files; no separate
//! persistence), this module is a *pure function over an [`Artifact`] +
//! [`Layout`]*. There is no relation store. Discovery is deterministic and
//! cheap; callers (e.g. `ArtifactStore::relations_from`) cache results
//! by directory mtime.
//!
//! ## Discovery rules (per ISS-053 §4.5)
//!
//! 1. **Frontmatter fields** matching [`Layout::relation_fields`] (default:
//!    `related`, `blocks`, `blocked_by`, `supersedes`, `derives_from`,
//!    `applies_to`, `references`). Each value parsed as an [`ArtifactId`].
//! 2. **Markdown links** of the form `[anything](relative_or_absolute_path)`
//!    where the target is a `.md` under `.gid/`. URL fragments and external
//!    URLs are ignored.
//! 3. **Inline backtick refs** matching `` `<short_form>` `` parseable by
//!    [`ArtifactId::parse_short`] via a [`Registry`].
//! 4. **Directory nesting**: any artifact under `<X>/reviews/Y.md` emits
//!    `Relation { from: Y, to: X, kind: "reviews", source: DirectoryNesting }`.
//!
//! ## Heuristic discipline (risk mitigation)
//!
//! Per ISS-053 §9 risks: false positives are worse than false negatives.
//! Both link- and backtick-based discovery only emit a relation when the
//! captured target *successfully* parses as an `ArtifactId` (rule 2) or
//! short-form (rule 3). Anything else is silently ignored.

use std::path::{Component, PathBuf};

use regex::Regex;
use thiserror::Error;

use super::artifact::Artifact;
use super::id::{ArtifactId, ArtifactIdError};
use super::layout::Layout;
use super::metadata::FieldValue;
use crate::project_registry::Registry;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A directed edge between two artifacts, with provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub from: ArtifactId,
    pub to: ArtifactId,
    /// Edge kind. Drawn from frontmatter field name (`blocks`, `related`, …),
    /// `"link"` for markdown-link discovery, or `"reviews"` for nesting.
    pub kind: String,
    pub source: RelationSource,
}

/// Where a [`Relation`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationSource {
    /// From a frontmatter field (`field` is the YAML key, e.g. `"blocks"`).
    Frontmatter { field: String },
    /// From a `[text](path)` markdown link.
    MarkdownLink,
    /// From a `` `short_form` `` backtick reference.
    BacktickRef,
    /// From `<X>/reviews/Y.md` directory nesting (Y reviews X).
    DirectoryNesting,
}

#[derive(Debug, Error)]
pub enum RelationError {
    #[error("invalid artifact id in frontmatter field {field:?}: {source}")]
    FrontmatterIdParse {
        field: String,
        #[source]
        source: ArtifactIdError,
    },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Discover all outgoing relations from `artifact`.
///
/// Aggregates the four discovery rules. The supplied `layout` controls which
/// frontmatter fields are scanned (rule 1). The `registry`, when provided,
/// enables cross-project backtick refs (rule 3); pass `None` to skip that
/// rule (e.g. in unit tests with no project registry).
///
/// **Determinism:** results are returned in discovery order
/// (frontmatter → markdown-link → backtick → nesting). Within each rule,
/// order follows the artifact's natural order (field declaration order /
/// body byte order).
///
/// **Errors:** only frontmatter id parse failures are surfaced; ill-formed
/// markdown links and unparseable backtick refs are silently dropped (false
/// positives are strictly worse than false negatives — see module docs).
pub fn discover(
    artifact: &Artifact,
    layout: &Layout,
    registry: Option<&Registry>,
) -> Result<Vec<Relation>, RelationError> {
    let mut out = Vec::new();
    out.extend(from_frontmatter(artifact, layout)?);
    out.extend(from_markdown_links(artifact));
    if let Some(reg) = registry {
        out.extend(from_backtick_refs(artifact, reg));
    }
    if let Some(rel) = from_directory_nesting(artifact) {
        out.push(rel);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Rule 1 — frontmatter
// ---------------------------------------------------------------------------

/// Discover relations from frontmatter fields named in `layout.relation_fields`.
///
/// Each field's value (scalar or list) is parsed as a relative path via
/// [`ArtifactId::new`]. A scalar holding a list-shaped string is *not*
/// re-parsed — it is treated as a single id. (Multi-id values should use a
/// YAML list.)
pub fn from_frontmatter(
    artifact: &Artifact,
    layout: &Layout,
) -> Result<Vec<Relation>, RelationError> {
    let mut out = Vec::new();
    for field in layout.relation_fields() {
        let Some(value) = artifact.metadata.get(field) else {
            continue;
        };
        let candidates: Vec<&str> = match value {
            FieldValue::Scalar(s) => {
                if s.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![s.as_str()]
                }
            }
            FieldValue::List(items) => items
                .iter()
                .map(String::as_str)
                .filter(|s| !s.trim().is_empty())
                .collect(),
        };
        for raw in candidates {
            let id = ArtifactId::new(raw.trim()).map_err(|source| {
                RelationError::FrontmatterIdParse {
                    field: field.clone(),
                    source,
                }
            })?;
            out.push(Relation {
                from: artifact.id.clone(),
                to: id,
                kind: field.clone(),
                source: RelationSource::Frontmatter {
                    field: field.clone(),
                },
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Rule 2 — markdown links
// ---------------------------------------------------------------------------

/// Match `[text](target)` where `target` does not start with a URL scheme
/// or `#`. We post-filter the captured target on `.md` extension under
/// `.gid/`, and on successful `ArtifactId::new` parse.
fn markdown_link_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // [text](target) — non-greedy text and target. We exclude a leading
        // `!` (image) and disallow whitespace inside `target` (markdown spec
        // forbids unescaped spaces in autolink targets without `<>`).
        Regex::new(r"(?m)(?:^|[^!])\[[^\]]*\]\(([^)\s]+)\)").expect("static regex")
    })
}

/// Discover relations from `[text](path.md)` links pointing at artifacts.
///
/// Filters applied:
/// - Skip targets containing `://` (external URLs: `http://`, `mailto:` …).
/// - Skip pure fragments (`#section`).
/// - Strip a trailing `#fragment` from the target before id parsing.
/// - Require `.md` extension (case-insensitive).
/// - Resolve relative targets against the artifact's *directory*; absolute
///   `/`-rooted targets are interpreted as project-root-relative (the leading
///   `/` is stripped before normalization).
/// - Require the resolved path to be inside `.gid/` (the artifact corpus).
/// - Require [`ArtifactId::new`] to accept the resolved path.
///
/// All filter failures are silent — they produce no `Relation`.
pub fn from_markdown_links(artifact: &Artifact) -> Vec<Relation> {
    let re = markdown_link_regex();
    let mut out = Vec::new();
    let artifact_dir = artifact_dir_id(&artifact.id);

    for cap in re.captures_iter(&artifact.body) {
        let raw_target = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let Some(id) = parse_link_target(raw_target, artifact_dir.as_deref()) else {
            continue;
        };
        // Don't emit self-links.
        if id == artifact.id {
            continue;
        }
        out.push(Relation {
            from: artifact.id.clone(),
            to: id,
            kind: "link".to_string(),
            source: RelationSource::MarkdownLink,
        });
    }
    out
}

/// Resolve a markdown-link `target` (relative to `artifact_dir`) into an
/// [`ArtifactId`], applying the filter chain documented on
/// [`from_markdown_links`]. Returns `None` if any filter rejects.
fn parse_link_target(raw: &str, artifact_dir: Option<&str>) -> Option<ArtifactId> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // External URLs: contain a scheme like `http://`, `https://`, `mailto:`,
    // etc. Strip any single trailing `>` from autolinks.
    if raw.contains("://") || raw.starts_with("mailto:") || raw.starts_with('#') {
        return None;
    }
    // Drop fragments + query strings.
    let target = raw.split(['#', '?']).next().unwrap_or("");
    if target.is_empty() {
        return None;
    }
    // Require `.md` (case-insensitive). `.md/` (trailing slash) is not a file.
    if !target.to_ascii_lowercase().ends_with(".md") {
        return None;
    }
    // Resolve.
    let resolved = if let Some(stripped) = target.strip_prefix('/') {
        // Project-root-relative.
        stripped.to_string()
    } else if let Some(dir) = artifact_dir {
        format!("{dir}/{target}")
    } else {
        target.to_string()
    };
    let normalized = normalize_relative(&resolved)?;
    // Require inside `.gid/`.
    if !normalized.starts_with(".gid/") {
        return None;
    }
    ArtifactId::new(&normalized).ok()
}

/// Directory portion of an artifact id (everything before the last `/`),
/// or `None` if the id is a top-level file.
fn artifact_dir_id(id: &ArtifactId) -> Option<String> {
    let s = id.as_str();
    s.rfind('/').map(|i| s[..i].to_string())
}

/// Normalize a `/`-separated relative path: collapse `.` / `..` segments,
/// fail (return `None`) if `..` would escape the root.
fn normalize_relative(raw: &str) -> Option<String> {
    // Build a virtual stack and walk components. We use `Path::components`
    // for portability, but operate on `/`-style results.
    let p = PathBuf::from(raw);
    let mut stack: Vec<String> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.pop().is_none() {
                    return None;
                }
            }
            Component::Normal(seg) => {
                let s = seg.to_str()?;
                if s.is_empty() {
                    continue;
                }
                stack.push(s.to_string());
            }
            // RootDir / Prefix should not appear (we stripped a leading `/`
            // before calling). If they do, treat as escape.
            _ => return None,
        }
    }
    if stack.is_empty() {
        return None;
    }
    Some(stack.join("/"))
}

// ---------------------------------------------------------------------------
// Rule 3 — backtick refs
// ---------------------------------------------------------------------------

/// Match inline backtick spans: `` `…` ``. We then attempt
/// [`ArtifactId::parse_short`] on the captured contents; non-matches are
/// silently dropped.
fn backtick_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Non-greedy single-backtick span. Multi-backtick code blocks are
        // matched as a sequence of single spans, but their contents rarely
        // parse as short-form ids — collateral damage is acceptable.
        Regex::new(r"`([^`\n]+)`").expect("static regex")
    })
}

/// Discover backtick refs of the shape `` `<project>:<path>` ``.
///
/// We rely on [`ArtifactId::parse_short`] to validate; anything that fails
/// to parse is dropped. The emitted [`Relation`] uses the *resolved local*
/// id when the project happens to match the artifact's own — but at this
/// layer we don't know the artifact's project (that's `ArtifactStore`'s
/// responsibility, Phase D). We therefore record the **target's local
/// path** verbatim; cross-project resolution and project tagging is added
/// at the store layer.
pub fn from_backtick_refs(artifact: &Artifact, registry: &Registry) -> Vec<Relation> {
    let re = backtick_regex();
    let mut out = Vec::new();
    for cap in re.captures_iter(&artifact.body) {
        let inner = cap.get(1).map(|m| m.as_str()).unwrap_or("").trim();
        if inner.is_empty() {
            continue;
        }
        // Only attempt parse_short if there's a `:` separator. (Otherwise
        // every inline `code` token would hit the parser.)
        if !inner.contains(':') {
            continue;
        }
        match ArtifactId::parse_short(inner, registry) {
            Ok((_project, target)) => {
                if target == artifact.id {
                    continue;
                }
                out.push(Relation {
                    from: artifact.id.clone(),
                    to: target,
                    kind: "ref".to_string(),
                    source: RelationSource::BacktickRef,
                });
            }
            Err(_) => {
                // Silent drop — false-positive avoidance (module-level rule).
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Rule 4 — directory nesting
// ---------------------------------------------------------------------------

/// Detect `<X>/reviews/<Y>.md` patterns and emit a `reviews` relation.
///
/// The `from` is `Y` (the review file), the `to` is the *primary* artifact
/// of `<X>`. Phase C resolves the primary artifact name by **path shape
/// only** (no filesystem inspection):
///
/// - `.gid/issues/<X>/reviews/Y.md` → `to = .gid/issues/<X>/issue.md`
/// - `.gid/features/<X>/reviews/Y.md` → `to = .gid/features/<X>/feature.md`
/// - other shapes → no relation emitted (Phase D's `ArtifactStore` will
///   tighten this by consulting `Layout::list` on the parent dir).
///
/// **Heuristic note:** the path-shape mapping is intentionally conservative.
/// Emitting a wrong `to` (false positive) is worse than missing it (§9
/// risks); when in doubt we drop. The two shapes above cover all current
/// review locations across engram / gid-rs / rustclaw `.gid/` corpora.
pub fn from_directory_nesting(artifact: &Artifact) -> Option<Relation> {
    let path = artifact.id.as_str();
    let segments: Vec<&str> = path.split('/').collect();
    // Need: …/<X>/reviews/<Y>.md  → at least 4 segments and "reviews" at -2.
    if segments.len() < 4 {
        return None;
    }
    if segments[segments.len() - 2] != "reviews" {
        return None;
    }
    // Path shape: <gid_dir>/<container>/<X>/reviews/<Y>.md
    // We need to look at `container` (segments[-4]) to decide the primary
    // artifact name.
    let container = segments[segments.len() - 4];
    let primary_name = match container {
        "issues" => "issue.md",
        "features" => "feature.md",
        _ => return None,
    };
    // Parent dir = everything up to (and including) `<X>`.
    let parent_dir_segments = &segments[..segments.len() - 2];
    let parent_dir = parent_dir_segments.join("/");
    let candidate = format!("{parent_dir}/{primary_name}");
    let parent_id = ArtifactId::new(&candidate).ok()?;
    Some(Relation {
        from: artifact.id.clone(),
        to: parent_id,
        kind: "reviews".to_string(),
        source: RelationSource::DirectoryNesting,
    })
}

// ---------------------------------------------------------------------------
// Tests — populated incrementally per discovery rule.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Unit tests are added in dedicated submodules per rule.
    use super::*;
    use crate::artifact::metadata::{MetaSourceHint, Metadata};

    fn mk_artifact(path: &str, body: &str) -> Artifact {
        Artifact {
            id: ArtifactId::new(path).unwrap(),
            metadata: Metadata::new(MetaSourceHint::None),
            body: body.to_string(),
        }
    }

    fn mk_artifact_with_meta(path: &str, meta: Metadata, body: &str) -> Artifact {
        Artifact {
            id: ArtifactId::new(path).unwrap(),
            metadata: meta,
            body: body.to_string(),
        }
    }

    // ----- Rule 4: directory nesting -----

    #[test]
    fn nesting_emits_reviews_relation_with_issue_parent() {
        let a = mk_artifact(".gid/issues/ISS-053/reviews/design-r1.md", "");
        let rel = from_directory_nesting(&a).expect("must detect");
        assert_eq!(rel.from.as_str(), ".gid/issues/ISS-053/reviews/design-r1.md");
        assert_eq!(rel.to.as_str(), ".gid/issues/ISS-053/issue.md");
        assert_eq!(rel.kind, "reviews");
        assert_eq!(rel.source, RelationSource::DirectoryNesting);
    }

    #[test]
    fn nesting_no_reviews_dir_returns_none() {
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", "");
        assert!(from_directory_nesting(&a).is_none());
    }

    #[test]
    fn nesting_emits_with_feature_parent() {
        let a = mk_artifact(".gid/features/auth/reviews/r1.md", "");
        let rel = from_directory_nesting(&a).expect("must detect");
        assert_eq!(rel.to.as_str(), ".gid/features/auth/feature.md");
    }

    #[test]
    fn nesting_too_shallow_returns_none() {
        let a = mk_artifact("reviews/r1.md", "");
        assert!(from_directory_nesting(&a).is_none());
    }

    #[test]
    fn nesting_unknown_container_returns_none() {
        // We only emit for `issues/` or `features/` containers — anything
        // else (custom shapes, top-level reviews) is dropped to avoid false
        // positives.
        let a = mk_artifact(".gid/notes/foo/reviews/r1.md", "");
        assert!(from_directory_nesting(&a).is_none());
    }

    // ----- Rule 1: frontmatter -----

    fn meta_with(field: &str, value: FieldValue) -> Metadata {
        let mut m = Metadata::new(MetaSourceHint::Frontmatter);
        m.set_field(field, value);
        m
    }

    #[test]
    fn frontmatter_scalar_emits_relation() {
        let layout = Layout::default();
        let meta = meta_with(
            "blocks",
            FieldValue::Scalar(".gid/issues/ISS-051/issue.md".to_string()),
        );
        let a = mk_artifact_with_meta(".gid/issues/ISS-053/issue.md", meta, "");
        let rels = from_frontmatter(&a, &layout).unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].kind, "blocks");
        assert_eq!(rels[0].to.as_str(), ".gid/issues/ISS-051/issue.md");
        assert_eq!(
            rels[0].source,
            RelationSource::Frontmatter {
                field: "blocks".to_string()
            }
        );
    }

    #[test]
    fn frontmatter_list_emits_one_per_value() {
        let layout = Layout::default();
        let meta = meta_with(
            "related",
            FieldValue::List(vec![
                ".gid/issues/ISS-051/issue.md".to_string(),
                ".gid/issues/ISS-052/issue.md".to_string(),
            ]),
        );
        let a = mk_artifact_with_meta(".gid/issues/ISS-053/issue.md", meta, "");
        let rels = from_frontmatter(&a, &layout).unwrap();
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].to.as_str(), ".gid/issues/ISS-051/issue.md");
        assert_eq!(rels[1].to.as_str(), ".gid/issues/ISS-052/issue.md");
        assert!(rels.iter().all(|r| r.kind == "related"));
    }

    #[test]
    fn frontmatter_skips_non_relation_fields() {
        let layout = Layout::default();
        // `status` is not a relation field; should be ignored.
        let meta = meta_with("status", FieldValue::Scalar("open".to_string()));
        let a = mk_artifact_with_meta(".gid/issues/ISS-053/issue.md", meta, "");
        let rels = from_frontmatter(&a, &layout).unwrap();
        assert!(rels.is_empty());
    }

    #[test]
    fn frontmatter_empty_value_is_skipped() {
        let layout = Layout::default();
        let meta = meta_with("blocks", FieldValue::Scalar("   ".to_string()));
        let a = mk_artifact_with_meta(".gid/issues/ISS-053/issue.md", meta, "");
        let rels = from_frontmatter(&a, &layout).unwrap();
        assert!(rels.is_empty());
    }

    #[test]
    fn frontmatter_invalid_id_is_an_error() {
        let layout = Layout::default();
        // Absolute path is rejected by ArtifactId::new.
        let meta = meta_with(
            "blocks",
            FieldValue::Scalar("/etc/passwd".to_string()),
        );
        let a = mk_artifact_with_meta(".gid/issues/ISS-053/issue.md", meta, "");
        let err = from_frontmatter(&a, &layout).unwrap_err();
        match err {
            RelationError::FrontmatterIdParse { field, .. } => {
                assert_eq!(field, "blocks");
            }
        }
    }

    #[test]
    fn frontmatter_value_is_trimmed_before_parse() {
        let layout = Layout::default();
        let meta = meta_with(
            "blocks",
            FieldValue::Scalar("  .gid/issues/ISS-051/issue.md  ".to_string()),
        );
        let a = mk_artifact_with_meta(".gid/issues/ISS-053/issue.md", meta, "");
        let rels = from_frontmatter(&a, &layout).unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to.as_str(), ".gid/issues/ISS-051/issue.md");
    }

    // ----- Rule 2: markdown links -----

    #[test]
    fn link_relative_path_resolves_against_artifact_dir() {
        let body = "See [the design](design.md) for details.";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        let rels = from_markdown_links(&a);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to.as_str(), ".gid/issues/ISS-053/design.md");
        assert_eq!(rels[0].kind, "link");
        assert_eq!(rels[0].source, RelationSource::MarkdownLink);
    }

    #[test]
    fn link_absolute_path_is_root_relative() {
        let body = "Cross-ref: [foo](/.gid/features/auth/feature.md).";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        let rels = from_markdown_links(&a);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to.as_str(), ".gid/features/auth/feature.md");
    }

    #[test]
    fn link_parent_dir_normalizes() {
        let body = "Up: [other](../ISS-051/issue.md).";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        let rels = from_markdown_links(&a);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to.as_str(), ".gid/issues/ISS-051/issue.md");
    }

    #[test]
    fn link_external_urls_dropped() {
        let body = "[google](https://google.com) [mail](mailto:a@b.com)";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        assert!(from_markdown_links(&a).is_empty());
    }

    #[test]
    fn link_outside_gid_dir_dropped() {
        // From .gid/issues/ISS-053/issue.md, going up 3 levels reaches the
        // project root, then `README.md` — outside .gid/, so dropped.
        let body = "[readme](../../../README.md)";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        assert!(from_markdown_links(&a).is_empty());
    }

    #[test]
    fn link_non_md_extension_dropped() {
        let body = "[code](src/main.rs) [conf](Cargo.toml)";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        assert!(from_markdown_links(&a).is_empty());
    }

    #[test]
    fn link_with_fragment_strips_fragment() {
        let body = "[sec](design.md#decisions)";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        let rels = from_markdown_links(&a);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to.as_str(), ".gid/issues/ISS-053/design.md");
    }

    #[test]
    fn link_image_syntax_dropped() {
        let body = "![alt](diagram.png) ![](pic.md)"; // image, not link
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        assert!(from_markdown_links(&a).is_empty());
    }

    #[test]
    fn link_self_reference_dropped() {
        let body = "[self](issue.md)";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        assert!(from_markdown_links(&a).is_empty());
    }

    #[test]
    fn link_parent_escape_returns_none() {
        // Going up too many levels — should normalize to None and be dropped.
        let body = "[bad](../../../../../etc.md)";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        assert!(from_markdown_links(&a).is_empty());
    }

    // ----- Rule 3: backtick refs -----

    fn registry_with_self_project(name: &str, root: &std::path::Path) -> Registry {
        // Build a registry that resolves `name` to `root`. We construct
        // the on-disk shape directly because `Registry::empty()` produces
        // an empty list and there is no public mutator.
        use crate::project_registry::{ProjectEntry, Registry};
        Registry {
            version: crate::project_registry::SCHEMA_VERSION,
            projects: vec![ProjectEntry {
                name: name.to_string(),
                path: root.to_path_buf(),
                aliases: Vec::new(),
                default_branch: None,
                tags: Vec::new(),
                archived: false,
                notes: None,
            }],
        }
    }

    #[test]
    fn backtick_ref_with_short_form_emits_relation() {
        // Body has a backtick-wrapped short ref like `myproj:.gid/issues/ISS-051/issue.md`.
        let dir = std::path::Path::new("/tmp/relation-test-proj");
        let registry = registry_with_self_project("myproj", dir);
        let body = "See `myproj:.gid/issues/ISS-051/issue.md` for context.";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        let rels = from_backtick_refs(&a, &registry);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to.as_str(), ".gid/issues/ISS-051/issue.md");
        assert_eq!(rels[0].source, RelationSource::BacktickRef);
        assert_eq!(rels[0].kind, "ref");
    }

    #[test]
    fn backtick_no_colon_is_skipped() {
        let dir = std::path::Path::new("/tmp/relation-test-proj");
        let registry = registry_with_self_project("myproj", dir);
        let body = "Use the `clone()` method and `Vec<T>` types.";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        assert!(from_backtick_refs(&a, &registry).is_empty());
    }

    #[test]
    fn backtick_unknown_project_dropped() {
        let dir = std::path::Path::new("/tmp/relation-test-proj");
        let registry = registry_with_self_project("myproj", dir);
        let body = "From `otherproj:foo.md` somewhere.";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        // otherproj isn't in the registry → silently dropped.
        assert!(from_backtick_refs(&a, &registry).is_empty());
    }

    // ----- discover(): aggregate -----

    #[test]
    fn discover_aggregates_all_rules_in_order() {
        let layout = Layout::default();
        let meta = meta_with(
            "blocks",
            FieldValue::Scalar(".gid/issues/ISS-051/issue.md".to_string()),
        );
        let body = "See [other](../ISS-052/issue.md).";
        let a = mk_artifact_with_meta(".gid/issues/ISS-053/issue.md", meta, body);
        let rels = discover(&a, &layout, None).unwrap();
        // Frontmatter (1) + link (1) + nesting (0, this is issue.md not in reviews/).
        assert_eq!(rels.len(), 2);
        assert!(matches!(
            rels[0].source,
            RelationSource::Frontmatter { .. }
        ));
        assert_eq!(rels[1].source, RelationSource::MarkdownLink);
    }

    #[test]
    fn discover_includes_nesting_for_review_files() {
        let layout = Layout::default();
        let a = mk_artifact(
            ".gid/issues/ISS-053/reviews/design-r1.md",
            "no body refs",
        );
        let rels = discover(&a, &layout, None).unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].source, RelationSource::DirectoryNesting);
        assert_eq!(rels[0].to.as_str(), ".gid/issues/ISS-053/issue.md");
    }

    #[test]
    fn discover_without_registry_skips_backtick_rule() {
        let layout = Layout::default();
        let body = "See `myproj:.gid/issues/ISS-051/issue.md`.";
        let a = mk_artifact(".gid/issues/ISS-053/issue.md", body);
        let rels = discover(&a, &layout, None).unwrap();
        // Backtick rule skipped because registry=None; no other rules match.
        assert!(rels.is_empty());
    }
}
