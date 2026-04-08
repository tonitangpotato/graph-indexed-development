//! Integration tests for the "frictionless" features in gid-core.
//!
//! Covers: slugify, add_feature, add_edge_dedup, merge_feature_nodes,
//! resolve_node, infer_node_type, and watch::sync_on_change.

use gid_core::graph::{Graph, Node, Edge, NodeStatus, TaskSpec};
use gid_core::slugify::slugify;
use gid_core::graph::infer_node_type;

// ═══════════════════════════════════════════════════════════════════
// 1. slugify() tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn slugify_simple_text() {
    assert_eq!(slugify("Hello World"), "hello-world");
}

#[test]
fn slugify_preserves_numbers() {
    assert_eq!(slugify("Task 42 complete"), "task-42-complete");
}

#[test]
fn slugify_special_characters() {
    assert_eq!(slugify("feat: add login!"), "feat-add-login");
}

#[test]
fn slugify_colons_and_slashes() {
    assert_eq!(slugify("file:src/main.rs"), "file-src-main-rs");
}

#[test]
fn slugify_empty_string() {
    assert_eq!(slugify(""), "unnamed");
}

#[test]
fn slugify_only_whitespace() {
    assert_eq!(slugify("   \t\n  "), "unnamed");
}

#[test]
fn slugify_only_special_chars() {
    assert_eq!(slugify("@#$%^&*"), "unnamed");
}

#[test]
fn slugify_unicode_stripped() {
    // Non-ASCII chars are stripped; what remains gets slugified
    assert_eq!(slugify("日本語"), "unnamed");
}

#[test]
fn slugify_mixed_unicode_and_ascii() {
    // The é in café is non-ASCII, stripped; the ASCII portion remains
    let result = slugify("café résumé");
    assert_eq!(result, "caf-rsum");
}

#[test]
fn slugify_consecutive_dashes_collapsed() {
    assert_eq!(slugify("hello---world"), "hello-world");
}

#[test]
fn slugify_leading_trailing_dashes_stripped() {
    assert_eq!(slugify("---leading-trailing---"), "leading-trailing");
}

#[test]
fn slugify_single_character() {
    assert_eq!(slugify("A"), "a");
}

#[test]
fn slugify_already_valid() {
    assert_eq!(slugify("already-valid-slug"), "already-valid-slug");
}

// ═══════════════════════════════════════════════════════════════════
// 2. add_feature() tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn add_feature_creates_feature_node() {
    let mut g = Graph::new();
    let feat_id = g.add_feature("User Authentication", &[]);
    assert_eq!(feat_id, "feat-user-authentication");

    let node = g.get_node(&feat_id).unwrap();
    assert_eq!(node.title, "User Authentication");
    assert_eq!(node.node_type.as_deref(), Some("feature"));
    assert_eq!(node.status, NodeStatus::Todo);
}

#[test]
fn add_feature_creates_task_nodes_and_edges() {
    let mut g = Graph::new();
    let tasks = vec![
        TaskSpec { title: "Design schema".into(), status: None, tags: vec![], deps: vec![] },
        TaskSpec { title: "Write migration".into(), status: None, tags: vec![], deps: vec!["Design schema".into()] },
    ];
    let feat_id = g.add_feature("Database Setup", &tasks);

    // Feature + 2 tasks = 3 nodes
    assert_eq!(g.nodes.len(), 3);

    // Each task should have an "implements" edge to the feature
    let implements_edges: Vec<_> = g.edges.iter()
        .filter(|e| e.to == feat_id && e.relation == "implements")
        .collect();
    assert_eq!(implements_edges.len(), 2);

    // There should be a depends_on edge from "Write migration" to "Design schema"
    let dep_edges: Vec<_> = g.edges.iter()
        .filter(|e| e.relation == "depends_on")
        .collect();
    assert_eq!(dep_edges.len(), 1);
}

