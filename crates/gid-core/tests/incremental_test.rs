//! Integration tests for the incremental extract system.
//!
//! Tests FileDelta helpers, ExtractReport Display, compute_file_delta(),
//! and the full extract_incremental() lifecycle with tempdir.

use gid_core::code_graph::types::Language;
use gid_core::{
    compute_file_delta, CodeGraph, ExtractMetadata, ExtractReport, FileDelta, FileState,
};

// ═══════════════════════════════════════════════════════════════════
// 1. FileDelta helpers
// ═══════════════════════════════════════════════════════════════════

#[test]
fn file_delta_is_empty_when_default() {
    let delta = FileDelta::default();
    assert!(delta.is_empty());
    assert_eq!(delta.changed_count(), 0);
}

#[test]
fn file_delta_is_empty_with_only_unchanged() {
    let delta = FileDelta {
        added: vec![],
        modified: vec![],
        deleted: vec![],
        unchanged: vec!["foo.rs".to_string()],
    };
    assert!(delta.is_empty());
    assert_eq!(delta.changed_count(), 0);
}

#[test]
fn file_delta_not_empty_with_added() {
    let delta = FileDelta {
        added: vec!["new.rs".to_string()],
        modified: vec![],
        deleted: vec![],
        unchanged: vec![],
    };
    assert!(!delta.is_empty());
    assert_eq!(delta.changed_count(), 1);
}

#[test]
fn file_delta_not_empty_with_modified() {
    let delta = FileDelta {
        added: vec![],
        modified: vec!["changed.rs".to_string()],
        deleted: vec![],
        unchanged: vec![],
    };
    assert!(!delta.is_empty());
    assert_eq!(delta.changed_count(), 1);
}

#[test]
fn file_delta_not_empty_with_deleted() {
    let delta = FileDelta {
        added: vec![],
        modified: vec![],
        deleted: vec!["gone.rs".to_string()],
        unchanged: vec![],
    };
    assert!(!delta.is_empty());
    assert_eq!(delta.changed_count(), 1);
}

#[test]
fn file_delta_changed_count_mixed() {
    let delta = FileDelta {
        added: vec!["a.rs".to_string(), "b.rs".to_string()],
        modified: vec!["c.rs".to_string()],
        deleted: vec!["d.rs".to_string(), "e.rs".to_string(), "f.rs".to_string()],
        unchanged: vec!["g.rs".to_string()],
    };
    assert!(!delta.is_empty());
    assert_eq!(delta.changed_count(), 6); // 2 added + 1 modified + 3 deleted
}

// ═══════════════════════════════════════════════════════════════════
// 2. ExtractReport Display
// ═══════════════════════════════════════════════════════════════════

#[test]
fn extract_report_display_full_rebuild() {
    let report = ExtractReport {
        added: 5,
        modified: 0,
        deleted: 0,
        unchanged: 0,
        full_rebuild: true,
        duration_ms: 42,
    };
    let s = format!("{}", report);
    assert_eq!(s, "Full rebuild: 5 files extracted (42ms)");
}

#[test]
fn extract_report_display_full_rebuild_with_unchanged() {
    // full_rebuild + unchanged files (e.g., force rebuild on existing project)
    let report = ExtractReport {
        added: 3,
        modified: 0,
        deleted: 0,
        unchanged: 7,
        full_rebuild: true,
        duration_ms: 100,
    };
    let s = format!("{}", report);
    // Display sums added + modified + unchanged for total
    assert_eq!(s, "Full rebuild: 10 files extracted (100ms)");
}

#[test]
fn extract_report_display_no_changes() {
    let report = ExtractReport {
        added: 0,
        modified: 0,
        deleted: 0,
        unchanged: 12,
        full_rebuild: false,
        duration_ms: 5,
    };
    let s = format!("{}", report);
    assert_eq!(s, "Graph is up to date (12 files, 5ms)");
}

#[test]
fn extract_report_display_mixed_changes() {
    let report = ExtractReport {
        added: 2,
        modified: 3,
        deleted: 1,
        unchanged: 10,
        full_rebuild: false,
        duration_ms: 33,
    };
    let s = format!("{}", report);
    // total_changed = 6, parts order: modified, added, deleted
    assert_eq!(
        s,
        "Updated 6 files (3 modified, 2 added, 1 deleted), 10 unchanged (33ms)"
    );
}

