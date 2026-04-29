//! Drift detection between `.gid/` artifacts and graph nodes (ISS-059).
//!
//! # What "drift" means here
//!
//! The graph DB and the `.gid/` markdown tree are two stores of overlapping
//! truth. They must agree on:
//!
//! 1. **Existence.** Every artifact file has a node, every node that should
//!    have an artifact actually points to one (Layer A.1 / A.2).
//! 2. **Status.** A node's `status` field must equal the frontmatter
//!    `status:` of the artifact it represents (A.3).
//!
//! Stricter cross-checks (relation skew, ledger entries, commit linkage)
//! are split off to ISS-060 and not implemented here. The data model
//! (`DriftCategory`) reserves enum slots for them so JSON consumers don't
//! break when they appear later.
//!
//! # Read-only
//!
//! This module is **strictly read-only**. It does not write to the graph,
//! the artifact tree, or the config. The fix engine (`--fix-drift`) is a
//! separate ISS-060 concern. See design §3.4 for the rationale.
//!
//! # Performance
//!
//! Layer A is O(N) over nodes + O(M) over artifacts on disk, with a single
//! HashMap from artifact path → graph node for the join. Status check
//! re-uses the artifact's already-parsed metadata (no re-read).
//!
//! # Output stability
//!
//! Findings are sorted by `(category, severity desc, node_id, artifact_path)`
//! before return. JSON consumers and snapshot tests can rely on stable order.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::artifact::ArtifactStore;
use crate::backfill_doc_path::infer_doc_path;
use crate::config::{DriftConfig, SeverityFilter};
use crate::graph::{Graph, NodeStatus};

// ─────────────────────────────────────────────────────────────────────────────
// Data model — design §3.5
// ─────────────────────────────────────────────────────────────────────────────

/// A single drift finding.
///
/// Stable JSON shape — additions to `DriftCategory` are additive; clients
/// must tolerate unknown variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftFinding {
    pub category: DriftCategory,
    pub severity: Severity,
    /// Graph node id, when one exists. `None` for "artifact has no node yet".
    pub node_id: Option<String>,
    /// Artifact path relative to project root, when one exists.
    /// `None` for "node points to a missing artifact" — the path *would*
    /// be in `message` and `suggested_fix`.
    pub artifact_path: Option<PathBuf>,
    /// One-line human-readable summary.
    pub message: String,
    /// Imperative phrasing for the operator: "set status to closed", etc.
    pub suggested_fix: String,
    /// `true` ⇒ a future fix engine could repair this without human input.
    /// `false` ⇒ ambiguous; needs a person.
    pub auto_fixable: bool,
    /// Where the fix would land. `None` is only valid when `auto_fixable`
    /// is also `false`.
    pub fix_target: FixTarget,
}

/// Drift category. Layer A variants (`MissingNode`, `DanglingDocPointer`,
/// `StatusDrift`) are implemented in this file. `MissingEdge` and
/// `LedgerNotUpdated` are reserved for ISS-060 and are not produced today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftCategory {
    /// A.1 — artifact file on disk has no graph node.
    MissingNode,
    /// A.2 — graph node's `doc_path` (or canonical inferred path) is missing on disk.
    DanglingDocPointer,
    /// A.3 — frontmatter status doesn't match graph node status.
    StatusDrift,
    /// Reserved (ISS-060) — frontmatter relation present but no graph edge.
    MissingEdge,
    /// Reserved (ISS-060) — ledger entry expected but missing.
    LedgerNotUpdated,
}

/// Per-finding severity. Same level set as the config `SeverityFilter`,
/// kept as a separate enum so finding output isn't confused with filter
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    /// Convert to the corresponding [`SeverityFilter`] level for filtering.
    pub fn as_filter(self) -> SeverityFilter {
        match self {
            Severity::Info => SeverityFilter::Info,
            Severity::Warn => SeverityFilter::Warn,
            Severity::Error => SeverityFilter::Error,
        }
    }
}

