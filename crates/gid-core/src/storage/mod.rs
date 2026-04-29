//! gid-core graph storage backends.
//!
//! ## Storage Invariants (do not violate)
//!
//! ### SQLite Foreign-Key Enforcement (ISS-033)
//!
//! gid relies on SQLite foreign-key enforcement for graph integrity:
//!
//! - `edges.from_node` and `edges.to_node` reference `nodes(id) ON DELETE CASCADE`
//! - `node_tags`, `node_metadata`, `knowledge` likewise cascade on node delete
//! - Inserting an edge that references a non-existent node is rejected at write-time
//!
//! SQLite's `PRAGMA foreign_keys` defaults to **OFF** and is **per-connection**
//! state — it does not persist across connection re-opens, schema, or backup.
//! Every code path that opens a `rusqlite::Connection` to a gid graph file
//! **must** issue `PRAGMA foreign_keys=ON` before any read/write that depends on
//! referential integrity. `SqliteStorage::open` does this and verifies the
//! setting took effect, returning an error if FK enforcement could not be enabled.
//!
//! Bypassing this — e.g., raw `sqlite3` shell access, or another tool opening
//! the file without setting the PRAGMA — risks silent orphan-edge accumulation
//! and dangling-edge inserts. If you add a new path that opens a gid graph
//! file directly, replicate the PRAGMA + verify pattern from
//! `SqliteStorage::open`.

pub mod error;
pub mod trait_def;
pub mod schema;

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "sqlite")]
pub mod migration;

#[cfg(test)]
#[cfg(feature = "sqlite")]
mod integration_tests;

// Re-export key types for convenience.
pub use error::{StorageError, StorageOp, StorageResult};
pub use trait_def::{BatchOp, GraphStorage, NodeFilter};
pub use schema::SCHEMA_SQL;

#[cfg(feature = "sqlite")]
pub use sqlite::{Direction, SqliteStorage};

#[cfg(feature = "sqlite")]
pub use migration::{migrate, MigrationConfig, MigrationReport, MigrationError, MigrationStatus, ValidationLevel};

// =============================================================================
// Backend Auto-Detection
// =============================================================================

/// Detected storage backend for a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    /// YAML file-based storage (graph.yml).
    Yaml,
    /// SQLite database storage (graph.db).
    Sqlite,
}

impl std::fmt::Display for StorageBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageBackend::Yaml => write!(f, "yaml"),
            StorageBackend::Sqlite => write!(f, "sqlite"),
        }
    }
}

/// Auto-detect storage backend for a `.gid/` directory.
///
/// Detection rules:
/// 1. If `graph.db` exists → SQLite
/// 2. If `graph.yml` exists → YAML
/// 3. Neither → default to SQLite (new project)
///
/// The user can override via `--backend yaml|sqlite` CLI flag.
pub fn detect_backend(gid_dir: &std::path::Path) -> StorageBackend {
    let db_path = gid_dir.join("graph.db");
    let yaml_path = gid_dir.join("graph.yml");

    if db_path.exists() {
        StorageBackend::Sqlite
    } else if yaml_path.exists() {
        StorageBackend::Yaml
    } else {
        // New project — default to SQLite
        StorageBackend::Sqlite
    }
}

/// Resolve backend from explicit flag or auto-detection.
///
/// If `explicit` is Some, use it. Otherwise, auto-detect from `gid_dir`.
pub fn resolve_backend(
    explicit: Option<StorageBackend>,
    gid_dir: &std::path::Path,
) -> StorageBackend {
    explicit.unwrap_or_else(|| detect_backend(gid_dir))
}

impl std::str::FromStr for StorageBackend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "yaml" | "yml" => Ok(StorageBackend::Yaml),
            "sqlite" | "db" | "sql" => Ok(StorageBackend::Sqlite),
            _ => Err(format!("unknown backend '{}': expected 'yaml' or 'sqlite'", s)),
        }
    }
}

// =============================================================================
// Graph ↔ SQLite Bridge
// =============================================================================