#[test]
fn extract_report_display_only_added() {
    let report = ExtractReport {
        added: 1,
        modified: 0,
        deleted: 0,
        unchanged: 5,
        full_rebuild: false,
        duration_ms: 10,
    };
    let s = format!("{}", report);
    assert_eq!(s, "Updated 1 files (1 added), 5 unchanged (10ms)");
}

#[test]
fn extract_report_display_only_deleted() {
    let report = ExtractReport {
        added: 0,
        modified: 0,
        deleted: 2,
        unchanged: 8,
        full_rebuild: false,
        duration_ms: 7,
    };
    let s = format!("{}", report);
    assert_eq!(s, "Updated 2 files (2 deleted), 8 unchanged (7ms)");
}

// ═══════════════════════════════════════════════════════════════════
// 3. compute_file_delta() — hash-only variant
// ═══════════════════════════════════════════════════════════════════

fn make_metadata(files: Vec<(&str, u64)>) -> ExtractMetadata {
    let mut meta = ExtractMetadata::default();
    for (path, hash) in files {
        meta.files.insert(
            path.to_string(),
            FileState {
                mtime: 0,
                content_hash: hash,
                node_ids: vec![format!("file:{}", path)],
                edge_count: 0,
            },
        );
    }
    meta
}

fn content_hash(content: &str) -> u64 {
    xxhash_rust::xxh64::xxh64(content.as_bytes(), 0)
}

#[test]
fn compute_delta_files_in_current_not_metadata_are_added() {
    let current = vec![
        ("src/new.rs".to_string(), "fn new() {}".to_string(), Language::Rust),
    ];
    let metadata = ExtractMetadata::default();

    let delta = compute_file_delta(&current, &metadata);

    assert_eq!(delta.added, vec!["src/new.rs"]);
    assert!(delta.modified.is_empty());
    assert!(delta.deleted.is_empty());
    assert!(delta.unchanged.is_empty());
}

#[test]
fn compute_delta_files_same_hash_are_unchanged() {
    let content = "fn hello() {}";
    let hash = content_hash(content);

    let current = vec![
        ("src/lib.rs".to_string(), content.to_string(), Language::Rust),
    ];
    let metadata = make_metadata(vec![("src/lib.rs", hash)]);

    let delta = compute_file_delta(&current, &metadata);

    assert!(delta.added.is_empty());
    assert!(delta.modified.is_empty());
    assert!(delta.deleted.is_empty());
    assert_eq!(delta.unchanged, vec!["src/lib.rs"]);
}

#[test]
fn compute_delta_files_different_hash_are_modified() {
    let old_content = "fn hello() {}";
    let new_content = "fn hello() { println!(\"hi\"); }";
    let old_hash = content_hash(old_content);

    let current = vec![
        ("src/lib.rs".to_string(), new_content.to_string(), Language::Rust),
    ];
    let metadata = make_metadata(vec![("src/lib.rs", old_hash)]);

    let delta = compute_file_delta(&current, &metadata);

    assert!(delta.added.is_empty());
    assert_eq!(delta.modified, vec!["src/lib.rs"]);
    assert!(delta.deleted.is_empty());
    assert!(delta.unchanged.is_empty());
}

#[test]
fn compute_delta_files_in_metadata_not_current_are_deleted() {
    let current: Vec<(String, String, Language)> = vec![];
    let metadata = make_metadata(vec![("src/gone.rs", 12345)]);

    let delta = compute_file_delta(&current, &metadata);

    assert!(delta.added.is_empty());
    assert!(delta.modified.is_empty());
    assert_eq!(delta.deleted, vec!["src/gone.rs"]);
    assert!(delta.unchanged.is_empty());
}

#[test]
fn compute_delta_empty_inputs_gives_empty_delta() {
    let current: Vec<(String, String, Language)> = vec![];
    let metadata = ExtractMetadata::default();

    let delta = compute_file_delta(&current, &metadata);

    assert!(delta.is_empty());
    assert!(delta.added.is_empty());
    assert!(delta.modified.is_empty());
    assert!(delta.deleted.is_empty());
    assert!(delta.unchanged.is_empty());
}