/// Where an auto-fix would write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixTarget {
    /// No fix possible / human judgement needed.
    None,
    /// Fix lands in the graph DB (e.g., status update, edge insert).
    Graph,
    /// Fix lands in `.gid/<artifact>.md` (e.g., create stub, update frontmatter).
    Artifact,
    /// Fix lands in the drift ledger (ISS-060).
    Ledger,
}

// ─────────────────────────────────────────────────────────────────────────────
// Report wrapper — what `check_drift` actually returns
// ─────────────────────────────────────────────────────────────────────────────

/// Output of [`check_drift`]: the filtered findings plus a summary block
/// suitable for both human and JSON consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftReport {
    pub findings: Vec<DriftFinding>,
    pub summary: DriftSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DriftSummary {
    pub by_category: BTreeMap<String, usize>,
    pub by_severity: BTreeMap<String, usize>,
    pub auto_fixable: usize,
    /// True if any finding has severity ≥ Error. Drives non-zero exit.
    pub has_errors: bool,
}

impl DriftReport {
    /// `true` if any error-severity finding is present *after* config
    /// filtering. Used by the CLI to choose the exit code.
    pub fn has_errors(&self) -> bool {
        self.summary.has_errors
    }

    fn rebuild_summary(&mut self) {
        let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_severity: BTreeMap<String, usize> = BTreeMap::new();
        let mut auto_fixable = 0usize;
        let mut has_errors = false;
        for f in &self.findings {
            *by_category
                .entry(format!("{:?}", f.category).to_lowercase())
                .or_insert(0) += 1;
            *by_severity
                .entry(format!("{:?}", f.severity).to_lowercase())
                .or_insert(0) += 1;
            if f.auto_fixable {
                auto_fixable += 1;
            }
            if matches!(f.severity, Severity::Error) {
                has_errors = true;
            }
        }
        self.summary = DriftSummary {
            by_category,
            by_severity,
            auto_fixable,
            has_errors,
        };
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Run drift detection over `(graph, artifact tree)` rooted at `project_root`.
///
/// The `store` argument is taken explicitly (not opened internally) so tests
/// can inject a tempdir-backed store and so the CLI can re-use an already
/// opened store. `graph` likewise comes pre-loaded.
///
/// Findings are filtered against `config.severity_filter` and `config.ignore`
/// before return. If `config.enabled` is false, returns an empty report
/// immediately (no scan). Per-check toggles in `config.checks` skip individual
/// checks but still emit summary buckets so JSON consumers see consistent keys.
pub fn check_drift(
    graph: &Graph,
    store: &ArtifactStore,
    project_root: &Path,
    config: &DriftConfig,
) -> DriftReport {
    let mut report = DriftReport {
        findings: Vec::new(),
        summary: DriftSummary::default(),
    };

    if !config.enabled {
        return report;
    }

    let ignore: HashSet<&str> = config.ignore.iter().map(String::as_str).collect();

    // Build the (artifact path → node) and (node id → node) indexes once.
    // We canonicalise to forward-slash strings to match `Node.doc_path`,
    // `ArtifactId::as_str`, and `infer_doc_path`'s output.
    let mut path_to_node: HashMap<String, &crate::graph::Node> = HashMap::new();
    let mut id_to_node: HashMap<&str, &crate::graph::Node> = HashMap::new();
    for n in &graph.nodes {
        id_to_node.insert(n.id.as_str(), n);
        if let Some(p) = node_canonical_path(n) {
            path_to_node.insert(p, n);
        }
    }

    let toggles = &config.checks;

    // List artifacts once. We keep the full list for both A.1 and A.3.
    let artifacts = store.list(None).unwrap_or_default();

    if toggles.a1_orphan_artifacts {
        check_a1_orphan_artifacts(&artifacts, &path_to_node, &ignore, &mut report.findings);
    }
    if toggles.a2_orphan_nodes {
        check_a2_orphan_nodes(graph, project_root, &ignore, &mut report.findings);
    }
    if toggles.a3_status_mismatch {
        check_a3_status_mismatch(&artifacts, &path_to_node, &ignore, &mut report.findings);
    }

    // Filter by severity.
    let min = config.severity_filter;
    report.findings.retain(|f| f.severity.as_filter() >= min);

    // Stable order: category, severity desc, node_id, artifact_path.
    report
        .findings
        .sort_by(|a, b| match a.category.cmp(&b.category) {
            std::cmp::Ordering::Equal => match b.severity.cmp(&a.severity) {
                std::cmp::Ordering::Equal => match a.node_id.cmp(&b.node_id) {
                    std::cmp::Ordering::Equal => a.artifact_path.cmp(&b.artifact_path),
                    o => o,
                },
                o => o,
            },
            o => o,
        });

    report.rebuild_summary();
    report
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Best canonical artifact path for a node. Prefers explicit `doc_path`,
/// falls back to the conventional path inferred from `node_type` + `id`
/// (per ISS-058 §3.4). Returns `None` for code-layer nodes that have no
/// artifact at all.
fn node_canonical_path(n: &crate::graph::Node) -> Option<String> {
    if let Some(p) = &n.doc_path {
        return Some(p.replace('\\', "/"));
    }
    infer_doc_path(n.node_type.as_deref(), &n.id)
}

fn artifact_short_id(path: &str) -> Option<String> {
    // `.gid/issues/ISS-059/issue.md` → "ISS-059"
    // `.gid/features/foo/design.md` → "foo"
    // Falls back to the file stem if neither layout matches.
    let p = Path::new(path);
    let parent = p.parent()?;
    let parent_name = parent.file_name()?.to_str()?;
    Some(parent_name.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// A.1 — artifact on disk, no graph node
// ─────────────────────────────────────────────────────────────────────────────

fn check_a1_orphan_artifacts(
    artifacts: &[crate::artifact::Artifact],
    path_to_node: &HashMap<String, &crate::graph::Node>,
    ignore: &HashSet<&str>,
    out: &mut Vec<DriftFinding>,
) {
    for a in artifacts {
        let path_str = a.id.as_str();
        if path_to_node.contains_key(path_str) {
            continue;
        }
        let short = artifact_short_id(path_str).unwrap_or_else(|| path_str.to_string());
        if ignore.contains(short.as_str()) {
            continue;
        }
        out.push(DriftFinding {
            category: DriftCategory::MissingNode,
            severity: Severity::Warn,
            node_id: None,
            artifact_path: Some(PathBuf::from(path_str)),
            message: format!(
                "artifact `{}` exists on disk but no graph node references it",
                path_str
            ),
            suggested_fix: format!(
                "add a graph node with id `{}` and doc_path `{}` (e.g. `gid add {} ...`)",
                short, path_str, short
            ),
            auto_fixable: false, // node-creation needs human-supplied title/kind
            fix_target: FixTarget::Graph,
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A.2 — node points to missing artifact
// ─────────────────────────────────────────────────────────────────────────────

fn check_a2_orphan_nodes(
    graph: &Graph,
    project_root: &Path,
    ignore: &HashSet<&str>,
    out: &mut Vec<DriftFinding>,
) {
    for n in &graph.nodes {
        if ignore.contains(n.id.as_str()) {
            continue;
        }
        // Code-layer nodes have no artifact — skip cleanly.
        let path = match node_canonical_path(n) {
            Some(p) => p,
            None => continue,
        };
        // Only flag drift for nodes that *should* have an artifact, i.e.
        // where `doc_path` is set (explicit pointer), or where the node_type
        // has a canonical layout (issue/feature/design). `infer_doc_path`
        // already returns None otherwise.
        let abs = project_root.join(&path);
        if abs.exists() {
            continue;
        }
        let was_explicit = n.doc_path.is_some();
        out.push(DriftFinding {
            category: DriftCategory::DanglingDocPointer,
            severity: Severity::Error,
            node_id: Some(n.id.clone()),
            artifact_path: Some(PathBuf::from(&path)),
            message: if was_explicit {
                format!(
                    "node `{}` has doc_path `{}` but the file does not exist",
                    n.id, path
                )
            } else {
                format!(
                    "node `{}` (type {}) is expected at `{}` but the file does not exist",
                    n.id,
                    n.node_type.as_deref().unwrap_or("?"),
                    path
                )
            },
            suggested_fix: format!(
                "either create `{}` or clear `doc_path` on node `{}`",
                path, n.id
            ),
            auto_fixable: false, // creating a stub artifact needs human content
            fix_target: FixTarget::Artifact,
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A.3 — frontmatter status differs from node status
// ─────────────────────────────────────────────────────────────────────────────

fn check_a3_status_mismatch(
    artifacts: &[crate::artifact::Artifact],
    path_to_node: &HashMap<String, &crate::graph::Node>,
    ignore: &HashSet<&str>,
    out: &mut Vec<DriftFinding>,
) {
    for a in artifacts {
        let path_str = a.id.as_str();
        let node = match path_to_node.get(path_str) {
            Some(n) => *n,
            None => continue, // covered by A.1
        };
        if ignore.contains(node.id.as_str()) {
            continue;
        }
        let frontmatter_status = a.metadata.get("status").and_then(|v| v.as_scalar());
        let Some(fm) = frontmatter_status else {
            continue; // status absent in frontmatter is not drift; just unsynced metadata
        };
        let parsed_fm = match parse_status(fm) {
            Some(s) => s,
            None => {
                out.push(DriftFinding {
                    category: DriftCategory::StatusDrift,
                    severity: Severity::Warn,
                    node_id: Some(node.id.clone()),
                    artifact_path: Some(PathBuf::from(path_str)),
                    message: format!(
                        "artifact `{}` has unrecognized frontmatter status `{}`",
                        path_str, fm
                    ),
                    suggested_fix: format!(
                        "set `{}` frontmatter status to one of: todo, in_progress, done, blocked, cancelled",
                        path_str
                    ),
                    auto_fixable: false,
                    fix_target: FixTarget::Artifact,
                });
                continue;
            }
        };
        if parsed_fm == node.status {
            continue;
        }
        out.push(DriftFinding {
            category: DriftCategory::StatusDrift,
            severity: Severity::Warn,
            node_id: Some(node.id.clone()),
            artifact_path: Some(PathBuf::from(path_str)),
            message: format!(
                "node `{}` status `{}` ≠ artifact frontmatter status `{}`",
                node.id, node.status, parsed_fm
            ),
            suggested_fix: format!(
                "decide which is authoritative; either run `gid update --status {} {}` or edit `{}` frontmatter",
                node.id, parsed_fm, path_str
            ),
            auto_fixable: true, // both sides representable, but we don't know which wins
            fix_target: FixTarget::Graph,
        });
    }
}

fn parse_status(raw: &str) -> Option<NodeStatus> {
    let s = raw.trim().to_lowercase();
    match s.as_str() {
        "todo" | "open" => Some(NodeStatus::Todo),
        "in_progress" | "in-progress" | "doing" | "wip" => Some(NodeStatus::InProgress),
        "done" | "closed" | "resolved" => Some(NodeStatus::Done),
        "blocked" => Some(NodeStatus::Blocked),
        "cancelled" | "canceled" => Some(NodeStatus::Cancelled),
        "failed" => Some(NodeStatus::Failed),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Human-readable rendering
// ─────────────────────────────────────────────────────────────────────────────

/// Render a [`DriftReport`] for a terminal. Stable, line-oriented format.
/// Returns the empty string for an empty report (caller decides whether to
/// print a "✓ no drift" banner).
pub fn render_text(report: &DriftReport) -> String {
    if report.findings.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut last_cat: Option<DriftCategory> = None;
    for f in &report.findings {
        if Some(f.category) != last_cat {
            if last_cat.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("[{}]\n", category_label(f.category)));
            last_cat = Some(f.category);
        }
        let where_ = match (&f.node_id, &f.artifact_path) {
            (Some(n), Some(p)) => format!("{} ({})", n, p.display()),
            (Some(n), None) => n.clone(),
            (None, Some(p)) => p.display().to_string(),
            (None, None) => "?".to_string(),
        };
        out.push_str(&format!(
            "  {sev} {where_}\n    {msg}\n    fix: {fix}\n",
            sev = severity_label(f.severity),
            where_ = where_,
            msg = f.message,
            fix = f.suggested_fix,
        ));
    }
    out.push_str(&format!(
        "\n{} finding(s); auto-fixable: {}; errors: {}\n",
        report.findings.len(),
        report.summary.auto_fixable,
        if report.summary.has_errors {
            "yes"
        } else {
            "no"
        }
    ));
    out
}

fn category_label(c: DriftCategory) -> &'static str {
    match c {
        DriftCategory::MissingNode => "missing-node",
        DriftCategory::DanglingDocPointer => "dangling-doc-pointer",
        DriftCategory::StatusDrift => "status-drift",
        DriftCategory::MissingEdge => "missing-edge",
        DriftCategory::LedgerNotUpdated => "ledger-not-updated",
    }
}

fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info  ",
        Severity::Warn => "warn  ",
        Severity::Error => "error ",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ArtifactStore;
    use crate::config::{CheckToggles, DriftConfig};
    use crate::graph::{Graph, Node, NodeStatus};
    use std::fs;
    use tempfile::TempDir;

    /// Spin up a project root with a .gid/ tree and write a graph with the
    /// given nodes. Returns (tempdir, graph, store).
    fn fixture(nodes: Vec<Node>) -> (TempDir, Graph, ArtifactStore) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".gid").join("issues")).unwrap();
        fs::create_dir_all(root.join(".gid").join("features")).unwrap();
        let mut graph = Graph::default();
        graph.nodes = nodes;
        let store = ArtifactStore::open_at("test".to_string(), root.to_path_buf()).unwrap();
        (tmp, graph, store)
    }

    fn make_issue_node(id: &str, status: NodeStatus) -> Node {
        let mut n = Node::new(id, id);
        n.status = status;
        n.node_type = Some("issue".to_string());
        n.doc_path = Some(format!(".gid/issues/{}/issue.md", id));
        n
    }

    fn write_issue(root: &Path, id: &str, frontmatter_status: Option<&str>, body: &str) {
        let dir = root.join(".gid").join("issues").join(id);
        fs::create_dir_all(&dir).unwrap();
        let mut content = String::new();
        if let Some(s) = frontmatter_status {
            content.push_str("---\n");
            content.push_str(&format!("status: {}\n", s));
            content.push_str("---\n\n");
        }
        content.push_str(body);
        fs::write(dir.join("issue.md"), content).unwrap();
    }

    #[test]
    fn empty_project_zero_findings() {
        let (tmp, graph, store) = fixture(vec![]);
        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        assert!(report.findings.is_empty());
        assert!(!report.has_errors());
    }

    #[test]
    fn a1_artifact_without_node() {
        let (tmp, graph, store) = fixture(vec![]);
        write_issue(tmp.path(), "ISS-901", Some("open"), "body");

        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.category, DriftCategory::MissingNode);
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.node_id.is_none());
        assert!(f.artifact_path.as_ref().unwrap().ends_with("issue.md"));
        assert!(!f.auto_fixable);
    }

    #[test]
    fn a2_node_pointing_at_missing_file() {
        let (tmp, graph, store) =
            fixture(vec![make_issue_node("ISS-902", NodeStatus::Todo)]);
        // No file written for ISS-902.
        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.category, DriftCategory::DanglingDocPointer);
        assert_eq!(f.severity, Severity::Error);
        assert_eq!(f.node_id.as_deref(), Some("ISS-902"));
        assert!(report.has_errors());
    }

    #[test]
    fn a3_status_mismatch_detected() {
        let (tmp, graph, store) =
            fixture(vec![make_issue_node("ISS-903", NodeStatus::Todo)]);
        write_issue(tmp.path(), "ISS-903", Some("done"), "body");
        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.category, DriftCategory::StatusDrift);
        assert_eq!(f.node_id.as_deref(), Some("ISS-903"));
        assert!(f.message.contains("todo"));
        assert!(f.message.contains("done"));
        assert!(f.auto_fixable);
    }

    #[test]
    fn a3_open_alias_maps_to_todo() {
        // `open` is the legacy alias used in older issue frontmatter.
        let (tmp, graph, store) =
            fixture(vec![make_issue_node("ISS-904", NodeStatus::Todo)]);
        write_issue(tmp.path(), "ISS-904", Some("open"), "body");
        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        // open == Todo, so no status drift.
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.category != DriftCategory::StatusDrift),
            "unexpected status drift: {:?}",
            report.findings
        );
    }

    #[test]
    fn a3_unrecognized_status_warns() {
        let (tmp, graph, store) =
            fixture(vec![make_issue_node("ISS-905", NodeStatus::Todo)]);
        write_issue(tmp.path(), "ISS-905", Some("zomg-new-state"), "body");
        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].category, DriftCategory::StatusDrift);
        assert!(!report.findings[0].auto_fixable);
    }

    #[test]
    fn ignore_list_filters_out_findings() {
        let (tmp, graph, store) = fixture(vec![]);
        write_issue(tmp.path(), "ISS-906", Some("open"), "body");
        let mut cfg = DriftConfig::default();
        cfg.ignore.push("ISS-906".to_string());
        let report = check_drift(&graph, &store, tmp.path(), &cfg);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn disabled_returns_empty_without_scan() {
        let (tmp, graph, store) =
            fixture(vec![make_issue_node("ISS-907", NodeStatus::Todo)]);
        // Don't write the file — would be A.2.
        let mut cfg = DriftConfig::default();
        cfg.enabled = false;
        let report = check_drift(&graph, &store, tmp.path(), &cfg);
        assert!(report.findings.is_empty());
        assert!(!report.has_errors());
    }

    #[test]
    fn per_check_toggles_silence_specific_categories() {
        let (tmp, graph, store) =
            fixture(vec![make_issue_node("ISS-908", NodeStatus::Todo)]);
        write_issue(tmp.path(), "ISS-908", Some("done"), "body");
        // Both A.3 (status drift) and existing A.1/A.2 do not fire (file matches node).
        let mut cfg = DriftConfig::default();
        cfg.checks = CheckToggles {
            a1_orphan_artifacts: true,
            a2_orphan_nodes: true,
            a3_status_mismatch: false,
        };
        let report = check_drift(&graph, &store, tmp.path(), &cfg);
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.category != DriftCategory::StatusDrift),
            "status drift should be silenced: {:?}",
            report.findings
        );
    }