#[test]
fn add_feature_with_custom_status_and_tags() {
    let mut g = Graph::new();
    let tasks = vec![
        TaskSpec {
            title: "Setup CI".into(),
            status: Some(NodeStatus::InProgress),
            tags: vec!["devops".into(), "ci".into()],
            deps: vec![],
        },
    ];
    let feat_id = g.add_feature("CI Pipeline", &tasks);

    let task_node = g.nodes.iter()
        .find(|n| n.title == "Setup CI")
        .unwrap();
    assert_eq!(task_node.status, NodeStatus::InProgress);
    assert_eq!(task_node.tags, vec!["devops", "ci"]);
    assert_eq!(task_node.node_type.as_deref(), Some("task"));

    // Feature node should exist
    assert!(g.get_node(&feat_id).is_some());
}

#[test]
fn add_feature_empty_tasks() {
    let mut g = Graph::new();
    let feat_id = g.add_feature("Empty Feature", &[]);

    assert_eq!(g.nodes.len(), 1);
    assert_eq!(g.edges.len(), 0);
    assert!(g.get_node(&feat_id).is_some());
}

#[test]
fn add_feature_duplicate_names_get_unique_ids() {
    let mut g = Graph::new();
    let id1 = g.add_feature("Auth", &[]);
    let id2 = g.add_feature("Auth", &[]);

    assert_ne!(id1, id2);
    assert_eq!(id1, "feat-auth");
    assert_eq!(id2, "feat-auth-2");
    assert_eq!(g.nodes.len(), 2);
}

#[test]
fn add_feature_task_ids_include_feature_slug() {
    let mut g = Graph::new();
    let tasks = vec![
        TaskSpec { title: "Implement login".into(), status: None, tags: vec![], deps: vec![] },
    ];
    g.add_feature("Auth", &tasks);

    let task_node = g.nodes.iter()
        .find(|n| n.title == "Implement login")
        .unwrap();
    assert!(task_node.id.starts_with("task-auth-"), "Task ID should start with 'task-auth-', got: {}", task_node.id);
}

// ═══════════════════════════════════════════════════════════════════
// 3. add_edge_dedup() tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn add_edge_dedup_new_edge_returns_true() {
    let mut g = Graph::new();
    let result = g.add_edge_dedup(Edge::new("a", "b", "depends_on"));
    assert!(result);
    assert_eq!(g.edges.len(), 1);
}

#[test]
fn add_edge_dedup_duplicate_returns_false() {
    let mut g = Graph::new();
    g.add_edge_dedup(Edge::new("a", "b", "depends_on"));
    let result = g.add_edge_dedup(Edge::new("a", "b", "depends_on"));
    assert!(!result);
    assert_eq!(g.edges.len(), 1);
}

#[test]
fn add_edge_dedup_different_relation_is_not_duplicate() {
    let mut g = Graph::new();
    g.add_edge_dedup(Edge::new("a", "b", "depends_on"));
    let result = g.add_edge_dedup(Edge::new("a", "b", "implements"));
    assert!(result);
    assert_eq!(g.edges.len(), 2);
}

#[test]
fn add_edge_dedup_reversed_direction_is_not_duplicate() {
    let mut g = Graph::new();
    g.add_edge_dedup(Edge::new("a", "b", "depends_on"));
    let result = g.add_edge_dedup(Edge::new("b", "a", "depends_on"));
    assert!(result);
    assert_eq!(g.edges.len(), 2);
}