/// Load a complete `Graph` from a SQLite database.
///
/// Reads all nodes (with tags, metadata, knowledge) and all edges,
/// plus project metadata, and assembles them into a `Graph` struct.
#[cfg(feature = "sqlite")]
pub fn load_graph_from_sqlite(db_path: &std::path::Path) -> Result<crate::Graph, StorageError> {
    let storage = SqliteStorage::open(db_path)?;

    // Load project metadata
    let project = storage.get_project_meta()?;

    // Load all nodes
    let ids = storage.get_all_node_ids()?;
    let mut nodes = Vec::with_capacity(ids.len());
    for id in &ids {
        if let Some(node) = storage.get_node(id)? {
            nodes.push(node);
        }
    }

    // Load all edges (deduplicated via HashSet since get_edges returns both directions)
    let mut seen_edges = std::collections::HashSet::new();
    let mut edges = Vec::new();
    for id in &ids {
        for edge in storage.get_edges(id)? {
            let key = (edge.from.clone(), edge.to.clone(), edge.relation.clone());
            if seen_edges.insert(key) {
                edges.push(edge);
            }
        }
    }

    Ok(crate::Graph {
        project,
        nodes,
        edges,
    })
}

/// Save a complete `Graph` to a SQLite database.
///
/// Opens (or creates) the database, clears existing data, then inserts
/// all nodes and edges from the Graph. Uses batch operations for atomicity.
///
/// Note: `put_node_on()` already syncs tags, metadata, and knowledge,
/// so we only need `PutNode` + `AddEdge` operations.
#[cfg(feature = "sqlite")]
pub fn save_graph_to_sqlite(graph: &crate::Graph, db_path: &std::path::Path) -> Result<(), StorageError> {
    let storage = SqliteStorage::open(db_path)?;

    // Build batch operations: delete all existing, then insert all new
    let mut ops = Vec::new();

    // Delete existing nodes (cascades to edges, tags, metadata, knowledge)
    let existing_ids = storage.get_all_node_ids()?;
    for id in existing_ids {
        ops.push(BatchOp::DeleteNode(id));
    }

    // Insert all nodes (put_node_on handles tags, metadata, knowledge internally)
    for node in &graph.nodes {
        ops.push(BatchOp::PutNode(node.clone()));
    }

    // Insert all edges
    for edge in &graph.edges {
        ops.push(BatchOp::AddEdge(edge.clone()));
    }

    // Use migration batch (FK-disabled) to handle edge ordering
    storage.execute_migration_batch(&ops)?;

    // Set project metadata
    if let Some(ref meta) = graph.project {
        storage.set_project_meta(meta)?;
    }

    Ok(())
}

// =============================================================================
// Auto-Loading/Saving (Backend-Agnostic)
// =============================================================================

/// Load a `Graph` from the appropriate backend, auto-detected from the `.gid/` directory.
///
/// - If `graph.db` exists → load from SQLite
/// - If `graph.yml` exists → load from YAML
/// - Neither → empty Graph (YAML default)
///
/// The `gid_dir` should point to the `.gid/` directory.
/// `explicit_backend` allows CLI override via `--backend`.
pub fn load_graph_auto(
    gid_dir: &std::path::Path,
    explicit_backend: Option<StorageBackend>,
) -> Result<crate::Graph, Box<dyn std::error::Error + Send + Sync>> {
    let backend = resolve_backend(explicit_backend, gid_dir);
    match backend {
        StorageBackend::Yaml => {
            let yaml_path = gid_dir.join("graph.yml");
            if yaml_path.exists() {
                crate::load_graph(&yaml_path).map_err(|e| e.into())
            } else {
                Ok(crate::Graph::default())
            }
        }
        #[cfg(feature = "sqlite")]
        StorageBackend::Sqlite => {
            let db_path = gid_dir.join("graph.db");
            if db_path.exists() {
                load_graph_from_sqlite(&db_path).map_err(|e| e.into())
            } else {
                Ok(crate::Graph::default())
            }
        }
        #[cfg(not(feature = "sqlite"))]
        StorageBackend::Sqlite => {
            // Without the sqlite feature compiled in we can't read a real db,
            // but an empty dir (no graph.db) is unambiguously an empty graph —
            // returning that is correct regardless of which backend is "preferred".
            let db_path = gid_dir.join("graph.db");
            if db_path.exists() {
                Err("SQLite backend not available (compile with --features sqlite)".into())
            } else {
                Ok(crate::Graph::default())
            }
        }
    }
}

