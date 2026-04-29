//! Back-fill `doc_path` on existing graph nodes (ISS-058 §3.4).
//!
//! When the v1 → v2 schema migration runs (`storage::schema::apply_migrations`),
//! it only adds the `doc_path` column. Existing rows are left at `NULL`. This
//! module provides convention-based inference that walks every node with
//! `doc_path IS NULL` and proposes (dry-run) or applies a value.
//!
//! # Inference rules (canonical — see design.md §3.3 / §3.4)
//!
//! | node_type           | id form          | inferred doc_path                                |
//! |---------------------|------------------|--------------------------------------------------|
//! | `issue`             | `ISS-NNN`        | `.gid/issues/ISS-NNN/issue.md`                   |
//! | `feature`           | `<slug>`         | `.gid/features/<slug>/design.md`                 |
//! | `design`            | `<slug>` or     | `.gid/features/<slug>/design.md`                 |
//! |                     | `<slug>/r<N>`    | (the `/rN` suffix is informational)              |
//! | `review`            | `<feat>/<name>`  | `.gid/features/<feat>/reviews/<name>.md`         |
//! | `task` / `code` /   | —                | NULL (no canonical authored artifact)            |
//! | `function` / etc.   |                  |                                                  |
//!
//! # Classification
//!
//! Every node falls into exactly one bucket:
//! - **fillable**       — convention applies AND target file exists on disk.
//! - **skipped-missing** — convention applies but the file is absent.
//! - **skipped-no-rule** — node type has no canonical artifact (legitimate NULL).
//! - **already-set**    — `doc_path` is already non-NULL (untouched).
//!
//! The CLI surface (`gid backfill-doc-path`) is dry-run by default; `--apply`
//! is required to write. Plan generation is pure (`plan_backfill`) so it can
//! be unit-tested without a database.

use crate::graph::Node;
use std::path::{Path, PathBuf};

/// Outcome bucket for a single node during back-fill planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillOutcome {
    /// Convention applies AND the inferred file exists on disk → would set `doc_path`.
    Fillable { inferred_path: String },
    /// Convention applies but the file is missing on disk → leave NULL, warn.
    SkippedMissing { inferred_path: String },
    /// Node type has no canonical artifact (e.g. `task`, `code`, `function`).
    SkippedNoRule,
    /// `doc_path` is already non-NULL → untouched.
    AlreadySet { existing: String },
}

impl BackfillOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            BackfillOutcome::Fillable { .. } => "fillable",
            BackfillOutcome::SkippedMissing { .. } => "skipped-missing",
            BackfillOutcome::SkippedNoRule => "skipped-no-rule",
            BackfillOutcome::AlreadySet { .. } => "already-set",
        }
    }
}

/// Per-node planning entry — what we'd do to this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillEntry {
    pub node_id: String,
    pub node_type: Option<String>,
    pub outcome: BackfillOutcome,
}

/// Aggregate plan — totals per bucket plus the per-entry detail.
#[derive(Debug, Clone, Default)]
pub struct BackfillPlan {
    pub entries: Vec<BackfillEntry>,
    pub fillable: usize,
    pub skipped_missing: usize,
    pub skipped_no_rule: usize,
    pub already_set: usize,
}

impl BackfillPlan {
    pub fn total(&self) -> usize {
        self.entries.len()
    }
}

/// Convention-based inference: given a node's type + id, return the canonical
/// artifact path *relative to project root*, if any rule applies.
///
/// Returns `None` when the node type has no canonical artifact (task, code,
/// function, class, module, file, etc. — see design table).
pub fn infer_doc_path(node_type: Option<&str>, node_id: &str) -> Option<String> {
    let nt = node_type?;
    match nt {
        "issue" => {
            // id form: `ISS-NNN` (or any opaque issue id).
            // Convention: .gid/issues/<id>/issue.md
            if node_id.is_empty() {
                None
            } else {
                Some(format!(".gid/issues/{}/issue.md", node_id))
            }
        }
        "feature" => {
            // id form: `<slug>` → .gid/features/<slug>/design.md
            // (We prefer design.md over requirements.md; see design §3.4.)
            if node_id.is_empty() {
                None
            } else {
                Some(format!(".gid/features/{}/design.md", node_id))
            }
        }
        "design" => {
            // id forms: `<slug>` or `<slug>/r<N>` — the /rN suffix is informational,
            // canonical doc is always design.md.
            let slug = node_id.split('/').next()?;
            if slug.is_empty() {
                None
            } else {
                Some(format!(".gid/features/{}/design.md", slug))
            }
        }
        "review" => {
            // id form: `<feature-slug>/<review-name>` — both segments required.
            // → .gid/features/<feature-slug>/reviews/<review-name>.md
            let mut parts = node_id.splitn(2, '/');
            let feat = parts.next()?;
            let name = parts.next()?;
            if feat.is_empty() || name.is_empty() {
                None
            } else {
                Some(format!(".gid/features/{}/reviews/{}.md", feat, name))
            }
        }
        // Code/task layer nodes legitimately have no canonical authored artifact.
        "task" | "code" | "function" | "class" | "module" | "file" | "component"
        | "method" | "interface" | "trait" | "enum" | "struct" => None,
        // Unknown node types: be conservative, no inference.
        _ => None,
    }
}