#[test]
fn add_edge_dedup_triple_insert_same_edge() {
    let mut g = Graph::new();
    assert!(g.add_edge_dedup(Edge::new("x", "y", "calls")));
    assert!(!g.add_edge_dedup(Edge::new("x", "y", "calls")));
    assert!(!g.add_edge_dedup(Edge::new("x", "y", "calls")));
    assert_eq!(g.edges.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════
// 4. merge_feature_nodes() tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn merge_feature_nodes_replaces_old_tasks() {
    let mut g = Graph::new();
    let tasks = vec![
        TaskSpec { title: "Old Task 1".into(), status: None, tags: vec![], deps: vec![] },
        TaskSpec { title: "Old Task 2".into(), status: None, tags: vec![], deps: vec![] },
    ];
    let feat_id = g.add_feature("Feature X", &tasks);

    // Build incoming graph with new tasks
    let mut incoming = Graph::new();
    incoming.nodes.push(Node::new("task-feature-x-new-task-1", "New Task 1"));
    incoming.nodes.push(Node::new("task-feature-x-new-task-2", "New Task 2"));
    incoming.nodes.push(Node::new("task-feature-x-new-task-3", "New Task 3"));

    let (removed, added) = g.merge_feature_nodes(&feat_id, incoming);
    assert_eq!(removed, 2);
    assert_eq!(added, 3);

    // Old tasks should be gone
    assert!(g.nodes.iter().all(|n| n.title != "Old Task 1"));
    assert!(g.nodes.iter().all(|n| n.title != "Old Task 2"));

    // New tasks should be present
    assert!(g.get_node("task-feature-x-new-task-1").is_some());
    assert!(g.get_node("task-feature-x-new-task-2").is_some());
    assert!(g.get_node("task-feature-x-new-task-3").is_some());
}

#[test]
fn merge_feature_nodes_preserves_feature_node() {
    let mut g = Graph::new();
    let feat_id = g.add_feature("Keep Me", &[
        TaskSpec { title: "Task".into(), status: None, tags: vec![], deps: vec![] },
    ]);

    let incoming = Graph::new();
    g.merge_feature_nodes(&feat_id, incoming);

    // Feature node should still exist
    assert!(g.get_node(&feat_id).is_some());
}

#[test]
fn merge_feature_nodes_adds_implements_edges() {
    let mut g = Graph::new();
    let feat_id = g.add_feature("Feature Y", &[]);

    let mut incoming = Graph::new();
    incoming.nodes.push(Node::new("task-1", "New Task 1"));
    incoming.nodes.push(Node::new("task-2", "New Task 2"));

    g.merge_feature_nodes(&feat_id, incoming);

    let implements_edges: Vec<_> = g.edges.iter()
        .filter(|e| e.to == feat_id && e.relation == "implements")
        .collect();
    assert_eq!(implements_edges.len(), 2);
}

#[test]
fn merge_feature_nodes_carries_incoming_edges() {
    let mut g = Graph::new();
    let feat_id = g.add_feature("Feature Z", &[]);

    let mut incoming = Graph::new();
    incoming.nodes.push(Node::new("t1", "Task 1"));
    incoming.nodes.push(Node::new("t2", "Task 2"));
    incoming.edges.push(Edge::new("t2", "t1", "depends_on"));

    g.merge_feature_nodes(&feat_id, incoming);

    // The depends_on edge should be present
    let dep = g.edges.iter().find(|e| e.from == "t2" && e.to == "t1" && e.relation == "depends_on");
    assert!(dep.is_some());
}

#[test]
fn merge_feature_nodes_empty_incoming() {
    let mut g = Graph::new();
    let tasks = vec![
        TaskSpec { title: "Existing".into(), status: None, tags: vec![], deps: vec![] },
    ];
    let feat_id = g.add_feature("Feature W", &tasks);

    let initial_node_count = g.nodes.len(); // feature + 1 task = 2

    let incoming = Graph::new();
    let (removed, added) = g.merge_feature_nodes(&feat_id, incoming);

    assert_eq!(removed, 1); // the old task was removed
    assert_eq!(added, 0);   // nothing was added
    // Only feature node remains
    assert_eq!(g.nodes.len(), initial_node_count - 1);
}

// ═══════════════════════════════════════════════════════════════════
// 5. resolve_node() tests
// ═══════════════════════════════════════════════════════════════════

fn graph_with_test_nodes() -> Graph {
    let mut g = Graph::new();
    g.add_node(Node::new("feat-auth", "Authentication Feature"));
    g.add_node(Node::new("task-auth-login", "Implement Login"));
    g.add_node(Node::new("task-auth-logout", "Implement Logout"));

    let mut file_node = Node::new("file-main", "Main Entry");
    file_node.file_path = Some("src/main.rs".to_string());
    g.add_node(file_node);

    g.add_node(Node::new("impl-jwt", "JWT Validation"));
    g
}

#[test]
fn resolve_node_exact_id_match() {
    let g = graph_with_test_nodes();
    let results = g.resolve_node("feat-auth");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "feat-auth");
}

#[test]
fn resolve_node_exact_title_case_insensitive() {
    let g = graph_with_test_nodes();
    let results = g.resolve_node("authentication feature");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "feat-auth");
}

#[test]
fn resolve_node_title_mixed_case() {
    let g = graph_with_test_nodes();
    let results = g.resolve_node("IMPLEMENT LOGIN");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "task-auth-login");
}

