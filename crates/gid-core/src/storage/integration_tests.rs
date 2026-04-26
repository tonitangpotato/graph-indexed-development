//! Integration tests: YAML migration → SQLite queries → context assembly → history snapshots.
//!
//! These tests exercise the full pipeline end-to-end, verifying that data flows
//! correctly from YAML source through SQLite storage, context scoring, and
//! history management.

#[cfg(test)]
#[cfg(feature = "sqlite")]
mod tests {
    
    use std::fs;

    use tempfile::TempDir;

    use crate::graph::{Edge, Graph, Node, NodeStatus};
    use crate::harness::context::{
        budget_fit_by_category, score_candidates, Candidate, ScoredCandidate, TargetContext,
    };
    use crate::history::HistoryManager;
    use crate::storage::migration::{
        migrate, MigrationConfig, MigrationStatus, ValidationLevel,
    };
    use crate::storage::sqlite::SqliteStorage;
    use crate::storage::trait_def::{BatchOp, GraphStorage, NodeFilter};

    // ═════════════════════════════════════════════════════════
    // Helper: build a sample YAML string with nodes + edges
    // ═════════════════════════════════════════════════════════

    const SAMPLE_YAML: &str = r#"
project:
  name: integration-test
  description: End-to-end pipeline test

nodes:
  - id: feature-auth
    title: Authentication Feature
    type: feature
    status: in_progress
    description: User authentication and authorization
    tags: [security, backend]
    metadata:
      design_doc: auth
      priority_level: "high"

  - id: task-login
    title: Implement Login Endpoint
    type: task
    status: todo
    description: POST /api/login with JWT token generation
    tags: [api, security]
    priority: 10
    owner: alice
    file_path: src/auth/login.rs

  - id: task-signup
    title: Implement Signup Endpoint
    type: task
    status: todo
    description: POST /api/signup with email verification
    tags: [api, security]
    priority: 8
    owner: bob
    file_path: src/auth/signup.rs

  - id: task-middleware
    title: Auth Middleware
    type: task
    status: done
    description: JWT validation middleware for protected routes
    tags: [middleware, security]
    priority: 9
    owner: alice
    file_path: src/auth/middleware.rs

  - id: comp-db
    title: Database Layer
    type: component
    status: in_progress
    description: PostgreSQL connection pool and query builder
    tags: [database, infrastructure]

  - id: file-schema
    title: Database Schema
    type: file
    status: done
    description: SQL migration files for user tables
    file_path: migrations/001_users.sql
    tags: [database, schema]

  - id: test-login
    title: Login Integration Tests
    type: test
    status: todo
    description: Test login endpoint with valid and invalid credentials
    tags: [testing, auth]

  - id: task-logout
    title: Implement Logout Endpoint
    type: task
    status: blocked
    description: POST /api/logout - invalidate JWT tokens
    tags: [api, security]
    owner: alice

edges:
  - from: task-login
    to: feature-auth
    relation: implements
  - from: task-signup
    to: feature-auth
    relation: implements
  - from: task-middleware
    to: feature-auth
    relation: implements
  - from: task-login
    to: comp-db
    relation: depends_on
  - from: task-signup
    to: comp-db
    relation: depends_on
  - from: comp-db
    to: file-schema
    relation: contains
  - from: test-login
    to: task-login
    relation: tests_for
  - from: task-logout
    to: task-middleware
    relation: depends_on
  - from: task-login
    to: task-middleware
    relation: depends_on
"#;