#[test]
fn compute_delta_mixed_scenario() {
    let unchanged_content = "fn unchanged() {}";
    let modified_old = "fn old() {}";
    let modified_new = "fn modified() {}";

    let current = vec![
        (
            "src/unchanged.rs".to_string(),
            unchanged_content.to_string(),
            Language::Rust,
        ),
        (
            "src/modified.rs".to_string(),
            modified_new.to_string(),
            Language::Rust,
        ),
        (
            "src/added.rs".to_string(),
            "fn added() {}".to_string(),
            Language::Rust,
        ),
    ];
    let metadata = make_metadata(vec![
        ("src/unchanged.rs", content_hash(unchanged_content)),
        ("src/modified.rs", content_hash(modified_old)),
        ("src/deleted.rs", 99999),
    ]);

    let delta = compute_file_delta(&current, &metadata);

    assert_eq!(delta.added, vec!["src/added.rs"]);
    assert_eq!(delta.modified, vec!["src/modified.rs"]);
    assert_eq!(delta.unchanged, vec!["src/unchanged.rs"]);
    assert_eq!(delta.deleted, vec!["src/deleted.rs"]);
    assert_eq!(delta.changed_count(), 3);
    assert!(!delta.is_empty());
}

// ═══════════════════════════════════════════════════════════════════
// 4. extract_incremental() integration tests (tempdir)
// ═══════════════════════════════════════════════════════════════════

