//! Integration test for the ISS-058 §3.4 back-fill subcommand logic.
//!
//! Exercises the full SQLite round-trip: build a graph with a mix of node
//! types + on-disk artifacts, persist to graph.db (schema_version 2 via
//! apply_migrations), run plan_backfill against the loaded graph, then
//! apply the updates and verify they persist back to disk correctly.

#![cfg(feature = "sqlite")]

use gid_core::backfill_doc_path::{
    applicable_updates, default_file_exists, plan_backfill, BackfillOutcome,
};
use gid_core::storage::{load_graph_auto, save_graph_auto, StorageBackend};
use gid_core::{Graph, Node};
use std::fs;
use tempfile::TempDir;

/// Build a project-shaped fixture under `root`:
///
/// ```text
/// root/
///   .gid/
///     issues/ISS-058/issue.md       ← exists
///     features/auth/design.md       ← exists
///     features/auth/reviews/design-r1.md ← exists
///     (ISS-999 dir omitted on purpose → skipped-missing case)
///     (feature `ghost` omitted     → skipped-missing case)
/// ```
fn build_fixture(root: &std::path::Path) {
    let gid = root.join(".gid");
    fs::create_dir_all(gid.join("issues/ISS-058")).unwrap();
    fs::write(gid.join("issues/ISS-058/issue.md"), "# ISS-058\n").unwrap();

    fs::create_dir_all(gid.join("features/auth/reviews")).unwrap();
    fs::write(gid.join("features/auth/design.md"), "# auth design\n").unwrap();
    fs::write(
        gid.join("features/auth/reviews/design-r1.md"),
        "# r1\n",
    )
    .unwrap();
}

fn make_node(id: &str, ty: &str) -> Node {
    let mut n = Node::new(id, id);
    n.node_type = Some(ty.to_string());
    n
}