/// Build a back-fill plan over a slice of nodes, given a project root used to
/// resolve relative inferred paths and check file existence.
///
/// This function is pure with respect to the filesystem only via the
/// `file_exists` callback, which lets unit tests substitute an in-memory oracle
/// instead of touching real files. Production callers pass `default_file_exists`.
pub fn plan_backfill<F>(nodes: &[Node], project_root: &Path, file_exists: F) -> BackfillPlan
where
    F: Fn(&Path) -> bool,
{
    let mut plan = BackfillPlan::default();

    for node in nodes {
        // Already-set short-circuit.
        if let Some(existing) = node.doc_path.as_ref() {
            plan.already_set += 1;
            plan.entries.push(BackfillEntry {
                node_id: node.id.clone(),
                node_type: node.node_type.clone(),
                outcome: BackfillOutcome::AlreadySet {
                    existing: existing.clone(),
                },
            });
            continue;
        }

        let inferred = infer_doc_path(node.node_type.as_deref(), &node.id);

        let outcome = match inferred {
            None => {
                plan.skipped_no_rule += 1;
                BackfillOutcome::SkippedNoRule
            }
            Some(rel) => {
                let abs = project_root.join(&rel);
                if file_exists(&abs) {
                    plan.fillable += 1;
                    BackfillOutcome::Fillable {
                        inferred_path: rel,
                    }
                } else {
                    plan.skipped_missing += 1;
                    BackfillOutcome::SkippedMissing {
                        inferred_path: rel,
                    }
                }
            }
        };

        plan.entries.push(BackfillEntry {
            node_id: node.id.clone(),
            node_type: node.node_type.clone(),
            outcome,
        });
    }

    plan
}

/// Default file-existence check — `Path::exists()` honours symlinks/permissions
/// the same way the rest of the codebase does.
pub fn default_file_exists(p: &Path) -> bool {
    p.exists()
}