    /// Write sample YAML to a temp dir and return (tmp_dir, yaml_path, db_path).
    fn setup_migration_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = TempDir::new().expect("create temp dir");
        let yaml_path = tmp.path().join("graph.yml");
        let db_path = tmp.path().join("graph.db");
        fs::write(&yaml_path, SAMPLE_YAML).expect("write sample YAML");
        (tmp, yaml_path, db_path)
    }

    /// Run migration and return the SqliteStorage handle.
    fn migrate_and_open(
        yaml_path: &std::path::Path,
        db_path: &std::path::Path,
    ) -> SqliteStorage {
        let config = MigrationConfig {
            source_path: yaml_path.to_path_buf(),
            target_path: db_path.to_path_buf(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };
        let report = migrate(&config).expect("migration should succeed");
        assert!(
            report.status == MigrationStatus::Success
                || report.status == MigrationStatus::SuccessWithWarnings,
            "migration failed: {:?}",
            report.status
        );
        SqliteStorage::open(db_path).expect("open DB after migration")
    }

    // ═════════════════════════════════════════════════════════
    // Test 1: Full migration pipeline — YAML → SQLite roundtrip
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_migration_yaml_to_sqlite_roundtrip() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let config = MigrationConfig {
            source_path: yaml_path,
            target_path: db_path.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).expect("migration should succeed");

        // Verify counts match sample YAML
        assert_eq!(report.nodes_migrated, 8, "expected 8 nodes migrated");
        assert_eq!(report.edges_migrated, 9, "expected 9 edges migrated");
        assert!(
            report.status == MigrationStatus::Success
                || report.status == MigrationStatus::SuccessWithWarnings
        );

        // Open the DB and verify node count via GraphStorage trait
        let storage = SqliteStorage::open(&db_path).expect("open migrated DB");
        assert_eq!(storage.get_node_count().unwrap(), 8);
        assert_eq!(storage.get_edge_count().unwrap(), 9);

        // Verify project metadata was stored
        let project = storage.get_project_meta().unwrap();
        assert!(project.is_some());
        let project = project.unwrap();
        assert_eq!(project.name, "integration-test");
    }

    // ═════════════════════════════════════════════════════════
    // Test 2: CRUD operations on migrated SQLite data
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_crud_operations_on_migrated_data() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        // READ: get an existing node
        let node = storage.get_node("task-login").unwrap();
        assert!(node.is_some());
        let node = node.unwrap();
        assert_eq!(node.title, "Implement Login Endpoint");
        assert_eq!(node.status, NodeStatus::Todo);
        assert_eq!(node.owner.as_deref(), Some("alice"));

        // READ: get edges for a node
        let edges = storage.get_edges("task-login").unwrap();
        assert!(edges.len() >= 3, "task-login should have ≥3 edges (implements, depends_on, tests_for back-ref, depends_on middleware)");

        // CREATE: insert a new node
        let new_node = Node::new("task-refresh", "Implement Token Refresh")
            .with_description("POST /api/refresh for JWT token renewal")
            .with_status(NodeStatus::Todo)
            .with_tags(vec!["api".into(), "security".into()]);
        storage.put_node(&new_node).unwrap();
        assert_eq!(storage.get_node_count().unwrap(), 9);

        // Verify the new node is retrievable
        let fetched = storage.get_node("task-refresh").unwrap().unwrap();
        assert_eq!(fetched.title, "Implement Token Refresh");

        // UPDATE: modify an existing node
        let mut updated = node.clone();
        updated.status = NodeStatus::InProgress;
        updated.description = Some("Updated: Login endpoint with rate limiting".into());
        storage.put_node(&updated).unwrap();
        let re_fetched = storage.get_node("task-login").unwrap().unwrap();
        assert_eq!(re_fetched.status, NodeStatus::InProgress);
        assert!(re_fetched.description.unwrap().contains("rate limiting"));

        // DELETE: remove a node
        storage.delete_node("task-refresh").unwrap();
        assert_eq!(storage.get_node_count().unwrap(), 8);
        assert!(storage.get_node("task-refresh").unwrap().is_none());
    }

    // ═════════════════════════════════════════════════════════
    // Test 3: FTS search over migrated data
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_fts_search_on_migrated_data() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        // Search for "JWT" — should find task-login, task-middleware, task-logout
        let jwt_results = storage.search("JWT").unwrap();
        assert!(
            !jwt_results.is_empty(),
            "FTS search for 'JWT' should return results"
        );
        let jwt_ids: Vec<&str> = jwt_results.iter().map(|n| n.id.as_str()).collect();
        assert!(jwt_ids.contains(&"task-login"), "JWT search should find task-login");
        assert!(jwt_ids.contains(&"task-middleware"), "JWT search should find task-middleware");

        // Search for "PostgreSQL" — should find comp-db
        let pg_results = storage.search("PostgreSQL").unwrap();
        assert!(!pg_results.is_empty());
        assert!(pg_results.iter().any(|n| n.id == "comp-db"));

        // Search for a term that shouldn't exist
        let empty_results = storage.search("kubernetes").unwrap();
        assert!(empty_results.is_empty(), "search for 'kubernetes' should return nothing");
    }

    // ═════════════════════════════════════════════════════════
    // Test 4: query_nodes filtering on migrated data
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_query_nodes_filtering() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        // Filter by node_type = "task"
        let tasks = storage
            .query_nodes(&NodeFilter::new().with_node_type("task"))
            .unwrap();
        assert_eq!(tasks.len(), 4, "should have 4 task nodes");
        assert!(tasks.iter().all(|n| n.node_type.as_deref() == Some("task")));

        // Filter by status = "todo"
        let todos = storage
            .query_nodes(&NodeFilter::new().with_status("todo"))
            .unwrap();
        assert!(
            todos.len() >= 3,
            "should have ≥3 todo nodes (login, signup, test-login)"
        );

        // Filter by owner = "alice"
        let alice_nodes = storage
            .query_nodes(&NodeFilter::new().with_owner("alice"))
            .unwrap();
        assert!(alice_nodes.len() >= 2, "alice owns ≥2 nodes");
        assert!(alice_nodes.iter().all(|n| n.owner.as_deref() == Some("alice")));

        // Filter by tag = "security"
        let security = storage
            .query_nodes(&NodeFilter::new().with_tag("security"))
            .unwrap();
        assert!(
            security.len() >= 4,
            "≥4 nodes tagged 'security': {:?}",
            security.iter().map(|n| &n.id).collect::<Vec<_>>()
        );

        // Filter with limit
        let limited = storage
            .query_nodes(&NodeFilter::new().with_node_type("task").with_limit(2))
            .unwrap();
        assert_eq!(limited.len(), 2, "limit=2 should return exactly 2 results");

        // Combined filter: type=task AND status=todo
        let todo_tasks = storage
            .query_nodes(
                &NodeFilter::new()
                    .with_node_type("task")
                    .with_status("todo"),
            )
            .unwrap();
        assert!(
            todo_tasks.len() >= 2,
            "should have ≥2 todo tasks (login, signup)"
        );
    }

    // ═════════════════════════════════════════════════════════
    // Test 5: Tags and metadata via GraphStorage trait
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_tags_and_metadata_roundtrip() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        // Verify tags were migrated
        let tags = storage.get_tags("task-login").unwrap();
        assert!(tags.contains(&"api".to_string()), "task-login should have 'api' tag");
        assert!(
            tags.contains(&"security".to_string()),
            "task-login should have 'security' tag"
        );

        // Update tags
        storage
            .set_tags("task-login", &["api".into(), "security".into(), "v2".into()])
            .unwrap();
        let updated_tags = storage.get_tags("task-login").unwrap();
        assert_eq!(updated_tags.len(), 3);
        assert!(updated_tags.contains(&"v2".to_string()));

        // Verify metadata was migrated for feature-auth
        let meta = storage.get_metadata("feature-auth").unwrap();
        assert!(
            meta.contains_key("design_doc"),
            "feature-auth should have design_doc metadata"
        );
        assert_eq!(meta["design_doc"], serde_json::json!("auth"));

        // Update metadata
        let mut new_meta = meta.clone();
        new_meta.insert("sprint".into(), serde_json::json!(42));
        storage.set_metadata("feature-auth", &new_meta).unwrap();
        let re_meta = storage.get_metadata("feature-auth").unwrap();
        assert_eq!(re_meta["sprint"], serde_json::json!(42));
        assert_eq!(re_meta["design_doc"], serde_json::json!("auth"));
    }

    // ═════════════════════════════════════════════════════════
    // Test 6: Batch operations on migrated data
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_batch_operations() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        let initial_count = storage.get_node_count().unwrap();

        // Execute a batch: add two nodes, add an edge, update tags
        let ops = vec![
            BatchOp::PutNode(
                Node::new("batch-1", "Batch Node 1")
                    .with_status(NodeStatus::Todo)
                    .with_tags(vec!["batch".into()]),
            ),
            BatchOp::PutNode(
                Node::new("batch-2", "Batch Node 2")
                    .with_status(NodeStatus::Todo),
            ),
            BatchOp::AddEdge(Edge::new("batch-1", "batch-2", "depends_on")),
            BatchOp::SetTags("batch-2".into(), vec!["batch".into(), "new".into()]),
        ];

        storage.execute_batch(&ops).unwrap();

        // Verify batch results
        assert_eq!(storage.get_node_count().unwrap(), initial_count + 2);
        let b1 = storage.get_node("batch-1").unwrap().unwrap();
        assert_eq!(b1.title, "Batch Node 1");
        let b2_tags = storage.get_tags("batch-2").unwrap();
        assert!(b2_tags.contains(&"batch".to_string()));
        assert!(b2_tags.contains(&"new".to_string()));

        // Verify the edge was created
        let edges = storage.get_edges("batch-1").unwrap();
        assert!(
            edges
                .iter()
                .any(|e| e.from == "batch-1" && e.to == "batch-2" && e.relation == "depends_on"),
            "batch edge should exist"
        );
    }

    // ═════════════════════════════════════════════════════════
    // Test 7: Context assembly — scoring + budget fitting
    //         with data sourced from SQLite queries
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_context_assembly_from_sqlite_data() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        // Load nodes from SQLite to build candidates for context assembly
        let task_login = storage.get_node("task-login").unwrap().unwrap();
        let comp_db = storage.get_node("comp-db").unwrap().unwrap();
        let task_middleware = storage.get_node("task-middleware").unwrap().unwrap();
        let test_login_node = storage.get_node("test-login").unwrap().unwrap();

        // Build target context from the task node
        let target = TargetContext::new(
            task_login.id.clone(),
            Some(task_login.title.clone()),
            task_login.file_path.clone(),
            task_login.signature.clone(),
            task_login.doc_comment.clone(),
            task_login.description.clone(),
            None, // no source code loaded
        );
        assert!(target.token_estimate > 0);

        // Build candidates from related nodes (using edge info from SQLite)
        let dep_candidate = Candidate {
            node_id: comp_db.id.clone(),
            node_type: comp_db.node_type.clone().unwrap_or_default(),
            file_path: comp_db.file_path.clone(),
            signature: comp_db.signature.clone(),
            doc_comment: comp_db.doc_comment.clone(),
            description: comp_db.description.clone(),
            source_code: None,
            hop_distance: 1,
            modified_at: None,
            connecting_relation: "depends_on".into(),
            token_estimate: 0, // recalculated by scoring
        };

        let caller_candidate = Candidate {
            node_id: task_middleware.id.clone(),
            node_type: task_middleware.node_type.clone().unwrap_or_default(),
            file_path: task_middleware.file_path.clone(),
            signature: task_middleware.signature.clone(),
            doc_comment: task_middleware.doc_comment.clone(),
            description: task_middleware.description.clone(),
            source_code: None,
            hop_distance: 1,
            modified_at: None,
            connecting_relation: "depends_on".into(),
            token_estimate: 0,
        };

        let test_candidate = Candidate {
            node_id: test_login_node.id.clone(),
            node_type: test_login_node.node_type.clone().unwrap_or_default(),
            file_path: test_login_node.file_path.clone(),
            signature: test_login_node.signature.clone(),
            doc_comment: test_login_node.doc_comment.clone(),
            description: test_login_node.description.clone(),
            source_code: None,
            hop_distance: 1,
            modified_at: None,
            connecting_relation: "tests_for".into(),
            token_estimate: 0,
        };

        // Score candidates
        let all_candidates = vec![dep_candidate.clone(), caller_candidate.clone(), test_candidate.clone()];
        let scored = score_candidates(&all_candidates);
        assert_eq!(scored.len(), 3);
        // Scores should be > 0 and sorted descending
        assert!(scored[0].score >= scored[1].score);
        assert!(scored[1].score >= scored[2].score);
        for sc in &scored {
            assert!(sc.score > 0.0, "all candidates should have positive scores");
            assert!(sc.token_estimate > 0, "all candidates should have token estimates");
        }

        // Run budget fitting
        let scored_deps: Vec<ScoredCandidate> = score_candidates(&[dep_candidate]);
        let scored_callers: Vec<ScoredCandidate> = score_candidates(&[caller_candidate]);
        let scored_tests: Vec<ScoredCandidate> = score_candidates(&[test_candidate]);

        let result = budget_fit_by_category(
            &[target],
            scored_deps,
            scored_callers,
            scored_tests,
            10000, // generous budget
        );

        // Verify the context result
        assert_eq!(result.targets.len(), 1);
        assert_eq!(result.targets[0].node_id, "task-login");
        assert!(!result.dependencies.is_empty(), "should have dependencies");
        assert!(!result.callers.is_empty(), "should have callers");
        assert!(!result.tests.is_empty(), "should have tests");
        assert!(result.estimated_tokens > 0);
        assert!(result.total_included() >= 4); // 1 target + at least 1 dep + 1 caller + 1 test
    }

    // ═════════════════════════════════════════════════════════
    // Test 8: Context assembly with tight budget triggers truncation
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_context_budget_truncation() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        // Load nodes from SQLite
        let task_login = storage.get_node("task-login").unwrap().unwrap();
        let comp_db = storage.get_node("comp-db").unwrap().unwrap();
        let task_middleware = storage.get_node("task-middleware").unwrap().unwrap();
        let test_login_node = storage.get_node("test-login").unwrap().unwrap();

        let target = TargetContext::new(
            task_login.id.clone(),
            Some(task_login.title.clone()),
            task_login.file_path.clone(),
            None,
            None,
            task_login.description.clone(),
            // Add a substantial source code body to consume budget
            Some("fn login(req: LoginRequest) -> Result<Token> {\n    // lots of code here\n    let user = db.find_user(&req.email)?;\n    let token = jwt::sign(&user)?;\n    Ok(token)\n}\n".repeat(5)),
        );

        // Create candidates with content
        let make_candidate = |node: &Node, relation: &str, hop: u32| -> Candidate {
            Candidate {
                node_id: node.id.clone(),
                node_type: node.node_type.clone().unwrap_or_default(),
                file_path: node.file_path.clone(),
                signature: node.signature.clone(),
                doc_comment: node.doc_comment.clone(),
                description: node.description.clone(),
                source_code: Some("// substantial code content\n".repeat(20)),
                hop_distance: hop,
                modified_at: None,
                connecting_relation: relation.into(),
                token_estimate: 0,
            }
        };

        let deps = score_candidates(&[make_candidate(&comp_db, "depends_on", 1)]);
        let callers = score_candidates(&[make_candidate(&task_middleware, "calls", 1)]);
        let tests = score_candidates(&[make_candidate(&test_login_node, "tests_for", 1)]);

        // Use a very tight budget — target alone should consume most of it
        let target_tokens = target.token_estimate;
        let tight_budget = target_tokens + 50; // barely enough for target + a tiny bit

        let result = budget_fit_by_category(&[target], deps, callers, tests, tight_budget);

        // Target is always included
        assert_eq!(result.targets.len(), 1);
        // With such a tight budget, some items should be truncated or dropped
        let total_non_target = result.dependencies.len() + result.callers.len() + result.tests.len();
        let trunc_info = &result.truncation_info;
        // Either items were dropped or truncated
        assert!(
            total_non_target < 3 || trunc_info.truncated_count > 0 || trunc_info.dropped_count > 0,
            "tight budget should cause truncation/dropping: included={}, truncated={}, dropped={}",
            total_non_target,
            trunc_info.truncated_count,
            trunc_info.dropped_count
        );
    }

    // ═════════════════════════════════════════════════════════
    // Test 9: History snapshots — save, diff, restore with
    //         data loaded from SQLite
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_history_snapshot_diff_restore() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        // Build a Graph from SQLite data
        let mut graph_v1 = Graph::new();
        graph_v1.project = storage.get_project_meta().unwrap();
        let all_ids = storage.get_all_node_ids().unwrap();
        for id in &all_ids {
            if let Some(node) = storage.get_node(id).unwrap() {
                graph_v1.add_node(node);
            }
        }
        for id in &all_ids {
            for edge in storage.get_edges(id).unwrap() {
                // Avoid duplicates: only add edges where `from` matches current id
                if edge.from == *id {
                    graph_v1.add_edge(edge);
                }
            }
        }

        // Set up history directory
        let gid_dir = _tmp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let history_mgr = HistoryManager::new(&gid_dir);

        // Save snapshot v1
        let snap_v1 = history_mgr
            .save_snapshot(&graph_v1, Some("Initial migration snapshot"))
            .unwrap();

        // Small delay to ensure distinct timestamps for snapshot filenames
        // (filenames are second-resolution: YYYY-MM-DDTHH-MM-SSZ.yml)
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Create a modified graph (v2): mark task-login as done, add a new node
        let mut graph_v2 = graph_v1.clone();
        if let Some(login_node) = graph_v2.nodes.iter_mut().find(|n| n.id == "task-login") {
            login_node.status = NodeStatus::Done;
        }
        graph_v2.add_node(
            Node::new("task-2fa", "Implement 2FA")
                .with_description("Two-factor authentication")
                .with_status(NodeStatus::Todo),
        );
        // Remove an edge
        graph_v2.edges.retain(|e| !(e.from == "task-logout" && e.to == "task-middleware"));

        // Save snapshot v2
        let _snap_v2 = history_mgr
            .save_snapshot(&graph_v2, Some("Added 2FA, completed login"))
            .unwrap();

        // Diff v1 → v2
        let diff = HistoryManager::diff(&graph_v1, &graph_v2);
        assert!(
            diff.added_nodes.contains(&"task-2fa".to_string()),
            "diff should show task-2fa as added"
        );
        assert!(
            diff.modified_nodes.contains(&"task-login".to_string()),
            "diff should show task-login as modified"
        );
        assert_eq!(diff.removed_edges, 1, "one edge was removed");
        assert!(diff.removed_nodes.is_empty(), "no nodes were removed");

        // Verify load_version roundtrip for v1
        let loaded_v1 = history_mgr.load_version(&snap_v1).unwrap();
        // graph_v1 was built from SQLite which may include nodes beyond the 8 in YAML
        // (e.g., if migration creates additional nodes). The key invariant is that
        // the snapshot roundtrip preserves all nodes exactly.
        assert_eq!(
            loaded_v1.nodes.len(),
            graph_v1.nodes.len(),
            "loaded v1 snapshot should have same node count as original v1 (got {} vs {}). \
             IDs in loaded: {:?}, IDs in original: {:?}",
            loaded_v1.nodes.len(),
            graph_v1.nodes.len(),
            loaded_v1.nodes.iter().map(|n| &n.id).collect::<Vec<_>>(),
            graph_v1.nodes.iter().map(|n| &n.id).collect::<Vec<_>>(),
        );
        assert!(
            !loaded_v1.nodes.iter().any(|n| n.id == "task-2fa"),
            "v1 snapshot should not contain task-2fa"
        );

        // Diff via diff_against: compare current v2 against saved v1
        let graph_yml_path = gid_dir.join("graph.yml");
        crate::parser::save_graph(&graph_v2, &graph_yml_path).unwrap();

        let diff_against = history_mgr.diff_against(&snap_v1, &graph_v2).unwrap();
        assert!(
            diff_against.added_nodes.contains(&"task-2fa".to_string()),
            "diff_against should show task-2fa as added"
        );
    }

    // ═════════════════════════════════════════════════════════
    // Test 10: Neighbor (BFS) queries on migrated graph
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_neighbor_queries_on_migrated_data() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);
        use crate::storage::sqlite::Direction;

        // 1-hop outgoing from task-login: feature-auth (implements), comp-db (depends_on), task-middleware (depends_on)
        let outgoing_1 = storage.neighbors("task-login", 1, Direction::Outgoing).unwrap();
        let out_ids: Vec<&str> = outgoing_1.iter().map(|n| n.id.as_str()).collect();
        assert!(
            out_ids.contains(&"task-login"),
            "BFS includes the root node at hop 0"
        );
        assert!(
            out_ids.contains(&"feature-auth"),
            "task-login → feature-auth (implements)"
        );
        assert!(
            out_ids.contains(&"comp-db"),
            "task-login → comp-db (depends_on)"
        );

        // 1-hop incoming to feature-auth: task-login, task-signup, task-middleware
        let incoming_1 = storage.neighbors("feature-auth", 1, Direction::Incoming).unwrap();
        let in_ids: Vec<&str> = incoming_1.iter().map(|n| n.id.as_str()).collect();
        assert!(in_ids.contains(&"task-login"));
        assert!(in_ids.contains(&"task-signup"));
        assert!(in_ids.contains(&"task-middleware"));

        // 2-hop outgoing from task-login: should include file-schema (via comp-db → contains → file-schema)
        let outgoing_2 = storage.neighbors("task-login", 2, Direction::Outgoing).unwrap();
        let out2_ids: Vec<&str> = outgoing_2.iter().map(|n| n.id.as_str()).collect();
        assert!(
            out2_ids.contains(&"file-schema"),
            "2-hop from task-login should reach file-schema via comp-db"
        );
    }

    // ═════════════════════════════════════════════════════════
    // Test 11: Edge add/remove on migrated data
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_edge_add_remove() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        let initial_edge_count = storage.get_edge_count().unwrap();

        // Add a new edge
        let new_edge = Edge::new("task-signup", "task-login", "relates_to");
        storage.add_edge(&new_edge).unwrap();
        assert_eq!(storage.get_edge_count().unwrap(), initial_edge_count + 1);

        // Verify the edge exists
        let edges = storage.get_edges("task-signup").unwrap();
        assert!(
            edges.iter().any(|e| e.from == "task-signup"
                && e.to == "task-login"
                && e.relation == "relates_to"),
            "new edge should exist"
        );

        // Remove the edge
        storage
            .remove_edge("task-signup", "task-login", "relates_to")
            .unwrap();
        assert_eq!(storage.get_edge_count().unwrap(), initial_edge_count);
    }

    // ═════════════════════════════════════════════════════════
    // Test 12: get_all_node_ids consistency with migration
    // ═════════════════════════════════════════════════════════

    #[test]
    fn test_all_node_ids_complete() {
        let (_tmp, yaml_path, db_path) = setup_migration_env();
        let storage = migrate_and_open(&yaml_path, &db_path);

        let ids = storage.get_all_node_ids().unwrap();
        assert_eq!(ids.len(), 8);

        let expected_ids = vec![
            "feature-auth",
            "task-login",
            "task-signup",
            "task-middleware",
            "comp-db",
            "file-schema",
            "test-login",
            "task-logout",
        ];

        for eid in &expected_ids {
            assert!(
                ids.contains(&eid.to_string()),
                "expected node ID '{}' not found in get_all_node_ids result",
                eid
            );
        }
    }
}