#[test]
fn plan_then_apply_round_trips_through_sqlite() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    build_fixture(root);

    let gid_dir = root.join(".gid");

    // Build a graph that exercises every outcome bucket.
    let mut graph = Graph::default();
    graph.nodes.push(make_node("ISS-058", "issue"));   // fillable
    graph.nodes.push(make_node("ISS-999", "issue"));   // skipped-missing (no dir)
    graph.nodes.push(make_node("auth", "feature"));    // fillable
    graph.nodes.push(make_node("ghost", "feature"));   // skipped-missing
    graph.nodes.push(make_node("auth/design-r1", "review")); // fillable
    graph.nodes.push(make_node("task-1", "task"));     // skipped-no-rule
    graph.nodes.push(make_node("fn:lib.rs:foo", "function")); // skipped-no-rule
    {
        // already-set: pre-populated doc_path is left untouched.
        let mut already = make_node("ISS-001", "issue");
        already.doc_path = Some(".gid/issues/ISS-001/issue.md".to_string());
        graph.nodes.push(already);
    }

    // Persist via the canonical save path → schema_version 2 + doc_path column live.
    save_graph_auto(&graph, &gid_dir, Some(StorageBackend::Sqlite))
        .expect("save to sqlite");

    // Reload — confirms the schema migration + put_node/get_node round-trip
    // we shipped in 3615a02 actually preserves doc_path on disk.
    let loaded = load_graph_auto(&gid_dir, Some(StorageBackend::Sqlite))
        .expect("load from sqlite");
    assert_eq!(loaded.nodes.len(), 8);
    let already = loaded
        .nodes
        .iter()
        .find(|n| n.id == "ISS-001")
        .expect("ISS-001 round-tripped");
    assert_eq!(
        already.doc_path.as_deref(),
        Some(".gid/issues/ISS-001/issue.md")
    );

    // Plan against the loaded graph — file-existence checks resolve relative
    // to the project root (parent of .gid/).
    let plan = plan_backfill(&loaded.nodes, root, default_file_exists);

    assert_eq!(plan.fillable, 3, "issue+feature+review on disk");
    assert_eq!(plan.skipped_missing, 2, "ISS-999 and ghost feature");
    assert_eq!(plan.skipped_no_rule, 2, "task + function");
    assert_eq!(plan.already_set, 1, "pre-populated ISS-001");
    assert_eq!(plan.total(), 8);

    // Specific entries: outcome contents must match the canonical paths.
    let by_id = |id: &str| -> &BackfillOutcome {
        &plan
            .entries
            .iter()
            .find(|e| e.node_id == id)
            .unwrap_or_else(|| panic!("plan missing {id}"))
            .outcome
    };
    match by_id("ISS-058") {
        BackfillOutcome::Fillable { inferred_path } => {
            assert_eq!(inferred_path, ".gid/issues/ISS-058/issue.md");
        }
        other => panic!("ISS-058 should be fillable, got {other:?}"),
    }
    match by_id("auth/design-r1") {
        BackfillOutcome::Fillable { inferred_path } => {
            assert_eq!(inferred_path, ".gid/features/auth/reviews/design-r1.md");
        }
        other => panic!("review should be fillable, got {other:?}"),
    }
    match by_id("ISS-999") {
        BackfillOutcome::SkippedMissing { inferred_path } => {
            assert_eq!(inferred_path, ".gid/issues/ISS-999/issue.md");
        }
        other => panic!("ISS-999 should be skipped-missing, got {other:?}"),
    }

    // Apply: take the updates and write them back.
    let updates = applicable_updates(&plan);
    assert_eq!(updates.len(), 3);

    let mut applied = loaded.clone();
    for (id, path) in &updates {
        for node in applied.nodes.iter_mut() {
            if node.id == *id {
                node.doc_path = Some(path.clone());
            }
        }
    }
    save_graph_auto(&applied, &gid_dir, Some(StorageBackend::Sqlite))
        .expect("save updated graph");

    // Reload one more time — verify writes survived a round-trip and didn't
    // disturb the already-set node.
    let final_graph = load_graph_auto(&gid_dir, Some(StorageBackend::Sqlite))
        .expect("reload after apply");

    let lookup = |id: &str| -> Option<String> {
        final_graph
            .nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| n.doc_path.clone())
    };

    assert_eq!(
        lookup("ISS-058"),
        Some(".gid/issues/ISS-058/issue.md".to_string())
    );
    assert_eq!(
        lookup("auth"),
        Some(".gid/features/auth/design.md".to_string())
    );
    assert_eq!(
        lookup("auth/design-r1"),
        Some(".gid/features/auth/reviews/design-r1.md".to_string())
    );
    // Pre-existing already-set node untouched.
    assert_eq!(
        lookup("ISS-001"),
        Some(".gid/issues/ISS-001/issue.md".to_string())
    );
    // Skipped buckets stayed NULL.
    assert_eq!(lookup("ISS-999"), None);
    assert_eq!(lookup("ghost"), None);
    assert_eq!(lookup("task-1"), None);
    assert_eq!(lookup("fn:lib.rs:foo"), None);
}

#[test]
fn dry_run_planning_does_not_mutate_graph() {
    // The CLI's default mode is dry-run; this test cements the contract that
    // plan_backfill is read-only — running it must not touch the input slice
    // or the underlying database.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    build_fixture(root);
    let gid_dir = root.join(".gid");

    let mut graph = Graph::default();
    graph.nodes.push(make_node("ISS-058", "issue"));
    graph.nodes.push(make_node("auth", "feature"));
    save_graph_auto(&graph, &gid_dir, Some(StorageBackend::Sqlite)).unwrap();

    let before = load_graph_auto(&gid_dir, Some(StorageBackend::Sqlite)).unwrap();
    let _plan = plan_backfill(&before.nodes, root, default_file_exists);
    // Reload — bytes on disk must be unchanged because plan_backfill never wrote.
    let after = load_graph_auto(&gid_dir, Some(StorageBackend::Sqlite)).unwrap();

    assert_eq!(before.nodes.len(), after.nodes.len());
    for (b, a) in before.nodes.iter().zip(after.nodes.iter()) {
        assert_eq!(b.id, a.id);
        assert_eq!(b.doc_path, a.doc_path, "doc_path must not change in dry-run");
    }
}