/// Convenience: turn a plan into the list of (node_id → inferred_path) pairs
/// that an `--apply` run would actually write.
pub fn applicable_updates(plan: &BackfillPlan) -> Vec<(String, String)> {
    plan.entries
        .iter()
        .filter_map(|e| match &e.outcome {
            BackfillOutcome::Fillable { inferred_path } => {
                Some((e.node_id.clone(), inferred_path.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Locate the project root from a starting directory. Walks up looking for
/// `.gid/`. Returns `None` if not found.
pub fn find_project_root_from(start: &Path) -> Option<PathBuf> {
    crate::parser::find_project_root(start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn n(id: &str, ty: Option<&str>, doc: Option<&str>) -> Node {
        let mut node = Node::new(id, id);
        node.node_type = ty.map(String::from);
        node.doc_path = doc.map(String::from);
        node
    }

    // ------------------- infer_doc_path -------------------

    #[test]
    fn infer_issue() {
        assert_eq!(
            infer_doc_path(Some("issue"), "ISS-058").as_deref(),
            Some(".gid/issues/ISS-058/issue.md")
        );
        // Non-numeric ids still work — convention is structural, not pattern-matched.
        assert_eq!(
            infer_doc_path(Some("issue"), "ISS-foo-bar").as_deref(),
            Some(".gid/issues/ISS-foo-bar/issue.md")
        );
    }

    #[test]
    fn infer_feature() {
        assert_eq!(
            infer_doc_path(Some("feature"), "iss-058-doc-path").as_deref(),
            Some(".gid/features/iss-058-doc-path/design.md")
        );
    }

    #[test]
    fn infer_design_plain_and_revisioned() {
        assert_eq!(
            infer_doc_path(Some("design"), "auth").as_deref(),
            Some(".gid/features/auth/design.md")
        );
        assert_eq!(
            infer_doc_path(Some("design"), "auth/r2").as_deref(),
            Some(".gid/features/auth/design.md")
        );
        // Even longer suffixes collapse to the canonical doc.
        assert_eq!(
            infer_doc_path(Some("design"), "auth/r2/notes").as_deref(),
            Some(".gid/features/auth/design.md")
        );
    }

    #[test]
    fn infer_review() {
        assert_eq!(
            infer_doc_path(Some("review"), "auth/design-r1").as_deref(),
            Some(".gid/features/auth/reviews/design-r1.md")
        );
    }

    #[test]
    fn infer_review_missing_name_is_none() {
        // No `/<name>` segment → can't form a path.
        assert_eq!(infer_doc_path(Some("review"), "auth"), None);
        assert_eq!(infer_doc_path(Some("review"), ""), None);
    }

    #[test]
    fn infer_code_layer_yields_none() {
        for ty in ["task", "code", "function", "class", "module", "file",
                   "component", "method", "interface", "trait", "enum", "struct"] {
            assert_eq!(
                infer_doc_path(Some(ty), "anything"),
                None,
                "node_type={ty} should be no-rule"
            );
        }
    }

    #[test]
    fn infer_unknown_type_yields_none() {
        assert_eq!(infer_doc_path(Some("mystery"), "x"), None);
        assert_eq!(infer_doc_path(None, "x"), None);
    }

    #[test]
    fn infer_empty_id_yields_none() {
        assert_eq!(infer_doc_path(Some("issue"), ""), None);
        assert_eq!(infer_doc_path(Some("feature"), ""), None);
        assert_eq!(infer_doc_path(Some("design"), ""), None);
    }

    // ------------------- plan_backfill -------------------

    fn oracle(present: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<String> = present.iter().map(|s| s.to_string()).collect();
        move |p: &Path| {
            let s = p.to_string_lossy().to_string();
            set.iter().any(|present| s.ends_with(present))
        }
    }

    #[test]
    fn plan_classifies_each_node_into_one_bucket() {
        let nodes = vec![
            n("ISS-058", Some("issue"), None),       // fillable
            n("ISS-999", Some("issue"), None),       // skipped-missing
            n("auth", Some("feature"), None),        // fillable
            n("ghost", Some("feature"), None),       // skipped-missing
            n("task-1", Some("task"), None),         // skipped-no-rule
            n("ISS-001", Some("issue"), Some(".gid/issues/ISS-001/issue.md")), // already-set
        ];
        let root = Path::new("/proj");
        let plan = plan_backfill(
            &nodes,
            root,
            oracle(&[
                ".gid/issues/ISS-058/issue.md",
                ".gid/features/auth/design.md",
            ]),
        );

        assert_eq!(plan.total(), 6);
        assert_eq!(plan.fillable, 2);
        assert_eq!(plan.skipped_missing, 2);
        assert_eq!(plan.skipped_no_rule, 1);
        assert_eq!(plan.already_set, 1);
    }

    #[test]
    fn plan_already_set_takes_priority_over_inference() {
        // If doc_path is non-NULL, we never re-infer — even if the existing
        // value points at a missing file.
        let nodes = vec![n(
            "ISS-058",
            Some("issue"),
            Some(".gid/issues/manual-override.md"),
        )];
        let plan = plan_backfill(&nodes, Path::new("/proj"), |_| false);
        assert_eq!(plan.already_set, 1);
        assert_eq!(plan.fillable, 0);
        assert_eq!(plan.skipped_missing, 0);
        assert!(matches!(
            plan.entries[0].outcome,
            BackfillOutcome::AlreadySet { .. }
        ));
    }

    #[test]
    fn applicable_updates_only_returns_fillables() {
        let nodes = vec![
            n("ISS-058", Some("issue"), None),
            n("task-1", Some("task"), None),
            n("ghost", Some("feature"), None),
        ];
        let plan = plan_backfill(
            &nodes,
            Path::new("/proj"),
            oracle(&[".gid/issues/ISS-058/issue.md"]),
        );
        let updates = applicable_updates(&plan);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, "ISS-058");
        assert_eq!(updates[0].1, ".gid/issues/ISS-058/issue.md");
    }

    #[test]
    fn plan_empty_input() {
        let plan = plan_backfill(&[], Path::new("/proj"), |_| true);
        assert_eq!(plan.total(), 0);
        assert_eq!(plan.fillable, 0);
    }

    #[test]
    fn outcome_labels_match_design_doc() {
        // The four labels are part of the §3.4 contract — cement them.
        assert_eq!(
            BackfillOutcome::Fillable {
                inferred_path: "x".into()
            }
            .label(),
            "fillable"
        );
        assert_eq!(
            BackfillOutcome::SkippedMissing {
                inferred_path: "x".into()
            }
            .label(),
            "skipped-missing"
        );
        assert_eq!(BackfillOutcome::SkippedNoRule.label(), "skipped-no-rule");
        assert_eq!(
            BackfillOutcome::AlreadySet {
                existing: "x".into()
            }
            .label(),
            "already-set"
        );
    }
}