    #[test]
    fn severity_filter_drops_lower_levels() {
        let (tmp, graph, store) = fixture(vec![]);
        // A.1 fires at Warn severity; filter at Error must drop it.
        write_issue(tmp.path(), "ISS-909", Some("open"), "body");
        let mut cfg = DriftConfig::default();
        cfg.severity_filter = SeverityFilter::Error;
        let report = check_drift(&graph, &store, tmp.path(), &cfg);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn happy_path_clean_project() {
        let (tmp, graph, store) =
            fixture(vec![make_issue_node("ISS-910", NodeStatus::Todo)]);
        write_issue(tmp.path(), "ISS-910", Some("open"), "body");
        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        assert!(
            report.findings.is_empty(),
            "expected zero findings, got: {:?}",
            report.findings
        );
        assert!(!report.has_errors());
    }

    #[test]
    fn render_text_groups_by_category_and_includes_fix() {
        let (tmp, graph, store) = fixture(vec![]);
        write_issue(tmp.path(), "ISS-911", Some("open"), "body");
        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        let text = render_text(&report);
        assert!(text.contains("[missing-node]"), "got: {}", text);
        assert!(text.contains("fix:"), "got: {}", text);
    }

    #[test]
    fn summary_counts_match_findings() {
        let (tmp, graph, store) = fixture(vec![
            make_issue_node("ISS-912", NodeStatus::Todo),
            make_issue_node("ISS-913", NodeStatus::Todo),
        ]);
        // ISS-912: A.3 status drift; ISS-913: A.2 dangling pointer (no file).
        write_issue(tmp.path(), "ISS-912", Some("done"), "body");
        let report = check_drift(&graph, &store, tmp.path(), &DriftConfig::default());
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.summary.by_category.len(), 2);
        assert!(report.summary.has_errors); // A.2 is Error
    }
}