/// Save a `Graph` to the appropriate backend, auto-detected from the `.gid/` directory.
///
/// The `gid_dir` should point to the `.gid/` directory.
/// `explicit_backend` allows CLI override via `--backend`.
pub fn save_graph_auto(
    graph: &crate::Graph,
    gid_dir: &std::path::Path,
    explicit_backend: Option<StorageBackend>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let backend = resolve_backend(explicit_backend, gid_dir);
    match backend {
        StorageBackend::Yaml => {
            let yaml_path = gid_dir.join("graph.yml");
            crate::save_graph(graph, &yaml_path).map_err(|e| e.into())
        }
        #[cfg(feature = "sqlite")]
        StorageBackend::Sqlite => {
            let db_path = gid_dir.join("graph.db");
            save_graph_to_sqlite(graph, &db_path).map_err(|e| e.into())
        }
        #[cfg(not(feature = "sqlite"))]
        StorageBackend::Sqlite => {
            Err("SQLite backend not available (compile with --features sqlite)".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_detect_yaml_when_graph_yml_exists() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("graph.yml"), "nodes: []\nedges: []\n").unwrap();
        assert_eq!(detect_backend(tmp.path()), StorageBackend::Yaml);
    }

    #[test]
    fn test_detect_sqlite_when_graph_db_exists() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("graph.db"), "").unwrap(); // empty file simulates DB
        assert_eq!(detect_backend(tmp.path()), StorageBackend::Sqlite);
    }

    #[test]
    fn test_detect_sqlite_takes_priority_over_yaml() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("graph.yml"), "nodes: []\n").unwrap();
        fs::write(tmp.path().join("graph.db"), "").unwrap();
        // Both exist — SQLite takes priority (migrated project)
        assert_eq!(detect_backend(tmp.path()), StorageBackend::Sqlite);
    }

    #[test]
    fn test_detect_defaults_to_sqlite_empty_dir() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(detect_backend(tmp.path()), StorageBackend::Sqlite);
    }

    #[test]
    fn test_resolve_explicit_overrides_detection() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("graph.db"), "").unwrap();
        // DB exists → would auto-detect SQLite, but explicit says YAML
        assert_eq!(
            resolve_backend(Some(StorageBackend::Yaml), tmp.path()),
            StorageBackend::Yaml
        );
    }

    #[test]
    fn test_resolve_none_delegates_to_detection() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("graph.yml"), "").unwrap();
        assert_eq!(
            resolve_backend(None, tmp.path()),
            StorageBackend::Yaml
        );
    }

    #[test]
    fn test_backend_display() {
        assert_eq!(StorageBackend::Yaml.to_string(), "yaml");
        assert_eq!(StorageBackend::Sqlite.to_string(), "sqlite");
    }

    #[test]
    fn test_backend_equality() {
        assert_eq!(StorageBackend::Yaml, StorageBackend::Yaml);
        assert_eq!(StorageBackend::Sqlite, StorageBackend::Sqlite);
        assert_ne!(StorageBackend::Yaml, StorageBackend::Sqlite);
    }

    #[test]
    fn test_backend_from_str() {
        assert_eq!("yaml".parse::<StorageBackend>().unwrap(), StorageBackend::Yaml);
        assert_eq!("yml".parse::<StorageBackend>().unwrap(), StorageBackend::Yaml);
        assert_eq!("sqlite".parse::<StorageBackend>().unwrap(), StorageBackend::Sqlite);
        assert_eq!("db".parse::<StorageBackend>().unwrap(), StorageBackend::Sqlite);
        assert_eq!("sql".parse::<StorageBackend>().unwrap(), StorageBackend::Sqlite);
        assert!("unknown".parse::<StorageBackend>().is_err());
    }

    #[test]
    fn test_backend_from_str_case_insensitive() {
        assert_eq!("YAML".parse::<StorageBackend>().unwrap(), StorageBackend::Yaml);
        assert_eq!("SQLite".parse::<StorageBackend>().unwrap(), StorageBackend::Sqlite);
        assert_eq!("DB".parse::<StorageBackend>().unwrap(), StorageBackend::Sqlite);
    }

    // ── load_graph_auto tests ──────────────────────────────

    #[test]
    fn test_load_graph_auto_yaml() {
        let tmp = TempDir::new().unwrap();
        let yaml = "project:\n  name: test\nnodes:\n  - id: n1\n    title: Node 1\nedges: []\n";
        fs::write(tmp.path().join("graph.yml"), yaml).unwrap();

        let graph = load_graph_auto(tmp.path(), None).unwrap();
        assert_eq!(graph.project.as_ref().unwrap().name, "test");
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id, "n1");
    }

    #[test]
    fn test_load_graph_auto_empty_dir_returns_default() {
        let tmp = TempDir::new().unwrap();
        let graph = load_graph_auto(tmp.path(), None).unwrap();
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn test_load_graph_auto_explicit_yaml_override() {
        let tmp = TempDir::new().unwrap();
        let yaml = "nodes:\n  - id: y1\n    title: YAML node\nedges: []\n";
        fs::write(tmp.path().join("graph.yml"), yaml).unwrap();
        // Create a graph.db too — but explicit YAML should win
        // (We don't create a real DB here since explicit override skips detection)

        let graph = load_graph_auto(tmp.path(), Some(StorageBackend::Yaml)).unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id, "y1");
    }

    // ── save_graph_auto tests ──────────────────────────────

    #[test]
    fn test_save_graph_auto_yaml() {
        let tmp = TempDir::new().unwrap();
        let graph = crate::Graph {
            project: Some(crate::graph::ProjectMeta {
                name: "test-save".into(),
                description: None,
            }),
            nodes: vec![crate::graph::Node::new("s1", "Saved 1")],
            edges: vec![],
        };

        save_graph_auto(&graph, tmp.path(), Some(StorageBackend::Yaml)).unwrap();
        assert!(tmp.path().join("graph.yml").exists());

        // Verify roundtrip
        let loaded = load_graph_auto(tmp.path(), Some(StorageBackend::Yaml)).unwrap();
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(loaded.nodes[0].id, "s1");
        assert_eq!(loaded.project.unwrap().name, "test-save");
    }

    // ── SQLite bridge tests (require sqlite feature) ───────

    #[cfg(feature = "sqlite")]
    mod sqlite_bridge {
        use super::*;
        use crate::graph::{Node, Edge, NodeStatus, ProjectMeta};
        use crate::task_graph_knowledge::KnowledgeNode;
        use std::collections::HashMap;

        #[test]
        fn test_save_and_load_roundtrip() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let graph = crate::Graph {
                project: Some(ProjectMeta {
                    name: "roundtrip-test".into(),
                    description: Some("Testing roundtrip".into()),
                }),
                nodes: vec![
                    Node::new("a", "Alpha"),
                    Node::new("b", "Beta"),
                ],
                edges: vec![
                    Edge::new("a", "b", "depends_on"),
                ],
            };

            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            assert_eq!(loaded.project.as_ref().unwrap().name, "roundtrip-test");
            assert_eq!(loaded.project.as_ref().unwrap().description.as_deref(), Some("Testing roundtrip"));
            assert_eq!(loaded.nodes.len(), 2);
            assert_eq!(loaded.edges.len(), 1);
            assert_eq!(loaded.edges[0].from, "a");
            assert_eq!(loaded.edges[0].to, "b");
            assert_eq!(loaded.edges[0].relation, "depends_on");
        }

        #[test]
        fn test_roundtrip_with_tags_and_metadata() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let mut node = Node::new("tagged", "Tagged Node");
            node.tags = vec!["urgent".into(), "backend".into()];
            node.metadata.insert("priority".into(), serde_json::json!("high"));
            node.metadata.insert("count".into(), serde_json::json!(42));

            let graph = crate::Graph {
                project: None,
                nodes: vec![node],
                edges: vec![],
            };

            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            let n = &loaded.nodes[0];
            assert_eq!(n.tags.len(), 2);
            assert!(n.tags.contains(&"urgent".into()));
            assert!(n.tags.contains(&"backend".into()));
            assert_eq!(n.metadata.get("priority"), Some(&serde_json::json!("high")));
            assert_eq!(n.metadata.get("count"), Some(&serde_json::json!(42)));
        }

        #[test]
        fn test_roundtrip_with_knowledge() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let mut node = Node::new("knowledgeable", "Smart Node");
            node.knowledge = KnowledgeNode {
                findings: HashMap::from([
                    ("FINDING-1".into(), "Bug found in parser".into()),
                ]),
                file_cache: HashMap::from([
                    ("src/main.rs".into(), "fn main() {}".into()),
                ]),
                tool_history: vec![],
            };

            let graph = crate::Graph {
                project: None,
                nodes: vec![node],
                edges: vec![],
            };

            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            let n = &loaded.nodes[0];
            assert_eq!(n.knowledge.findings.get("FINDING-1").unwrap(), "Bug found in parser");
            assert_eq!(n.knowledge.file_cache.get("src/main.rs").unwrap(), "fn main() {}");
        }

        #[test]
        fn test_roundtrip_node_statuses() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let mut todo = Node::new("t1", "Todo");
            todo.status = NodeStatus::Todo;
            let mut done = Node::new("t2", "Done");
            done.status = NodeStatus::Done;
            let mut ip = Node::new("t3", "InProgress");
            ip.status = NodeStatus::InProgress;

            let graph = crate::Graph {
                project: None,
                nodes: vec![todo, done, ip],
                edges: vec![],
            };

            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            let find = |id: &str| loaded.nodes.iter().find(|n| n.id == id).unwrap();
            assert_eq!(find("t1").status, NodeStatus::Todo);
            assert_eq!(find("t2").status, NodeStatus::Done);
            assert_eq!(find("t3").status, NodeStatus::InProgress);
        }

        #[test]
        fn test_roundtrip_edge_metadata() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let mut edge = Edge::new("a", "b", "calls");
            edge.weight = Some(0.8);
            edge.confidence = Some(0.95);

            let graph = crate::Graph {
                project: None,
                nodes: vec![Node::new("a", "A"), Node::new("b", "B")],
                edges: vec![edge],
            };

            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            assert_eq!(loaded.edges.len(), 1);
            assert!((loaded.edges[0].weight.unwrap() - 0.8).abs() < 0.001);
            assert!((loaded.edges[0].confidence.unwrap() - 0.95).abs() < 0.001);
        }

        #[test]
        fn test_roundtrip_empty_graph() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let graph = crate::Graph::default();
            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            assert!(loaded.nodes.is_empty());
            assert!(loaded.edges.is_empty());
        }

        #[test]
        fn test_save_overwrites_existing_data() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            // First save
            let graph1 = crate::Graph {
                project: Some(ProjectMeta { name: "v1".into(), description: None }),
                nodes: vec![Node::new("old", "Old Node")],
                edges: vec![],
            };
            save_graph_to_sqlite(&graph1, &db_path).unwrap();

            // Second save — should replace
            let graph2 = crate::Graph {
                project: Some(ProjectMeta { name: "v2".into(), description: None }),
                nodes: vec![Node::new("new", "New Node")],
                edges: vec![],
            };
            save_graph_to_sqlite(&graph2, &db_path).unwrap();

            let loaded = load_graph_from_sqlite(&db_path).unwrap();
            assert_eq!(loaded.project.unwrap().name, "v2");
            assert_eq!(loaded.nodes.len(), 1);
            assert_eq!(loaded.nodes[0].id, "new");
        }

        #[test]
        fn test_load_graph_auto_detects_sqlite() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let graph = crate::Graph {
                project: Some(ProjectMeta { name: "auto-sqlite".into(), description: None }),
                nodes: vec![Node::new("x", "X")],
                edges: vec![],
            };
            save_graph_to_sqlite(&graph, &db_path).unwrap();

            // Auto-detect should find graph.db and use SQLite
            let loaded = load_graph_auto(tmp.path(), None).unwrap();
            assert_eq!(loaded.project.unwrap().name, "auto-sqlite");
            assert_eq!(loaded.nodes.len(), 1);
        }

        #[test]
        fn test_save_graph_auto_sqlite() {
            let tmp = TempDir::new().unwrap();

            let graph = crate::Graph {
                project: Some(ProjectMeta { name: "auto-save".into(), description: None }),
                nodes: vec![Node::new("as1", "Auto Saved")],
                edges: vec![],
            };

            save_graph_auto(&graph, tmp.path(), Some(StorageBackend::Sqlite)).unwrap();
            assert!(tmp.path().join("graph.db").exists());

            let loaded = load_graph_auto(tmp.path(), None).unwrap();
            assert_eq!(loaded.project.unwrap().name, "auto-save");
            assert_eq!(loaded.nodes[0].id, "as1");
        }

        #[test]
        fn test_roundtrip_many_nodes_and_edges() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let nodes: Vec<Node> = (0..50).map(|i| {
                let mut n = Node::new(&format!("n{}", i), &format!("Node {}", i));
                n.tags = vec![format!("group-{}", i % 5)];
                n
            }).collect();
            let edges: Vec<Edge> = (0..49).map(|i| {
                Edge::new(&format!("n{}", i), &format!("n{}", i + 1), "depends_on")
            }).collect();

            let graph = crate::Graph {
                project: Some(ProjectMeta { name: "big".into(), description: None }),
                nodes,
                edges,
            };

            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            assert_eq!(loaded.nodes.len(), 50);
            assert_eq!(loaded.edges.len(), 49);
            // Verify tags survived
            let n0 = loaded.nodes.iter().find(|n| n.id == "n0").unwrap();
            assert!(n0.tags.contains(&"group-0".into()));
        }

        #[test]
        fn test_roundtrip_code_node_fields() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            let mut node = Node::new("fn:main", "main");
            node.node_type = Some("function".into());
            node.file_path = Some("src/main.rs".into());
            node.lang = Some("rust".into());
            node.start_line = Some(1);
            node.end_line = Some(10);
            node.signature = Some("fn main() -> Result<()>".into());
            node.visibility = Some("pub".into());
            node.doc_comment = Some("Entry point".into());
            node.source = Some("code_extract".into());
            node.body_hash = Some("abc123".into());

            let graph = crate::Graph {
                project: None,
                nodes: vec![node],
                edges: vec![],
            };

            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            let n = &loaded.nodes[0];
            assert_eq!(n.node_type.as_deref(), Some("function"));
            assert_eq!(n.file_path.as_deref(), Some("src/main.rs"));
            assert_eq!(n.lang.as_deref(), Some("rust"));
            assert_eq!(n.start_line, Some(1));
            assert_eq!(n.end_line, Some(10));
            assert_eq!(n.signature.as_deref(), Some("fn main() -> Result<()>"));
            assert_eq!(n.visibility.as_deref(), Some("pub"));
            assert_eq!(n.doc_comment.as_deref(), Some("Entry point"));
            assert_eq!(n.source.as_deref(), Some("code_extract"));
        }

        #[test]
        fn test_edge_deduplication_in_load() {
            let tmp = TempDir::new().unwrap();
            let db_path = tmp.path().join("graph.db");

            // Create graph with bidirectional edges that share nodes
            let graph = crate::Graph {
                project: None,
                nodes: vec![
                    Node::new("a", "A"),
                    Node::new("b", "B"),
                    Node::new("c", "C"),
                ],
                edges: vec![
                    Edge::new("a", "b", "calls"),
                    Edge::new("b", "c", "calls"),
                    Edge::new("a", "c", "depends_on"),
                ],
            };

            save_graph_to_sqlite(&graph, &db_path).unwrap();
            let loaded = load_graph_from_sqlite(&db_path).unwrap();

            // Should have exactly 3 edges, not duplicated
            assert_eq!(loaded.edges.len(), 3);
        }

        #[test]
        fn test_explicit_sqlite_when_both_exist() {
            let tmp = TempDir::new().unwrap();

            // Create YAML with different data
            let yaml = "nodes:\n  - id: yaml-node\n    title: From YAML\nedges: []\n";
            fs::write(tmp.path().join("graph.yml"), yaml).unwrap();

            // Create SQLite with different data
            let graph = crate::Graph {
                project: None,
                nodes: vec![Node::new("sqlite-node", "From SQLite")],
                edges: vec![],
            };
            save_graph_to_sqlite(&graph, &tmp.path().join("graph.db")).unwrap();

            // Explicit YAML → should get YAML data
            let loaded_yaml = load_graph_auto(tmp.path(), Some(StorageBackend::Yaml)).unwrap();
            assert_eq!(loaded_yaml.nodes[0].id, "yaml-node");

            // Explicit SQLite → should get SQLite data
            let loaded_sqlite = load_graph_auto(tmp.path(), Some(StorageBackend::Sqlite)).unwrap();
            assert_eq!(loaded_sqlite.nodes[0].id, "sqlite-node");

            // Auto → should prefer SQLite (migrated project)
            let loaded_auto = load_graph_auto(tmp.path(), None).unwrap();
            assert_eq!(loaded_auto.nodes[0].id, "sqlite-node");
        }
    }
}