#[test]
fn resolve_node_no_match_returns_empty() {
    let g = graph_with_test_nodes();
    let results = g.resolve_node("nonexistent-node");
    assert!(results.is_empty());
}

#[test]
fn resolve_node_file_path_match() {
    let g = graph_with_test_nodes();
    let results = g.resolve_node("src/main.rs");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "file-main");
}

#[test]
fn resolve_node_partial_file_path() {
    let g = graph_with_test_nodes();
    let results = g.resolve_node("main.rs");
    assert!(!results.is_empty());
    assert!(results.iter().any(|n| n.id == "file-main"));
}

#[test]
fn resolve_node_structural_segment_match() {
    let g = graph_with_test_nodes();
    // "auth" is a segment of "feat-auth" (split by '-')
    let results = g.resolve_node("auth");
    assert!(!results.is_empty());
    assert!(results.iter().any(|n| n.id == "feat-auth"));
}

#[test]
fn resolve_node_title_substring() {
    let g = graph_with_test_nodes();
    // "JWT" appears as substring in "JWT Validation" title
    let results = g.resolve_node("jwt");
    assert!(!results.is_empty());
    assert!(results.iter().any(|n| n.id == "impl-jwt"));
}

#[test]
fn resolve_node_empty_string_returns_empty_or_all() {
    let g = graph_with_test_nodes();
    // Empty string: behavior depends on implementation
    // Segments of "" produce no segments, so segment match returns false.
    // Substring match: "".contains("") is true for everything, so it may match all.
    let results = g.resolve_node("");
    // Empty string contains() returns true for all strings,
    // but let's verify it doesn't panic
    // Just verify it doesn't panic — empty string is a valid (if unusual) query
    let _ = results.len();
}

// ═══════════════════════════════════════════════════════════════════
// 6. infer_node_type() tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn infer_node_type_file() {
    assert_eq!(infer_node_type("file:src/main.rs"), Some("file"));
}

#[test]
fn infer_node_type_function_fn() {
    assert_eq!(infer_node_type("fn:my_func"), Some("function"));
}

#[test]
fn infer_node_type_function_func() {
    assert_eq!(infer_node_type("func:handler"), Some("function"));
}

#[test]
fn infer_node_type_struct() {
    assert_eq!(infer_node_type("struct:MyStruct"), Some("class"));
}

#[test]
fn infer_node_type_class() {
    assert_eq!(infer_node_type("class:MyClass"), Some("class"));
}

#[test]
fn infer_node_type_module() {
    assert_eq!(infer_node_type("mod:utils"), Some("module"));
    assert_eq!(infer_node_type("module:helpers"), Some("module"));
}

#[test]
fn infer_node_type_method() {
    assert_eq!(infer_node_type("method:MyStruct::new"), Some("method"));
}

#[test]
fn infer_node_type_trait() {
    assert_eq!(infer_node_type("trait:Display"), Some("trait"));
    assert_eq!(infer_node_type("interface:Renderable"), Some("trait"));
}

#[test]
fn infer_node_type_enum() {
    assert_eq!(infer_node_type("enum:Color"), Some("enum"));
}

#[test]
fn infer_node_type_constant() {
    assert_eq!(infer_node_type("const:MAX_SIZE"), Some("constant"));
    assert_eq!(infer_node_type("static:INSTANCE"), Some("constant"));
}

#[test]
fn infer_node_type_test() {
    assert_eq!(infer_node_type("test:test_login"), Some("test"));
}

#[test]
fn infer_node_type_impl() {
    assert_eq!(infer_node_type("impl:MyStruct"), Some("impl"));
}

#[test]
fn infer_node_type_unknown_prefix() {
    assert_eq!(infer_node_type("unknown:something"), None);
}

#[test]
fn infer_node_type_no_colon() {
    // No colon means prefix == full string, not a recognized prefix
    assert_eq!(infer_node_type("just-a-plain-id"), None);
}

#[test]
fn infer_node_type_empty_string() {
    assert_eq!(infer_node_type(""), None);
}

#[test]
fn infer_node_type_multiple_colons() {
    // Only the first segment before ':' is the prefix
    assert_eq!(infer_node_type("fn:module:func"), Some("function"));
}