/// Helper: create a Rust source file in the given directory.
fn write_rs_file(dir: &std::path::Path, rel_path: &str, content: &str) {
    let full = dir.join(rel_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
}

/// Helper: save a CodeGraph as code-graph.json so incremental can load it.
fn save_code_graph(gid_dir: &std::path::Path, graph: &CodeGraph) {
    let json_path = gid_dir.join("code-graph.json");
    let json = serde_json::to_string(graph).unwrap();
    std::fs::write(json_path, json).unwrap();
}

#[test]
fn incremental_first_run_no_metadata_full_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    let gid_dir = tmp.path().join("gid");
    let meta_path = gid_dir.join("extract-meta.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&gid_dir).unwrap();

    write_rs_file(&dir, "src/main.rs", "fn main() { println!(\"hello\"); }");
    write_rs_file(&dir, "src/lib.rs", "pub fn hello() { println!(\"hi\"); }");

    let (graph, report) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("extract should succeed");

    // Should be a full rebuild (no prior metadata)
    assert!(report.full_rebuild);
    assert!(report.added > 0);
    assert_eq!(report.modified, 0);
    assert_eq!(report.deleted, 0);
    assert_eq!(report.unchanged, 0);

    // Graph should have nodes
    assert!(!graph.nodes.is_empty());

    // Metadata file should have been created
    assert!(meta_path.exists(), "metadata file should be created");

    // Verify metadata is valid JSON
    let meta_content = std::fs::read_to_string(&meta_path).unwrap();
    let meta: ExtractMetadata = serde_json::from_str(&meta_content).unwrap();
    assert!(!meta.files.is_empty());
    assert_eq!(meta.version, 2); // Current EXTRACT_META_VERSION
}

#[test]
fn incremental_second_run_no_changes_all_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    let gid_dir = tmp.path().join("gid");
    let meta_path = gid_dir.join("extract-meta.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&gid_dir).unwrap();

    write_rs_file(&dir, "src/lib.rs", "pub fn hello() { println!(\"hi\"); }");

    // First run — full rebuild
    let (graph, report1) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("first extract should succeed");
    assert!(report1.full_rebuild);

    // Save the graph so incremental can load it
    save_code_graph(&gid_dir, &graph);

    // Second run — no changes
    let (_graph2, report2) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("second extract should succeed");

    assert!(!report2.full_rebuild);
    assert_eq!(report2.added, 0);
    assert_eq!(report2.modified, 0);
    assert_eq!(report2.deleted, 0);
    assert!(report2.unchanged > 0);
}

#[test]
fn incremental_add_new_file_between_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    let gid_dir = tmp.path().join("gid");
    let meta_path = gid_dir.join("extract-meta.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&gid_dir).unwrap();

    write_rs_file(&dir, "src/lib.rs", "pub fn hello() { println!(\"hi\"); }");

    // First run
    let (graph, report1) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("first extract should succeed");
    assert!(report1.full_rebuild);

    save_code_graph(&gid_dir, &graph);

    // Add a new file
    write_rs_file(
        &dir,
        "src/utils.rs",
        "pub fn helper() { println!(\"helping\"); }",
    );

    // Second run
    let (graph2, report2) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("second extract should succeed");

    assert!(!report2.full_rebuild);
    assert_eq!(report2.added, 1, "should detect 1 added file");
    assert_eq!(report2.modified, 0);
    assert_eq!(report2.deleted, 0);
    assert!(report2.unchanged >= 1);

    // Graph should contain a file node for the new file
    let has_utils = graph2
        .nodes
        .iter()
        .any(|n| n.file_path == "src/utils.rs");
    assert!(has_utils, "graph should contain new file node");
}

#[test]
fn incremental_modify_file_between_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    let gid_dir = tmp.path().join("gid");
    let meta_path = gid_dir.join("extract-meta.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&gid_dir).unwrap();

    write_rs_file(&dir, "src/lib.rs", "pub fn hello() { println!(\"hi\"); }");

    // First run
    let (graph, _) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("first extract should succeed");

    save_code_graph(&gid_dir, &graph);

    // Sleep briefly to ensure mtime changes (incremental uses mtime-first check)
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Modify the file (change content so hash differs)
    write_rs_file(
        &dir,
        "src/lib.rs",
        "pub fn hello() { println!(\"modified!\"); }\npub fn goodbye() {}",
    );

    // Second run
    let (_graph2, report2) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("second extract should succeed");

    assert!(!report2.full_rebuild);
    assert_eq!(report2.modified, 1, "should detect 1 modified file");
    assert_eq!(report2.added, 0);
    assert_eq!(report2.deleted, 0);
}

#[test]
fn incremental_delete_file_between_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    let gid_dir = tmp.path().join("gid");
    let meta_path = gid_dir.join("extract-meta.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&gid_dir).unwrap();

    write_rs_file(&dir, "src/lib.rs", "pub fn hello() { println!(\"hi\"); }");
    write_rs_file(
        &dir,
        "src/removeme.rs",
        "pub fn removeme() { println!(\"bye\"); }",
    );

    // First run
    let (graph, report1) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("first extract should succeed");
    assert!(report1.full_rebuild);
    assert!(report1.added >= 2);

    save_code_graph(&gid_dir, &graph);

    // Verify the file node exists before deletion
    let had_removeme = graph
        .nodes
        .iter()
        .any(|n| n.file_path == "src/removeme.rs");
    assert!(had_removeme, "graph should contain removeme.rs before deletion");

    // Delete the file
    std::fs::remove_file(dir.join("src/removeme.rs")).unwrap();

    // Second run
    let (graph2, report2) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("second extract should succeed");

    assert!(!report2.full_rebuild);
    assert_eq!(report2.deleted, 1, "should detect 1 deleted file");
    assert_eq!(report2.added, 0);

    // Nodes from the deleted file should be removed
    let still_has_removeme = graph2
        .nodes
        .iter()
        .any(|n| n.file_path == "src/removeme.rs");
    assert!(
        !still_has_removeme,
        "graph should not contain deleted file's nodes"
    );
}

#[test]
fn incremental_force_always_full_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    let gid_dir = tmp.path().join("gid");
    let meta_path = gid_dir.join("extract-meta.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&gid_dir).unwrap();

    write_rs_file(&dir, "src/lib.rs", "pub fn hello() { println!(\"hi\"); }");

    // First run — non-force
    let (graph, report1) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false)
        .expect("first extract should succeed");
    assert!(report1.full_rebuild);

    save_code_graph(&gid_dir, &graph);

    // Second run — force, even though nothing changed
    let (_graph2, report2) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, true)
        .expect("force extract should succeed");

    assert!(
        report2.full_rebuild,
        "force=true should always trigger full rebuild"
    );
}

#[test]
fn incremental_force_full_rebuild_even_with_existing_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    let gid_dir = tmp.path().join("gid");
    let meta_path = gid_dir.join("extract-meta.json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&gid_dir).unwrap();

    write_rs_file(&dir, "src/lib.rs", "pub fn hello() {}");
    write_rs_file(&dir, "src/utils.rs", "pub fn util() {}");

    // First run to create metadata
    let (graph, _) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, false).unwrap();
    save_code_graph(&gid_dir, &graph);

    // Verify metadata exists
    assert!(meta_path.exists());

    // Force rebuild with no changes
    let (_, report) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, true).unwrap();
    assert!(report.full_rebuild);
    // All files should show as "added" in full rebuild
    assert!(report.added >= 2);
    assert_eq!(report.modified, 0);
    assert_eq!(report.deleted, 0);
    assert_eq!(report.unchanged, 0);
}
