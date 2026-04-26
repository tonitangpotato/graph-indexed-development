//! History tracking for GID graphs.
//!
//! Save snapshots with timestamps, list/diff/restore versions.

// GOAL-3.1: SQLite backend snapshots use `save_snapshot_sqlite()` with rusqlite::backup::Backup
// for atomic, consistent .db snapshots. YAML backend uses `save_snapshot()` with serde_yaml.
// The rusqlite "backup" feature is enabled in Cargo.toml.

use std::path::{Path, PathBuf};
use std::fs;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use crate::graph::Graph;
use crate::parser::load_graph;  // for load_version() — history snapshots are always YAML
use crate::storage::{load_graph_auto, save_graph_auto, StorageBackend};  // for restore()

#[cfg(feature = "sqlite")]
use sha2::{Sha256, Digest};

/// Maximum number of history entries to keep.
const MAX_HISTORY_ENTRIES: usize = 50;

/// A history snapshot entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Filename of the snapshot (e.g., "2024-03-25T12-30-00Z.yml")
    pub filename: String,
    /// ISO 8601 timestamp
    pub timestamp: String,
    /// Optional commit-like message
    pub message: Option<String>,
    /// Number of nodes in this snapshot
    pub node_count: usize,
    /// Number of edges in this snapshot
    pub edge_count: usize,
    /// Git commit hash if available
    pub git_commit: Option<String>,
}

/// Diff result between two graph versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphDiff {
    /// Nodes added in the newer version
    pub added_nodes: Vec<String>,
    /// Nodes removed from the older version
    pub removed_nodes: Vec<String>,
    /// Nodes that changed (status, title, etc.)
    pub modified_nodes: Vec<String>,
    /// Number of edges added
    pub added_edges: usize,
    /// Number of edges removed
    pub removed_edges: usize,
}

impl std::fmt::Display for GraphDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return write!(f, "No differences found.");
        }
        
        let mut lines = Vec::new();
        
        if !self.added_nodes.is_empty() {
            lines.push(format!("+ Added nodes ({}):", self.added_nodes.len()));
            for node in self.added_nodes.iter().take(10) {
                lines.push(format!("    + {}", node));
            }
            if self.added_nodes.len() > 10 {
                lines.push(format!("    ... and {} more", self.added_nodes.len() - 10));
            }
        }
        
        if !self.removed_nodes.is_empty() {
            lines.push(format!("- Removed nodes ({}):", self.removed_nodes.len()));
            for node in self.removed_nodes.iter().take(10) {
                lines.push(format!("    - {}", node));
            }
            if self.removed_nodes.len() > 10 {
                lines.push(format!("    ... and {} more", self.removed_nodes.len() - 10));
            }
        }
        
        if !self.modified_nodes.is_empty() {
            lines.push(format!("~ Modified nodes ({}):", self.modified_nodes.len()));
            for node in self.modified_nodes.iter().take(10) {
                lines.push(format!("    ~ {}", node));
            }
            if self.modified_nodes.len() > 10 {
                lines.push(format!("    ... and {} more", self.modified_nodes.len() - 10));
            }
        }
        
        if self.added_edges > 0 || self.removed_edges > 0 {
            lines.push("Edge changes:".to_string());
            if self.added_edges > 0 {
                lines.push(format!("    + {} edges added", self.added_edges));
            }
            if self.removed_edges > 0 {
                lines.push(format!("    - {} edges removed", self.removed_edges));
            }
        }
        
        write!(f, "{}", lines.join("\n"))
    }
}

impl GraphDiff {
    pub fn is_empty(&self) -> bool {
        self.added_nodes.is_empty()
            && self.removed_nodes.is_empty()
            && self.modified_nodes.is_empty()
            && self.added_edges == 0
            && self.removed_edges == 0
    }
}

/// History manager for a GID project.
pub struct HistoryManager {
    history_dir: PathBuf,
}

impl HistoryManager {
    /// Create a new history manager for the given .gid directory.
    pub fn new(gid_dir: &Path) -> Self {
        Self {
            history_dir: gid_dir.join("history"),
        }
    }
    
    /// Ensure the history directory exists.
    fn ensure_dir(&self) -> Result<()> {
        if !self.history_dir.exists() {
            fs::create_dir_all(&self.history_dir)
                .with_context(|| format!("Failed to create history directory: {}", self.history_dir.display()))?;
        }
        Ok(())
    }
    
    /// Save a snapshot of the current graph.
    pub fn save_snapshot(&self, graph: &Graph, message: Option<&str>) -> Result<String> {
        let start = std::time::Instant::now();
        self.ensure_dir()?;
        
        let timestamp = Utc::now();
        let filename = format!("{}.yml", timestamp.format("%Y-%m-%dT%H-%M-%SZ"));
        let filepath = self.history_dir.join(&filename);
        
        // Add message as a comment at the top if provided
        let yaml = if let Some(msg) = message {
            format!("# {}\n{}", msg, serde_yaml::to_string(graph)?)
        } else {
            serde_yaml::to_string(graph)?
        };
        
        let file_size = yaml.len();
        fs::write(&filepath, &yaml)
            .with_context(|| format!("Failed to save snapshot: {}", filepath.display()))?;
        
        // Clean up old history entries
        self.cleanup()?;
        
        let elapsed = start.elapsed();
        tracing::info!(
            filename = %filename,
            file_size_bytes = file_size,
            elapsed_ms = elapsed.as_millis() as u64,
            "saved history snapshot"
        );
        
        Ok(filename)
    }
    