// ═══════════════════════════════════════════════════════════════════
// 7. watch module — should_trigger_sync integration test
// ═══════════════════════════════════════════════════════════════════

#[test]
fn watch_should_trigger_sync_source_files() {
    use std::path::Path;
    use gid_core::ignore::IgnoreList;
    use gid_core::watch::should_trigger_sync;

    let watch = Path::new("/project");
    let gid = Path::new("/project/.gid");
    let ignore = IgnoreList::with_defaults();

    // Source files should trigger
    assert!(should_trigger_sync(Path::new("/project/src/lib.rs"), watch, gid, &ignore));
    assert!(should_trigger_sync(Path::new("/project/app.py"), watch, gid, &ignore));
    assert!(should_trigger_sync(Path::new("/project/index.ts"), watch, gid, &ignore));

    // .gid files should NOT trigger
    assert!(!should_trigger_sync(Path::new("/project/.gid/graph.yml"), watch, gid, &ignore));

    // .git files should NOT trigger
    assert!(!should_trigger_sync(Path::new("/project/.git/HEAD"), watch, gid, &ignore));

    // Binary/non-source files should NOT trigger
    assert!(!should_trigger_sync(Path::new("/project/image.png"), watch, gid, &ignore));
    assert!(!should_trigger_sync(Path::new("/project/data.bin"), watch, gid, &ignore));
}

#[test]
fn watch_sync_on_change_no_files_returns_unmodified() {
    use std::fs;
    use gid_core::watch::{sync_on_change, WatchConfig};

    let tmp = tempfile::TempDir::new().unwrap();
    let gid_dir = tmp.path().join(".gid");
    fs::create_dir_all(&gid_dir).unwrap();
    fs::write(gid_dir.join("graph.yml"), "nodes: []\nedges: []\n").unwrap();

    // No source files at all → nothing to extract → graph_modified: false
    let config = WatchConfig::new(tmp.path().to_path_buf(), gid_dir);
    let result = sync_on_change(&config).unwrap();
    assert!(!result.graph_modified);
    assert_eq!(result.files_changed, 0);
}

#[test]
fn watch_sync_on_change_with_source_file() {
    use std::fs;
    use gid_core::watch::{sync_on_change, WatchConfig};

    let tmp = tempfile::TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let gid_dir = tmp.path().join(".gid");
    fs::create_dir_all(&gid_dir).unwrap();

    // Write a Rust source file
    fs::write(src_dir.join("main.rs"), "fn main() { println!(\"hello\"); }\n").unwrap();
    // Write minimal graph
    fs::write(gid_dir.join("graph.yml"), "nodes: []\nedges: []\n").unwrap();

    let config = WatchConfig::new(tmp.path().to_path_buf(), gid_dir.clone());

    // First sync should detect the new file
    let result = sync_on_change(&config).unwrap();
    assert!(result.graph_modified);
    assert!(result.files_changed > 0);
    assert!(result.code_nodes > 0);

    // Second sync (no changes) should be a no-op
    let result2 = sync_on_change(&config).unwrap();
    assert!(!result2.graph_modified);
    assert_eq!(result2.files_changed, 0);
}

// ═══════════════════════════════════════════════════════════════════
// Cross-feature integration tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn add_feature_then_resolve_by_title() {
    let mut g = Graph::new();
    let feat_id = g.add_feature("User Dashboard", &[
        TaskSpec { title: "Build layout".into(), status: None, tags: vec![], deps: vec![] },
    ]);

    // Resolve feature by title
    let results = g.resolve_node("User Dashboard");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, feat_id);

    // Resolve task by title
    let results = g.resolve_node("Build layout");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_type.as_deref(), Some("task"));
}

#[test]
fn add_node_auto_infers_type_from_id() {
    let mut g = Graph::new();
    g.add_node(Node::new("fn:parse_input", "Parse Input Function"));

    let node = g.get_node("fn:parse_input").unwrap();
    assert_eq!(node.node_type.as_deref(), Some("function"));
}

#[test]
fn slugify_used_in_feature_ids() {
    // Verify that the feature ID follows the slugify convention
    let mut g = Graph::new();
    let feat_id = g.add_feature("My Cool Feature!", &[]);
    assert_eq!(feat_id, format!("feat-{}", slugify("My Cool Feature!")));
}