    /// Save a snapshot of the current SQLite graph database using the Backup API.
    ///
    /// Uses `rusqlite::backup::Backup` for atomic, consistent point-in-time snapshots
    /// even with concurrent readers and WAL mode enabled.
    ///
    /// Returns the snapshot filename (e.g., "2026-04-09T13-45-00Z.db").
    ///
    /// [GOAL 3.1]
    #[cfg(feature = "sqlite")]
    pub fn save_snapshot_sqlite(
        &self,
        db: &rusqlite::Connection,
        message: Option<&str>,
    ) -> Result<String> {
        let start = std::time::Instant::now();
        self.ensure_dir()?;

        let timestamp = chrono::Utc::now();
        // Generate unique filename, handle same-second collisions
        let base = timestamp.format("%Y-%m-%dT%H-%M-%SZ").to_string();
        let filename = {
            let candidate = format!("{}.db", base);
            if !self.history_dir.join(&candidate).exists() {
                candidate
            } else {
                let mut suffix = 1;
                loop {
                    let candidate = format!("{}-{}.db", base, suffix);
                    if !self.history_dir.join(&candidate).exists() {
                        break candidate;
                    }
                    suffix += 1;
                }
            }
        };
        let dest_path = self.history_dir.join(&filename);

        // Perform backup via SQLite Backup API
        let mut dest_conn = rusqlite::Connection::open(&dest_path)
            .with_context(|| format!("Failed to open destination: {}", dest_path.display()))?;
        {
            let backup = rusqlite::backup::Backup::new(db, &mut dest_conn)
                .with_context(|| "Failed to initialize SQLite backup")?;
            backup
                .run_to_completion(256, std::time::Duration::from_millis(50), None)
                .with_context(|| "SQLite backup failed")?;
        }
        drop(dest_conn);

        // Verify snapshot integrity
        {
            let verify_conn = rusqlite::Connection::open(&dest_path)?;
            let integrity: String =
                verify_conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            if integrity != "ok" {
                fs::remove_file(&dest_path)?;
                anyhow::bail!("Snapshot integrity check failed: {}", integrity);
            }
        }

        // Compute SHA-256 checksum
        let checksum = {
            let mut file = std::fs::File::open(&dest_path)?;
            let mut hasher = Sha256::new();
            let mut buf = [0u8; 8192];
            loop {
                use std::io::Read;
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            format!("sha256:{:x}", hasher.finalize())
        };

        let file_size = fs::metadata(&dest_path)?.len();

        // Store message as a sidecar .meta file for this snapshot
        if let Some(msg) = message {
            let meta_path = dest_path.with_extension("db.meta");
            let meta = serde_json::json!({
                "message": msg,
                "created_at": timestamp.to_rfc3339(),
                "checksum": checksum,
                "size_bytes": file_size,
            });
            fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
        }

        // Clean up old history entries
        self.cleanup()?;

        let elapsed = start.elapsed();
        tracing::info!(
            filename = %filename,
            file_size_bytes = file_size,
            checksum = %checksum,
            elapsed_ms = elapsed.as_millis() as u64,
            "saved SQLite history snapshot via backup API"
        );

        Ok(filename)
    }

    /// List all history snapshots.
    pub fn list_snapshots(&self) -> Result<Vec<HistoryEntry>> {
        if !self.history_dir.exists() {
            return Ok(Vec::new());
        }
        
        let mut entries = Vec::new();
        
        let mut files: Vec<_> = fs::read_dir(&self.history_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().is_some_and(|ext| ext == "yml" || ext == "yaml")
            })
            .collect();
        
        // Sort by filename (which includes timestamp) in descending order
        files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        
        for entry in files {
            let filepath = entry.path();
            let filename = entry.file_name().to_string_lossy().to_string();
            
            // Extract timestamp from filename
            let timestamp = filename
                .trim_end_matches(".yml")
                .trim_end_matches(".yaml")
                .replace('T', " ")
                .replace('-', ":");
            
            // Try to load the graph to get stats
            if let Ok(content) = fs::read_to_string(&filepath) {
                // Extract message from first line if it's a comment
                let message = content.lines().next()
                    .filter(|l| l.starts_with("# "))
                    .map(|l| l[2..].to_string());
                
                // Parse the graph
                if let Ok(graph) = serde_yaml::from_str::<Graph>(&content) {
                    entries.push(HistoryEntry {
                        filename,
                        timestamp,
                        message,
                        node_count: graph.nodes.len(),
                        edge_count: graph.edges.len(),
                        git_commit: None, // TODO: Extract from metadata
                    });
                }
            }
        }
        
        Ok(entries)
    }
    
    /// Load a historical version by filename.
    pub fn load_version(&self, filename: &str) -> Result<Graph> {
        let filepath = self.history_dir.join(filename);
        
        if !filepath.exists() {
            bail!("History version not found: {}", filename);
        }
        
        load_graph(&filepath)
    }
    
    /// Compute diff between two graphs.
    pub fn diff(older: &Graph, newer: &Graph) -> GraphDiff {
        use std::collections::{HashMap, HashSet};
        
        let old_nodes: HashSet<&str> = older.nodes.iter().map(|n| n.id.as_str()).collect();
        let new_nodes: HashSet<&str> = newer.nodes.iter().map(|n| n.id.as_str()).collect();
        
        let added_nodes: Vec<String> = new_nodes.difference(&old_nodes)
            .map(|s| s.to_string())
            .collect();
        
        let removed_nodes: Vec<String> = old_nodes.difference(&new_nodes)
            .map(|s| s.to_string())
            .collect();
        
        // Find modified nodes (same ID but different content)
        let old_node_map: HashMap<&str, &crate::graph::Node> = 
            older.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let new_node_map: HashMap<&str, &crate::graph::Node> = 
            newer.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        
        let mut modified_nodes = Vec::new();
        for id in old_nodes.intersection(&new_nodes) {
            if let (Some(old), Some(new)) = (old_node_map.get(id), new_node_map.get(id)) {
                if old.status != new.status || old.title != new.title || old.description != new.description {
                    modified_nodes.push(id.to_string());
                }
            }
        }
        
        // Edge comparison
        let old_edges: HashSet<(&str, &str, &str)> = older.edges.iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.relation.as_str()))
            .collect();
        let new_edges: HashSet<(&str, &str, &str)> = newer.edges.iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.relation.as_str()))
            .collect();
        
        let added_edges = new_edges.difference(&old_edges).count();
        let removed_edges = old_edges.difference(&new_edges).count();
        
        GraphDiff {
            added_nodes,
            removed_nodes,
            modified_nodes,
            added_edges,
            removed_edges,
        }
    }
    
    /// Diff current graph against a historical version.
    pub fn diff_against(&self, version: &str, current: &Graph) -> Result<GraphDiff> {
        let start = std::time::Instant::now();
        let historical = self.load_version(version)?;
        let diff = Self::diff(&historical, current);
        let elapsed = start.elapsed();
        tracing::info!(
            version = %version,
            added = diff.added_nodes.len(),
            removed = diff.removed_nodes.len(),
            modified = diff.modified_nodes.len(),
            added_edges = diff.added_edges,
            removed_edges = diff.removed_edges,
            elapsed_ms = elapsed.as_millis() as u64,
            "diff_against complete"
        );
        Ok(diff)
    }
    
    /// Diff two historical snapshots against each other.
    pub fn diff_versions(&self, version_a: &str, version_b: &str) -> Result<GraphDiff> {
        let start = std::time::Instant::now();
        let graph_a = self.load_version(version_a)?;
        let graph_b = self.load_version(version_b)?;
        let diff = Self::diff(&graph_a, &graph_b);
        let elapsed = start.elapsed();
        tracing::info!(
            version_a = %version_a,
            version_b = %version_b,
            added = diff.added_nodes.len(),
            removed = diff.removed_nodes.len(),
            modified = diff.modified_nodes.len(),
            added_edges = diff.added_edges,
            removed_edges = diff.removed_edges,
            elapsed_ms = elapsed.as_millis() as u64,
            "diff_versions complete"
        );
        Ok(diff)
    }
    
    /// Restore a historical version to the main graph file.
    pub fn restore(&self, version: &str, gid_dir: &Path, backend: Option<StorageBackend>) -> Result<()> {
        let start = std::time::Instant::now();
        let historical = self.load_version(version)?;
        
        // Save current state to history first
        if let Ok(current) = load_graph_auto(gid_dir, backend) {
            if !current.nodes.is_empty() || !current.edges.is_empty() {
                self.save_snapshot(&current, Some("Auto-snapshot before restore"))?;
            }
        }
        
        // Write the historical version as the current graph
        save_graph_auto(&historical, gid_dir, backend).map_err(|e| anyhow::anyhow!("{e}"))?;
        
        let elapsed = start.elapsed();
        tracing::info!(
            version = %version,
            elapsed_ms = elapsed.as_millis() as u64,
            "restored historical version"
        );
        
        Ok(())
    }
    
    /// Clean up old history entries, keeping only the most recent N.
    fn cleanup(&self) -> Result<()> {
        let mut files: Vec<_> = fs::read_dir(&self.history_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().extension().is_some_and(|ext| ext == "yml" || ext == "yaml")
            })
            .collect();
        
        // Sort by filename in ascending order (oldest first)
        files.sort_by_key(|a| a.file_name());
        
        // Remove oldest files if we have too many
        while files.len() > MAX_HISTORY_ENTRIES {
            if let Some(oldest) = files.first() {
                fs::remove_file(oldest.path()).ok();
                files.remove(0);
            }
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Node, Edge, NodeStatus, ProjectMeta};
    use crate::parser::save_graph;
    use tempfile::TempDir;
    
    #[test]
    fn test_diff_empty_graphs() {
        let g1 = Graph::new();
        let g2 = Graph::new();
        let diff = HistoryManager::diff(&g1, &g2);
        assert!(diff.is_empty());
    }
    
    #[test]
    fn test_diff_added_nodes() {
        let g1 = Graph::new();
        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "Node A"));
        
        let diff = HistoryManager::diff(&g1, &g2);
        assert_eq!(diff.added_nodes, vec!["a"]);
        assert!(diff.removed_nodes.is_empty());
    }
    
    #[test]
    fn test_save_and_load_snapshot() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        
        let mgr = HistoryManager::new(&gid_dir);
        
        let mut graph = Graph::new();
        graph.add_node(Node::new("test", "Test Node"));
        
        let filename = mgr.save_snapshot(&graph, Some("Test snapshot")).unwrap();
        
        let loaded = mgr.load_version(&filename).unwrap();
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(loaded.nodes[0].id, "test");
    }

    #[test]
    fn test_save_prunes_old_snapshots() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();

        let mgr = HistoryManager::new(&gid_dir);
        let graph = Graph::new();

        // Create MAX + 5 snapshots via save_snapshot
        for i in 0..(MAX_HISTORY_ENTRIES + 5) {
            let ts = format!("2024-01-01T00-00-{:02}Z.yml", i);
            let path = mgr.history_dir.join(&ts);
            fs::create_dir_all(&mgr.history_dir).unwrap();
            fs::write(&path, serde_yaml::to_string(&graph).unwrap()).unwrap();
        }

        // Now save one more — this should trigger cleanup
        mgr.save_snapshot(&graph, Some("trigger prune")).unwrap();

        // Count remaining files
        let count = fs::read_dir(&mgr.history_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "yml"))
            .count();

        assert!(
            count <= MAX_HISTORY_ENTRIES,
            "Expected at most {} snapshots after prune, got {}",
            MAX_HISTORY_ENTRIES,
            count
        );
    }

    // ── Roundtrip Tests ──

    #[test]
    fn test_roundtrip_graph_with_all_fields() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        graph.project = Some(ProjectMeta {
            name: "roundtrip-test".to_string(),
            description: Some("Full field roundtrip".to_string()),
        });

        let mut node = Node::new("task-1", "Implement feature X")
            .with_description("A complex task with all fields populated")
            .with_status(NodeStatus::InProgress)
            .with_tags(vec!["rust".to_string(), "backend".to_string()])
            .with_priority(10);
        node.node_type = Some("task".to_string());
        node.assigned_to = Some("potato".to_string());
        node.file_path = Some("src/main.rs".to_string());
        node.lang = Some("rust".to_string());
        node.start_line = Some(42);
        node.end_line = Some(100);
        node.signature = Some("fn main() -> Result<()>".to_string());
        node.visibility = Some("public".to_string());
        node.doc_comment = Some("/// Entry point".to_string());
        node.body_hash = Some("abc123".to_string());
        node.node_kind = Some("function".to_string());
        node.owner = Some("team-alpha".to_string());
        node.source = Some("manual".to_string());
        node.repo = Some("gid-rs".to_string());
        node.parent_id = Some("feature-1".to_string());
        node.depth = Some(2);
        node.complexity = Some(7.5);
        node.is_public = Some(true);
        node.body = Some("fn main() { println!(\"hello\"); }".to_string());
        node.created_at = Some("2026-01-01T00:00:00Z".to_string());
        node.updated_at = Some("2026-04-08T00:00:00Z".to_string());
        node.metadata.insert("custom_key".to_string(), serde_json::json!("custom_value"));
        graph.add_node(node);

        let mut edge = Edge::new("task-1", "task-2", "depends_on");
        edge.weight = Some(0.9);
        edge.confidence = Some(0.85);
        edge.metadata = Some(serde_json::json!({"source": "extract"}));
        graph.add_edge(edge);

        let filename = mgr.save_snapshot(&graph, Some("All fields test")).unwrap();
        let loaded = mgr.load_version(&filename).unwrap();

        // Verify project meta
        let proj = loaded.project.as_ref().unwrap();
        assert_eq!(proj.name, "roundtrip-test");
        assert_eq!(proj.description.as_deref(), Some("Full field roundtrip"));

        // Verify node
        assert_eq!(loaded.nodes.len(), 1);
        let n = &loaded.nodes[0];
        assert_eq!(n.id, "task-1");
        assert_eq!(n.title, "Implement feature X");
        assert_eq!(n.status, NodeStatus::InProgress);
        assert_eq!(n.description.as_deref(), Some("A complex task with all fields populated"));
        assert_eq!(n.tags, vec!["rust", "backend"]);
        assert_eq!(n.priority, Some(10));
        assert_eq!(n.assigned_to.as_deref(), Some("potato"));
        assert_eq!(n.file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(n.lang.as_deref(), Some("rust"));
        assert_eq!(n.start_line, Some(42));
        assert_eq!(n.end_line, Some(100));
        assert_eq!(n.signature.as_deref(), Some("fn main() -> Result<()>"));
        assert_eq!(n.visibility.as_deref(), Some("public"));
        assert_eq!(n.doc_comment.as_deref(), Some("/// Entry point"));
        assert_eq!(n.body_hash.as_deref(), Some("abc123"));
        assert_eq!(n.node_kind.as_deref(), Some("function"));
        assert_eq!(n.owner.as_deref(), Some("team-alpha"));
        assert_eq!(n.source.as_deref(), Some("manual"));
        assert_eq!(n.repo.as_deref(), Some("gid-rs"));
        assert_eq!(n.parent_id.as_deref(), Some("feature-1"));
        assert_eq!(n.depth, Some(2));
        assert_eq!(n.complexity, Some(7.5));
        assert_eq!(n.is_public, Some(true));
        assert_eq!(n.body.as_deref(), Some("fn main() { println!(\"hello\"); }"));
        assert_eq!(n.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(n.updated_at.as_deref(), Some("2026-04-08T00:00:00Z"));
        assert_eq!(n.metadata.get("custom_key").unwrap(), &serde_json::json!("custom_value"));

        // Verify edge
        assert_eq!(loaded.edges.len(), 1);
        let e = &loaded.edges[0];
        assert_eq!(e.from, "task-1");
        assert_eq!(e.to, "task-2");
        assert_eq!(e.relation, "depends_on");
        assert_eq!(e.weight, Some(0.9));
        assert_eq!(e.confidence, Some(0.85));
        assert_eq!(e.source(), Some("extract"));
    }

    #[test]
    fn test_roundtrip_unicode_content() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        graph.add_node(
            Node::new("unicode-1", "实现功能 X — 中文标题")
                .with_description("描述包含 emoji 🚀 和日文 こんにちは")
                .with_tags(vec!["标签一".to_string(), "タグ".to_string()])
        );
        graph.add_edge(Edge::new("unicode-1", "unicode-2", "関連"));

        let filename = mgr.save_snapshot(&graph, Some("Unicode テスト 🎉")).unwrap();
        let loaded = mgr.load_version(&filename).unwrap();

        assert_eq!(loaded.nodes[0].title, "实现功能 X — 中文标题");
        assert_eq!(loaded.nodes[0].description.as_deref(), Some("描述包含 emoji 🚀 和日文 こんにちは"));
        assert_eq!(loaded.nodes[0].tags, vec!["标签一", "タグ"]);
        assert_eq!(loaded.edges[0].relation, "関連");
    }

    #[test]
    fn test_roundtrip_empty_graph() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        let filename = mgr.save_snapshot(&graph, None).unwrap();
        let loaded = mgr.load_version(&filename).unwrap();

        assert!(loaded.nodes.is_empty());
        assert!(loaded.edges.is_empty());
    }

    #[test]
    fn test_roundtrip_multiple_sequential_snapshots() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        // Snapshot 1: empty
        let mut graph = Graph::new();
        let f1 = mgr.save_snapshot(&graph, Some("v1: empty")).unwrap();

        // Snapshot 2: one node
        graph.add_node(Node::new("a", "Alpha"));
        // sleep a tiny bit to guarantee different timestamps
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let f2 = mgr.save_snapshot(&graph, Some("v2: one node")).unwrap();

        // Snapshot 3: two nodes + edge
        graph.add_node(Node::new("b", "Beta"));
        graph.add_edge(Edge::depends_on("b", "a"));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let f3 = mgr.save_snapshot(&graph, Some("v3: two nodes")).unwrap();

        // All three are distinct files
        assert_ne!(f1, f2);
        assert_ne!(f2, f3);

        // Load each and verify
        let g1 = mgr.load_version(&f1).unwrap();
        let g2 = mgr.load_version(&f2).unwrap();
        let g3 = mgr.load_version(&f3).unwrap();

        assert_eq!(g1.nodes.len(), 0);
        assert_eq!(g2.nodes.len(), 1);
        assert_eq!(g3.nodes.len(), 2);
        assert_eq!(g3.edges.len(), 1);
    }

    #[test]
    fn test_snapshot_message_preserved() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        let _f = mgr.save_snapshot(&graph, Some("Release v1.0.0")).unwrap();

        let entries = mgr.list_snapshots().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message.as_deref(), Some("Release v1.0.0"));
    }

    #[test]
    fn test_snapshot_no_message() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        mgr.save_snapshot(&graph, None).unwrap();

        let entries = mgr.list_snapshots().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].message.is_none());
    }

    #[test]
    fn test_snapshot_node_edge_counts_in_listing() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_node(Node::new("b", "B"));
        graph.add_node(Node::new("c", "C"));
        graph.add_edge(Edge::depends_on("b", "a"));
        graph.add_edge(Edge::depends_on("c", "b"));

        mgr.save_snapshot(&graph, None).unwrap();

        let entries = mgr.list_snapshots().unwrap();
        assert_eq!(entries[0].node_count, 3);
        assert_eq!(entries[0].edge_count, 2);
    }

    // ── List Tests ──

    #[test]
    fn test_list_empty_directory() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        // Don't even create the history dir
        let mgr = HistoryManager::new(&gid_dir);
        let entries = mgr.list_snapshots().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_list_chronological_order() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        let history_dir = gid_dir.join("history");
        fs::create_dir_all(&history_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        // Create files with known timestamps (sorted alphabetically = chronologically)
        let names = vec![
            "2026-01-01T00-00-00Z.yml",
            "2026-03-15T12-30-00Z.yml",
            "2026-04-08T23-59-59Z.yml",
        ];
        for name in &names {
            let path = history_dir.join(name);
            fs::write(&path, serde_yaml::to_string(&graph).unwrap()).unwrap();
        }

        let entries = mgr.list_snapshots().unwrap();
        assert_eq!(entries.len(), 3);
        // list_snapshots sorts descending (newest first)
        assert_eq!(entries[0].filename, "2026-04-08T23-59-59Z.yml");
        assert_eq!(entries[1].filename, "2026-03-15T12-30-00Z.yml");
        assert_eq!(entries[2].filename, "2026-01-01T00-00-00Z.yml");
    }

    #[test]
    fn test_list_ignores_non_yaml_files() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        let history_dir = gid_dir.join("history");
        fs::create_dir_all(&history_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        let yaml_content = serde_yaml::to_string(&graph).unwrap();

        fs::write(history_dir.join("2026-01-01T00-00-00Z.yml"), &yaml_content).unwrap();
        fs::write(history_dir.join("notes.txt"), "not a snapshot").unwrap();
        fs::write(history_dir.join("backup.json"), "{}").unwrap();
        fs::write(history_dir.join(".hidden"), "hidden file").unwrap();

        let entries = mgr.list_snapshots().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].filename, "2026-01-01T00-00-00Z.yml");
    }

    #[test]
    fn test_list_does_not_prune() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        let history_dir = gid_dir.join("history");
        fs::create_dir_all(&history_dir).unwrap();

        let mgr = HistoryManager::new(&gid_dir);
        let graph = Graph::new();

        // Create MAX + 3 snapshot files directly on disk
        let total = MAX_HISTORY_ENTRIES + 3;
        for i in 0..total {
            let ts = format!("2024-01-01T00-00-{:02}Z.yml", i);
            let path = history_dir.join(&ts);
            fs::write(&path, serde_yaml::to_string(&graph).unwrap()).unwrap();
        }

        // list_snapshots should NOT prune
        let entries = mgr.list_snapshots().unwrap();
        assert_eq!(entries.len(), total);

        // Verify files still on disk (no pruning happened)
        let count = fs::read_dir(&history_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "yml"))
            .count();
        assert_eq!(count, total, "list_snapshots should not prune files");
    }

    // ── Diff Tests ──

    #[test]
    fn test_diff_removed_nodes() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "Alpha"));
        g1.add_node(Node::new("b", "Beta"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "Alpha"));

        let diff = HistoryManager::diff(&g1, &g2);
        assert!(diff.added_nodes.is_empty());
        assert_eq!(diff.removed_nodes, vec!["b"]);
        assert!(diff.modified_nodes.is_empty());
    }

    #[test]
    fn test_diff_modified_status() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "Alpha").with_status(NodeStatus::Todo));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "Alpha").with_status(NodeStatus::Done));

        let diff = HistoryManager::diff(&g1, &g2);
        assert!(diff.added_nodes.is_empty());
        assert!(diff.removed_nodes.is_empty());
        assert_eq!(diff.modified_nodes, vec!["a"]);
    }

    #[test]
    fn test_diff_modified_title() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "Old Title"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "New Title"));

        let diff = HistoryManager::diff(&g1, &g2);
        assert_eq!(diff.modified_nodes, vec!["a"]);
    }

    #[test]
    fn test_diff_modified_description() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "Alpha").with_description("Old desc"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "Alpha").with_description("New desc"));

        let diff = HistoryManager::diff(&g1, &g2);
        assert_eq!(diff.modified_nodes, vec!["a"]);
    }

    #[test]
    fn test_diff_unchanged_nodes_not_reported() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "Alpha").with_status(NodeStatus::Todo).with_description("Same desc"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "Alpha").with_status(NodeStatus::Todo).with_description("Same desc"));

        let diff = HistoryManager::diff(&g1, &g2);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_diff_added_edges() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "A"));
        g1.add_node(Node::new("b", "B"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "A"));
        g2.add_node(Node::new("b", "B"));
        g2.add_edge(Edge::depends_on("b", "a"));

        let diff = HistoryManager::diff(&g1, &g2);
        assert_eq!(diff.added_edges, 1);
        assert_eq!(diff.removed_edges, 0);
    }

    #[test]
    fn test_diff_removed_edges() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "A"));
        g1.add_node(Node::new("b", "B"));
        g1.add_edge(Edge::depends_on("b", "a"));
        g1.add_edge(Edge::new("a", "b", "relates_to"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "A"));
        g2.add_node(Node::new("b", "B"));

        let diff = HistoryManager::diff(&g1, &g2);
        assert_eq!(diff.added_edges, 0);
        assert_eq!(diff.removed_edges, 2);
    }

    #[test]
    fn test_diff_edge_relation_change_counts_as_add_and_remove() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "A"));
        g1.add_node(Node::new("b", "B"));
        g1.add_edge(Edge::new("a", "b", "depends_on"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "A"));
        g2.add_node(Node::new("b", "B"));
        g2.add_edge(Edge::new("a", "b", "blocks"));

        let diff = HistoryManager::diff(&g1, &g2);
        // Edge identity is (from, to, relation), so changing relation = remove old + add new
        assert_eq!(diff.added_edges, 1);
        assert_eq!(diff.removed_edges, 1);
    }

    #[test]
    fn test_diff_complex_mixed_changes() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "Alpha").with_status(NodeStatus::Todo));
        g1.add_node(Node::new("b", "Beta"));
        g1.add_node(Node::new("c", "Gamma")); // will be removed
        g1.add_edge(Edge::depends_on("b", "a"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "Alpha").with_status(NodeStatus::Done)); // modified
        g2.add_node(Node::new("b", "Beta")); // unchanged
        g2.add_node(Node::new("d", "Delta")); // added
        g2.add_edge(Edge::depends_on("d", "a")); // new edge, old edge removed

        let diff = HistoryManager::diff(&g1, &g2);
        assert!(diff.added_nodes.contains(&"d".to_string()));
        assert!(diff.removed_nodes.contains(&"c".to_string()));
        assert!(diff.modified_nodes.contains(&"a".to_string()));
        assert!(!diff.modified_nodes.contains(&"b".to_string()));
        assert_eq!(diff.added_edges, 1);
        assert_eq!(diff.removed_edges, 1);
        assert!(!diff.is_empty());
    }

    #[test]
    fn test_diff_display_format_added() {
        let g1 = Graph::new();
        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "A"));

        let diff = HistoryManager::diff(&g1, &g2);
        let display = format!("{}", diff);
        assert!(display.contains("Added nodes (1)"));
        assert!(display.contains("+ a"));
    }

    #[test]
    fn test_diff_display_format_removed() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "A"));
        let g2 = Graph::new();

        let diff = HistoryManager::diff(&g1, &g2);
        let display = format!("{}", diff);
        assert!(display.contains("Removed nodes (1)"));
        assert!(display.contains("- a"));
    }

    #[test]
    fn test_diff_display_format_modified() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "Old Title"));
        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "New Title"));

        let diff = HistoryManager::diff(&g1, &g2);
        let display = format!("{}", diff);
        assert!(display.contains("Modified nodes (1)"));
        assert!(display.contains("~ a"));
    }

    #[test]
    fn test_diff_display_format_edges() {
        let mut g1 = Graph::new();
        g1.add_edge(Edge::depends_on("a", "b"));
        let g2 = Graph::new();

        let diff = HistoryManager::diff(&g1, &g2);
        let display = format!("{}", diff);
        assert!(display.contains("Edge changes:"));
        assert!(display.contains("1 edges removed"));
    }

    #[test]
    fn test_diff_display_empty() {
        let g1 = Graph::new();
        let g2 = Graph::new();
        let diff = HistoryManager::diff(&g1, &g2);
        let display = format!("{}", diff);
        assert_eq!(display, "No differences found.");
    }

    #[test]
    fn test_diff_display_truncates_at_10() {
        let g1 = Graph::new();
        let mut g2 = Graph::new();
        for i in 0..15 {
            g2.add_node(Node::new(&format!("node-{}", i), &format!("Node {}", i)));
        }

        let diff = HistoryManager::diff(&g1, &g2);
        let display = format!("{}", diff);
        assert!(display.contains("... and 5 more"));
    }

    #[test]
    fn test_diff_against_historical_version() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut old_graph = Graph::new();
        old_graph.add_node(Node::new("a", "A"));

        let filename = mgr.save_snapshot(&old_graph, Some("v1")).unwrap();

        let mut current = Graph::new();
        current.add_node(Node::new("a", "A"));
        current.add_node(Node::new("b", "B"));

        let diff = mgr.diff_against(&filename, &current).unwrap();
        assert_eq!(diff.added_nodes, vec!["b"]);
        assert!(diff.removed_nodes.is_empty());
    }

    // ── Restore Tests ──

    #[test]
    fn test_restore_overwrites_current_graph() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);
        let graph_path = gid_dir.join("graph.yml");

        // Save v1 with node A
        let mut v1 = Graph::new();
        v1.add_node(Node::new("a", "Alpha"));
        let v1_file = mgr.save_snapshot(&v1, Some("v1")).unwrap();

        // Write v2 as current (node B only)
        let mut v2 = Graph::new();
        v2.add_node(Node::new("b", "Beta"));
        save_graph(&v2, &graph_path).unwrap();

        // Restore v1
        mgr.restore(&v1_file, &gid_dir, Some(StorageBackend::Yaml)).unwrap();

        // Current graph should now be v1
        let current = load_graph(&graph_path).unwrap();
        assert_eq!(current.nodes.len(), 1);
        assert_eq!(current.nodes[0].id, "a");
        assert_eq!(current.nodes[0].title, "Alpha");
    }

    #[test]
    fn test_restore_creates_auto_snapshot() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);
        let graph_path = gid_dir.join("graph.yml");

        // Save v1
        let mut v1 = Graph::new();
        v1.add_node(Node::new("a", "A"));
        let v1_file = mgr.save_snapshot(&v1, Some("v1")).unwrap();

        // Ensure different timestamp for auto-snapshot
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Write v2 as current
        let mut v2 = Graph::new();
        v2.add_node(Node::new("b", "B"));
        v2.add_node(Node::new("c", "C"));
        save_graph(&v2, &graph_path).unwrap();

        let before_count = mgr.list_snapshots().unwrap().len();

        // Restore v1 — should auto-snapshot v2 first
        mgr.restore(&v1_file, &gid_dir, Some(StorageBackend::Yaml)).unwrap();

        let after = mgr.list_snapshots().unwrap();
        assert_eq!(after.len(), before_count + 1, "restore should create auto-snapshot");

        // The auto-snapshot should contain v2's data
        let auto_snap = after.iter()
            .find(|e| e.message.as_deref() == Some("Auto-snapshot before restore"))
            .expect("should have auto-snapshot");
        assert_eq!(auto_snap.node_count, 2); // v2 had nodes b + c
    }

    #[test]
    fn test_restore_preserves_all_node_data() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);
        let graph_path = gid_dir.join("graph.yml");

        let mut graph = Graph::new();
        let mut node = Node::new("task-1", "Complex Task")
            .with_status(NodeStatus::Blocked)
            .with_description("Blocked on dependencies")
            .with_tags(vec!["urgent".to_string(), "backend".to_string()]);
        node.assigned_to = Some("potato".to_string());
        node.priority = Some(5);
        graph.add_node(node);
        graph.add_edge(Edge::new("task-1", "task-2", "blocks"));

        let filename = mgr.save_snapshot(&graph, Some("original")).unwrap();

        // Write something else as current
        save_graph(&Graph::new(), &graph_path).unwrap();

        // Restore
        mgr.restore(&filename, &gid_dir, Some(StorageBackend::Yaml)).unwrap();

        let restored = load_graph(&graph_path).unwrap();
        let n = &restored.nodes[0];
        assert_eq!(n.id, "task-1");
        assert_eq!(n.title, "Complex Task");
        assert_eq!(n.status, NodeStatus::Blocked);
        assert_eq!(n.description.as_deref(), Some("Blocked on dependencies"));
        assert_eq!(n.tags, vec!["urgent", "backend"]);
        assert_eq!(n.assigned_to.as_deref(), Some("potato"));
        assert_eq!(n.priority, Some(5));
        assert_eq!(restored.edges.len(), 1);
        assert_eq!(restored.edges[0].relation, "blocks");
    }

    #[test]
    fn test_restore_nonexistent_version_fails() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let result = mgr.restore("nonexistent.yml", &gid_dir, Some(StorageBackend::Yaml));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("not found"), "Error should mention 'not found': {}", err_msg);
    }

    #[test]
    fn test_load_nonexistent_version_fails() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let result = mgr.load_version("does-not-exist.yml");
        assert!(result.is_err());
    }

    // ── Pruning / Cleanup Tests ──

    #[test]
    fn test_save_keeps_exactly_max_entries() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        let history_dir = gid_dir.join("history");
        fs::create_dir_all(&history_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);
        let graph = Graph::new();

        // Pre-populate exactly MAX entries
        for i in 0..MAX_HISTORY_ENTRIES {
            let ts = format!("2024-01-01T00-{:02}-00Z.yml", i);
            let path = history_dir.join(&ts);
            fs::write(&path, serde_yaml::to_string(&graph).unwrap()).unwrap();
        }

        // Count before
        let count_before: usize = fs::read_dir(&history_dir).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "yml"))
            .count();
        assert_eq!(count_before, MAX_HISTORY_ENTRIES);

        // Save one more — should prune oldest
        mgr.save_snapshot(&graph, None).unwrap();

        let count_after: usize = fs::read_dir(&history_dir).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "yml"))
            .count();
        assert!(count_after <= MAX_HISTORY_ENTRIES);
    }

    #[test]
    fn test_cleanup_removes_oldest_first() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        let history_dir = gid_dir.join("history");
        fs::create_dir_all(&history_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);
        let graph = Graph::new();

        // Create MAX + 2 with known timestamps
        for i in 0..(MAX_HISTORY_ENTRIES + 2) {
            let ts = format!("2024-01-01T00-{:02}-{:02}Z.yml", i / 60, i % 60);
            let path = history_dir.join(&ts);
            fs::write(&path, serde_yaml::to_string(&graph).unwrap()).unwrap();
        }

        // Save one more to trigger cleanup
        mgr.save_snapshot(&graph, None).unwrap();

        let remaining: Vec<String> = fs::read_dir(&history_dir).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "yml"))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        // The very first (oldest) file should have been pruned
        assert!(!remaining.contains(&"2024-01-01T00-00-00Z.yml".to_string()),
            "Oldest snapshot should be pruned");
    }

    // ── Edge Case Tests ──

    #[test]
    fn test_save_creates_history_directory() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        // Note: NOT creating .gid/history/ manually
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        let filename = mgr.save_snapshot(&graph, None).unwrap();
        assert!(!filename.is_empty());
        assert!(gid_dir.join("history").exists());
    }

    #[test]
    fn test_large_graph_roundtrip() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        // 100 nodes, 99 edges (chain)
        for i in 0..100 {
            let node = Node::new(
                &format!("node-{:03}", i),
                &format!("Node number {}", i),
            ).with_status(if i % 3 == 0 { NodeStatus::Done } else { NodeStatus::Todo })
             .with_tags(vec![format!("group-{}", i / 10)]);
            graph.add_node(node);
            if i > 0 {
                graph.add_edge(Edge::depends_on(
                    &format!("node-{:03}", i),
                    &format!("node-{:03}", i - 1),
                ));
            }
        }

        let filename = mgr.save_snapshot(&graph, Some("stress test")).unwrap();
        let loaded = mgr.load_version(&filename).unwrap();

        assert_eq!(loaded.nodes.len(), 100);
        assert_eq!(loaded.edges.len(), 99);

        // Spot check some nodes
        let node_50 = loaded.nodes.iter().find(|n| n.id == "node-050").unwrap();
        assert_eq!(node_50.title, "Node number 50");
        assert_eq!(node_50.tags, vec!["group-5"]);
    }

    #[test]
    fn test_diff_large_graphs() {
        let mut g1 = Graph::new();
        let mut g2 = Graph::new();

        // Both share 50 nodes, g1 has 25 extra, g2 has 25 extra, 10 are modified
        for i in 0..75 {
            g1.add_node(Node::new(&format!("n-{}", i), &format!("Node {}", i)));
        }
        for i in 0..50 {
            if i < 10 {
                // Modified: different title
                g2.add_node(Node::new(&format!("n-{}", i), &format!("Modified Node {}", i)));
            } else {
                g2.add_node(Node::new(&format!("n-{}", i), &format!("Node {}", i)));
            }
        }
        for i in 75..100 {
            g2.add_node(Node::new(&format!("n-{}", i), &format!("Node {}", i)));
        }

        let diff = HistoryManager::diff(&g1, &g2);
        assert_eq!(diff.added_nodes.len(), 25); // n-75 through n-99
        assert_eq!(diff.removed_nodes.len(), 25); // n-50 through n-74
        assert_eq!(diff.modified_nodes.len(), 10); // n-0 through n-9
    }

    #[test]
    fn test_snapshot_with_all_node_statuses() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        let statuses = vec![
            ("s-todo", NodeStatus::Todo),
            ("s-progress", NodeStatus::InProgress),
            ("s-done", NodeStatus::Done),
            ("s-blocked", NodeStatus::Blocked),
            ("s-cancelled", NodeStatus::Cancelled),
            ("s-failed", NodeStatus::Failed),
            ("s-needs-resolution", NodeStatus::NeedsResolution),
        ];
        for (id, status) in &statuses {
            graph.add_node(Node::new(id, &format!("Status: {:?}", status)).with_status(status.clone()));
        }

        let filename = mgr.save_snapshot(&graph, None).unwrap();
        let loaded = mgr.load_version(&filename).unwrap();

        assert_eq!(loaded.nodes.len(), 7);
        for (id, expected_status) in &statuses {
            let node = loaded.nodes.iter().find(|n| n.id == *id).unwrap();
            assert_eq!(node.status, *expected_status, "Status mismatch for node {}", id);
        }
    }

    #[test]
    fn test_snapshot_with_project_meta() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        graph.project = Some(ProjectMeta {
            name: "test-project".to_string(),
            description: Some("A test project with description".to_string()),
        });

        let filename = mgr.save_snapshot(&graph, None).unwrap();
        let loaded = mgr.load_version(&filename).unwrap();

        let project = loaded.project.unwrap();
        assert_eq!(project.name, "test-project");
        assert_eq!(project.description.as_deref(), Some("A test project with description"));
    }

    #[test]
    fn test_snapshot_filename_is_timestamp_yml() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        let filename = mgr.save_snapshot(&graph, None).unwrap();

        // Should match pattern: YYYY-MM-DDTHH-MM-SSZ.yml
        assert!(filename.ends_with("Z.yml"), "Filename should end with Z.yml: {}", filename);
        assert!(filename.contains('T'), "Filename should contain T separator: {}", filename);
        assert_eq!(filename.len(), 24, "Timestamp filename should be 24 chars: {}", filename);
    }

    #[test]
    fn test_diff_symmetric_property() {
        // diff(A, B) added == diff(B, A) removed, and vice versa
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "A"));
        g1.add_node(Node::new("shared", "Shared"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("b", "B"));
        g2.add_node(Node::new("shared", "Shared"));

        let forward = HistoryManager::diff(&g1, &g2);
        let backward = HistoryManager::diff(&g2, &g1);

        assert_eq!(forward.added_nodes, backward.removed_nodes);
        assert_eq!(forward.removed_nodes, backward.added_nodes);
        assert_eq!(forward.added_edges, backward.removed_edges);
        assert_eq!(forward.removed_edges, backward.added_edges);
    }

    #[test]
    fn test_diff_self_is_empty() {
        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        graph.add_edge(Edge::depends_on("a", "b"));

        let diff = HistoryManager::diff(&graph, &graph);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_save_and_diff_workflow() {
        // End-to-end: save two versions, diff them
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut v1 = Graph::new();
        v1.add_node(Node::new("a", "Alpha").with_status(NodeStatus::Todo));
        v1.add_node(Node::new("b", "Beta"));
        v1.add_edge(Edge::depends_on("b", "a"));
        let f1 = mgr.save_snapshot(&v1, Some("v1")).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(1100));

        let mut v2 = Graph::new();
        v2.add_node(Node::new("a", "Alpha").with_status(NodeStatus::Done));
        v2.add_node(Node::new("c", "Charlie"));
        v2.add_edge(Edge::depends_on("c", "a"));
        let f2 = mgr.save_snapshot(&v2, Some("v2")).unwrap();

        let loaded_v1 = mgr.load_version(&f1).unwrap();
        let loaded_v2 = mgr.load_version(&f2).unwrap();
        let diff = HistoryManager::diff(&loaded_v1, &loaded_v2);

        assert!(diff.added_nodes.contains(&"c".to_string()));
        assert!(diff.removed_nodes.contains(&"b".to_string()));
        assert!(diff.modified_nodes.contains(&"a".to_string()));
        assert_eq!(diff.added_edges, 1); // c→a
        assert_eq!(diff.removed_edges, 1); // b→a
    }

    #[test]
    fn test_restore_then_diff_shows_empty() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);
        let graph_path = gid_dir.join("graph.yml");

        let mut original = Graph::new();
        original.add_node(Node::new("a", "Alpha"));
        original.add_node(Node::new("b", "Beta"));
        original.add_edge(Edge::depends_on("b", "a"));
        let f = mgr.save_snapshot(&original, Some("original")).unwrap();

        // Ensure different timestamp for the auto-snapshot in restore
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Write different current graph
        save_graph(&Graph::new(), &graph_path).unwrap();

        // Restore and verify
        mgr.restore(&f, &gid_dir, Some(StorageBackend::Yaml)).unwrap();
        let restored = load_graph(&graph_path).unwrap();

        // Compare against original directly (not re-loading snapshot, which may have been
        // overwritten by the auto-snapshot if timestamps collide)
        let diff = HistoryManager::diff(&original, &restored);
        assert!(diff.is_empty(), "Restored graph should match original: added={:?} removed={:?} modified={:?}",
            diff.added_nodes, diff.removed_nodes, diff.modified_nodes);
    }

    #[test]
    fn test_graph_diff_is_empty_helper() {
        let diff = GraphDiff {
            added_nodes: vec![],
            removed_nodes: vec![],
            modified_nodes: vec![],
            added_edges: 0,
            removed_edges: 0,
        };
        assert!(diff.is_empty());

        let diff2 = GraphDiff {
            added_nodes: vec!["a".to_string()],
            removed_nodes: vec![],
            modified_nodes: vec![],
            added_edges: 0,
            removed_edges: 0,
        };
        assert!(!diff2.is_empty());
    }

    #[test]
    fn test_diff_only_edges_changed() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "A"));
        g1.add_node(Node::new("b", "B"));
        g1.add_edge(Edge::depends_on("b", "a"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "A"));
        g2.add_node(Node::new("b", "B"));
        g2.add_edge(Edge::new("a", "b", "blocks"));

        let diff = HistoryManager::diff(&g1, &g2);
        assert!(diff.added_nodes.is_empty());
        assert!(diff.removed_nodes.is_empty());
        assert!(diff.modified_nodes.is_empty());
        assert_eq!(diff.added_edges, 1);
        assert_eq!(diff.removed_edges, 1);
    }

    #[test]
    fn test_multiple_edges_between_same_nodes() {
        let mut g1 = Graph::new();
        g1.add_node(Node::new("a", "A"));
        g1.add_node(Node::new("b", "B"));
        g1.add_edge(Edge::new("a", "b", "depends_on"));
        g1.add_edge(Edge::new("a", "b", "relates_to"));

        let mut g2 = Graph::new();
        g2.add_node(Node::new("a", "A"));
        g2.add_node(Node::new("b", "B"));
        g2.add_edge(Edge::new("a", "b", "depends_on"));
        g2.add_edge(Edge::new("a", "b", "blocks"));

        let diff = HistoryManager::diff(&g1, &g2);
        // relates_to removed, blocks added
        assert_eq!(diff.added_edges, 1);
        assert_eq!(diff.removed_edges, 1);
    }

    #[test]
    fn test_snapshot_with_special_chars_in_message() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        // Message with YAML-special characters
        let _filename = mgr.save_snapshot(&graph, Some("fix: issue #42 — 'quoted' & \"double\"")).unwrap();

        let entries = mgr.list_snapshots().unwrap();
        assert_eq!(entries.len(), 1);
        // Message is stored as YAML comment, so it should survive read
        assert!(entries[0].message.is_some());
    }

    #[test]
    fn test_restore_without_existing_graph_file() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);
        let graph_path = gid_dir.join("graph.yml");

        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "A"));
        let filename = mgr.save_snapshot(&graph, None).unwrap();

        // graph_path doesn't exist — restore should still work
        assert!(!graph_path.exists());
        mgr.restore(&filename, &gid_dir, Some(StorageBackend::Yaml)).unwrap();

        let restored = load_graph(&graph_path).unwrap();
        assert_eq!(restored.nodes.len(), 1);
        assert_eq!(restored.nodes[0].id, "a");
    }

    #[test]
    fn test_diff_against_nonexistent_version() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let graph = Graph::new();
        let result = mgr.diff_against("fake.yml", &graph);
        assert!(result.is_err());
    }

    #[test]
    fn test_history_entry_struct_fields() {
        let entry = HistoryEntry {
            filename: "2026-04-08T01-00-00Z.yml".to_string(),
            timestamp: "2026:04:08 01:00:00".to_string(),
            message: Some("test message".to_string()),
            node_count: 5,
            edge_count: 3,
            git_commit: Some("abc123".to_string()),
        };
        assert_eq!(entry.filename, "2026-04-08T01-00-00Z.yml");
        assert_eq!(entry.node_count, 5);
        assert_eq!(entry.edge_count, 3);
        assert_eq!(entry.git_commit.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_snapshot_with_knowledge_node() {
        use crate::task_graph_knowledge::{KnowledgeNode, ToolCallRecord};

        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        let mut node = Node::new("k-1", "Knowledge test");
        node.knowledge = KnowledgeNode {
            findings: std::collections::HashMap::from([
                ("f1".to_string(), "Finding 1".to_string()),
                ("f2".to_string(), "Finding 2".to_string()),
            ]),
            file_cache: std::collections::HashMap::from([
                ("src/main.rs".to_string(), "fn main() {}".to_string()),
            ]),
            tool_history: vec![ToolCallRecord {
                tool_name: "read_file".to_string(),
                timestamp: "2026-04-08T00:00:00Z".to_string(),
                summary: "Read src/main.rs".to_string(),
            }],
        };
        graph.add_node(node);

        let filename = mgr.save_snapshot(&graph, None).unwrap();
        let loaded = mgr.load_version(&filename).unwrap();

        let n = &loaded.nodes[0];
        assert_eq!(n.knowledge.findings.len(), 2);
        assert_eq!(n.knowledge.findings.get("f1").unwrap(), "Finding 1");
        assert_eq!(n.knowledge.file_cache.get("src/main.rs").unwrap(), "fn main() {}");
        assert_eq!(n.knowledge.tool_history.len(), 1);
        assert_eq!(n.knowledge.tool_history[0].tool_name, "read_file");
    }

    // ── diff_versions Tests ──

    #[test]
    fn test_diff_versions_basic() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        // Snapshot A: one node
        let mut graph_a = Graph::new();
        graph_a.add_node(Node::new("a", "Alpha"));
        let file_a = mgr.save_snapshot(&graph_a, Some("v1")).unwrap();

        // Sleep to ensure different filenames
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // Snapshot B: different node
        let mut graph_b = Graph::new();
        graph_b.add_node(Node::new("a", "Alpha Changed"));
        graph_b.add_node(Node::new("b", "Beta"));
        let file_b = mgr.save_snapshot(&graph_b, Some("v2")).unwrap();

        let diff = mgr.diff_versions(&file_a, &file_b).unwrap();
        assert_eq!(diff.added_nodes, vec!["b"]);
        assert!(diff.removed_nodes.is_empty());
        assert_eq!(diff.modified_nodes, vec!["a"]);
    }

    #[test]
    fn test_diff_versions_same() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "Alpha"));
        let filename = mgr.save_snapshot(&graph, Some("v1")).unwrap();

        // Diff a version against itself should show no changes
        let diff = mgr.diff_versions(&filename, &filename).unwrap();
        assert!(diff.is_empty());
    }

    #[test]
    fn test_diff_versions_nonexistent() {
        let temp = TempDir::new().unwrap();
        let gid_dir = temp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let mgr = HistoryManager::new(&gid_dir);

        let mut graph = Graph::new();
        graph.add_node(Node::new("a", "Alpha"));
        let filename = mgr.save_snapshot(&graph, Some("v1")).unwrap();

        // Nonexistent version_a
        let result = mgr.diff_versions("nonexistent.yml", &filename);
        assert!(result.is_err());

        // Nonexistent version_b
        let result = mgr.diff_versions(&filename, "nonexistent.yml");
        assert!(result.is_err());

        // Both nonexistent
        let result = mgr.diff_versions("nope1.yml", "nope2.yml");
        assert!(result.is_err());
    }

    #[cfg(feature = "sqlite")]
    mod sqlite_backup_tests {
        use super::*;
        use rusqlite::Connection;

        fn create_test_db(path: &Path) -> Connection {
            let conn = Connection::open(path).unwrap();
            conn.execute_batch("
                CREATE TABLE nodes (id TEXT PRIMARY KEY, title TEXT, status TEXT);
                CREATE TABLE edges (from_id TEXT, to_id TEXT, relation TEXT);
                INSERT INTO nodes VALUES ('task-1', 'Auth', 'todo');
                INSERT INTO nodes VALUES ('task-2', 'Dashboard', 'done');
                INSERT INTO edges VALUES ('task-2', 'task-1', 'depends_on');
            ").unwrap();
            conn
        }

        #[test]
        fn test_sqlite_snapshot_save() {
            let tmp = tempfile::tempdir().unwrap();
            let gid_dir = tmp.path().join(".gid");
            fs::create_dir_all(&gid_dir).unwrap();
            let mgr = HistoryManager::new(&gid_dir);

            let db_path = gid_dir.join("graph.db");
            let conn = create_test_db(&db_path);

            let filename = mgr.save_snapshot_sqlite(&conn, Some("test snapshot")).unwrap();
            assert!(filename.ends_with(".db"));

            // Verify the snapshot file exists and is valid SQLite
            let snap_path = gid_dir.join("history").join(&filename);
            assert!(snap_path.exists());

            let snap_conn = Connection::open(&snap_path).unwrap();
            let count: i64 = snap_conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0)).unwrap();
            assert_eq!(count, 2);

            let edge_count: i64 = snap_conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0)).unwrap();
            assert_eq!(edge_count, 1);
        }

        #[test]
        fn test_sqlite_snapshot_integrity_verified() {
            let tmp = tempfile::tempdir().unwrap();
            let gid_dir = tmp.path().join(".gid");
            fs::create_dir_all(&gid_dir).unwrap();
            let mgr = HistoryManager::new(&gid_dir);

            let db_path = gid_dir.join("graph.db");
            let conn = create_test_db(&db_path);

            // Should succeed — integrity check passes on valid DB
            let filename = mgr.save_snapshot_sqlite(&conn, None).unwrap();
            assert!(filename.ends_with(".db"));
        }

        #[test]
        fn test_sqlite_snapshot_meta_file() {
            let tmp = tempfile::tempdir().unwrap();
            let gid_dir = tmp.path().join(".gid");
            fs::create_dir_all(&gid_dir).unwrap();
            let mgr = HistoryManager::new(&gid_dir);

            let db_path = gid_dir.join("graph.db");
            let conn = create_test_db(&db_path);

            let filename = mgr.save_snapshot_sqlite(&conn, Some("v1 release")).unwrap();

            // Check meta file exists
            let meta_path = gid_dir.join("history").join(format!("{}.meta", filename));
            assert!(meta_path.exists());

            let meta: serde_json::Value = serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
            assert_eq!(meta["message"], "v1 release");
            assert!(meta["checksum"].as_str().unwrap().starts_with("sha256:"));
        }

        #[test]
        fn test_sqlite_snapshot_no_meta_without_message() {
            let tmp = tempfile::tempdir().unwrap();
            let gid_dir = tmp.path().join(".gid");
            fs::create_dir_all(&gid_dir).unwrap();
            let mgr = HistoryManager::new(&gid_dir);

            let db_path = gid_dir.join("graph.db");
            let conn = create_test_db(&db_path);

            let filename = mgr.save_snapshot_sqlite(&conn, None).unwrap();

            // No meta file when message is None
            let meta_path = gid_dir.join("history").join(format!("{}.meta", filename));
            assert!(!meta_path.exists());
        }

        #[test]
        fn test_sqlite_snapshot_collision_handling() {
            let tmp = tempfile::tempdir().unwrap();
            let gid_dir = tmp.path().join(".gid");
            fs::create_dir_all(&gid_dir).unwrap();
            let mgr = HistoryManager::new(&gid_dir);

            let db_path = gid_dir.join("graph.db");
            let conn = create_test_db(&db_path);

            // Save two snapshots rapidly — should get different filenames
            let f1 = mgr.save_snapshot_sqlite(&conn, Some("first")).unwrap();
            let f2 = mgr.save_snapshot_sqlite(&conn, Some("second")).unwrap();
            assert_ne!(f1, f2);

            // Both should be valid
            let snap1 = Connection::open(gid_dir.join("history").join(&f1)).unwrap();
            let snap2 = Connection::open(gid_dir.join("history").join(&f2)).unwrap();
            let c1: i64 = snap1.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0)).unwrap();
            let c2: i64 = snap2.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0)).unwrap();
            assert_eq!(c1, 2);
            assert_eq!(c2, 2);
        }
    }
}
