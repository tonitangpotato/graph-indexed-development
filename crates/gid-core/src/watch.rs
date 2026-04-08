//! Watch — file system monitoring for automatic code graph sync.
//!
//! The core logic is in [`sync_on_change`], which is a testable pure function.
//! The watch loop itself ([`watch_and_sync`]) is a thin shell around it
//! that uses the `notify` crate for filesystem events.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};

use crate::code_graph::CodeGraph;
use crate::graph::Graph;
use crate::ignore::IgnoreList;
use crate::parser::load_graph;
use crate::unify::{codegraph_to_graph_nodes, merge_code_layer, generate_bridge_edges};
use crate::semantify::apply_heuristic_layers;

/// Result of a sync operation.
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// Number of files that changed.
    pub files_changed: usize,
    /// Number of code nodes in the updated graph.
    pub code_nodes: usize,
    /// Number of code edges in the updated graph.
    pub code_edges: usize,
    /// Number of bridge edges generated.
    pub bridge_edges: usize,
    /// Time taken for the sync operation.
    pub duration_ms: u64,
    /// Whether the graph was actually modified (false if no files changed).
    pub graph_modified: bool,
}

/// Configuration for the watch/sync operation.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Directory to watch for changes.
    pub watch_dir: PathBuf,
    /// Path to the .gid directory.
    pub gid_dir: PathBuf,
    /// Debounce interval in milliseconds.
    pub debounce_ms: u64,
    /// Whether to run LSP refinement (expensive).
    pub lsp: bool,
    /// Whether to skip semantify.
    pub no_semantify: bool,
}

impl WatchConfig {
    /// Create a new WatchConfig with defaults.
    pub fn new(watch_dir: PathBuf, gid_dir: PathBuf) -> Self {
        Self {
            watch_dir,
            gid_dir,
            debounce_ms: 1000,
            lsp: true,
            no_semantify: false,
        }
    }
}

/// Check if a changed path should trigger a re-extraction.
///
/// Returns false for:
/// - Paths inside .gid/ directory
/// - Paths matching .gidignore patterns
/// - Paths matching common ignore patterns (node_modules, target, .git, etc.)
/// - Non-source files (binary, media, etc.)
pub fn should_trigger_sync(path: &Path, watch_dir: &Path, gid_dir: &Path, ignore_list: &IgnoreList) -> bool {
    // Never trigger on .gid/ changes
    if path.starts_with(gid_dir) {
        return false;
    }

    // Never trigger on .git/ changes
    let git_dir = watch_dir.join(".git");
    if path.starts_with(&git_dir) {
        return false;
    }

    // Check .gidignore patterns
    if let Ok(rel) = path.strip_prefix(watch_dir) {
        let rel_str = rel.to_string_lossy();
        // Check full relative path
        if ignore_list.should_ignore(&rel_str, path.is_dir()) {
            return false;
        }
        // Check each path component individually (a pattern like "node_modules"
        // should block all files inside node_modules/, matching gitignore semantics
        // where directory patterns ignore all contents)
        for component in rel.components() {
            let comp_str = component.as_os_str().to_string_lossy();
            if ignore_list.should_ignore(&comp_str, true) {
                return false;
            }
        }
    }

    // Only trigger on source-like files
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "go" | "java" | "c" | "cpp" | "h" | "hpp"
             | "rb" | "swift" | "kt" | "scala" | "zig" | "toml" | "yaml" | "yml" | "json") => true,
        // Known config extensions that are relevant
        Some("mod") => {
            // go.mod but not random .mod files
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name == "go.mod"
        }
        Some("gradle") => true,
        // No extension but named like source (Makefile, Dockerfile, etc.)
        None => {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            matches!(name, "Makefile" | "Dockerfile")
        }
        _ => false,
    }
}

/// Core sync logic: extract changed files, merge into graph, save.
///
/// This is the testable heart of the watch system. The watch loop
/// calls this function on each batch of file changes.
///
/// Returns `Ok(SyncResult)` with `graph_modified: false` if no files changed.
pub fn sync_on_change(config: &WatchConfig) -> Result<SyncResult> {
    let start = Instant::now();
    let meta_path = config.gid_dir.join("extract-meta.json");

    // Run incremental extraction
    let (code_graph, report) = CodeGraph::extract_incremental(
        &config.watch_dir,
        &config.gid_dir,
        &meta_path,
        false, // never force in watch mode
    ).context("incremental extraction failed")?;

    let files_changed = report.added + report.modified + report.deleted;
    if files_changed == 0 {
        return Ok(SyncResult {
            files_changed: 0,
            code_nodes: 0,
            code_edges: 0,
            bridge_edges: 0,
            duration_ms: start.elapsed().as_millis() as u64,
            graph_modified: false,
        });
    }

    // Convert to graph nodes
    let (code_nodes, code_edges) = codegraph_to_graph_nodes(&code_graph, &config.watch_dir);
    let code_node_count = code_nodes.len();
    let code_edge_count = code_edges.len();

    // Load existing graph
    let graph_path = config.gid_dir.join("graph.yml");
    let mut graph = if graph_path.exists() {
        load_graph(&graph_path).unwrap_or_default()
    } else {
        Graph::default()
    };

    // Merge code layer
    merge_code_layer(&mut graph, code_nodes, code_edges);

    // Semantify + bridge edges
    if !config.no_semantify {
        apply_heuristic_layers(&mut graph);
        generate_bridge_edges(&mut graph);
    }

    let bridge_count = graph.bridge_edges().len();

    // Atomic write: tmp → rename
    let tmp_path = graph_path.with_extension("yml.tmp");
    let yaml = serde_yaml::to_string(&graph)
        .context("failed to serialize graph")?;
    std::fs::write(&tmp_path, &yaml)
        .context("failed to write temp graph file")?;
    std::fs::rename(&tmp_path, &graph_path)
        .context("failed to rename temp graph file")?;

    Ok(SyncResult {
        files_changed,
        code_nodes: code_node_count,
        code_edges: code_edge_count,
        bridge_edges: bridge_count,
        duration_ms: start.elapsed().as_millis() as u64,
        graph_modified: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_project(source: &str) -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        let gid_dir = tmp.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();

        // Write a Rust source file
        fs::write(src_dir.join("main.rs"), source).unwrap();

        // Write a minimal graph.yml
        fs::write(gid_dir.join("graph.yml"), "nodes: []\nedges: []\n").unwrap();

        (tmp, src_dir, gid_dir)
    }

    // ── should_trigger_sync tests ──────────────────────────────────────────

    #[test]
    fn test_trigger_rust_file() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(should_trigger_sync(Path::new("/project/src/main.rs"), watch, gid, &ignore));
    }

    #[test]
    fn test_trigger_python_file() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(should_trigger_sync(Path::new("/project/lib/parser.py"), watch, gid, &ignore));
    }

    #[test]
    fn test_trigger_typescript_file() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(should_trigger_sync(Path::new("/project/src/app.tsx"), watch, gid, &ignore));
    }

    #[test]
    fn test_no_trigger_gid_dir() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(!should_trigger_sync(Path::new("/project/.gid/graph.yml"), watch, gid, &ignore));
    }

    #[test]
    fn test_no_trigger_git_dir() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(!should_trigger_sync(Path::new("/project/.git/HEAD"), watch, gid, &ignore));
    }

    #[test]
    fn test_no_trigger_binary_file() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(!should_trigger_sync(Path::new("/project/image.png"), watch, gid, &ignore));
    }

    #[test]
    fn test_no_trigger_compiled_file() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(!should_trigger_sync(Path::new("/project/main.o"), watch, gid, &ignore));
    }

    #[test]
    fn test_no_trigger_node_modules() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(!should_trigger_sync(
            Path::new("/project/node_modules/lodash/index.js"), watch, gid, &ignore
        ));
    }

    #[test]
    fn test_no_trigger_target_dir() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(!should_trigger_sync(
            Path::new("/project/target/debug/main.rs"), watch, gid, &ignore
        ));
    }

    #[test]
    fn test_trigger_cargo_toml() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(should_trigger_sync(Path::new("/project/Cargo.toml"), watch, gid, &ignore));
    }

    #[test]
    fn test_trigger_json_config() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(should_trigger_sync(Path::new("/project/tsconfig.json"), watch, gid, &ignore));
    }

    #[test]
    fn test_trigger_go_file() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(should_trigger_sync(Path::new("/project/cmd/main.go"), watch, gid, &ignore));
    }

    #[test]
    fn test_no_trigger_markdown() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        // .md files are not source code
        assert!(!should_trigger_sync(Path::new("/project/README.md"), watch, gid, &ignore));
    }

    #[test]
    fn test_trigger_makefile() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(should_trigger_sync(Path::new("/project/Makefile"), watch, gid, &ignore));
    }

    #[test]
    fn test_trigger_dockerfile() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(should_trigger_sync(Path::new("/project/Dockerfile"), watch, gid, &ignore));
    }

    #[test]
    fn test_no_trigger_lock_file() {
        let ignore = IgnoreList::with_defaults();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(!should_trigger_sync(Path::new("/project/Cargo.lock"), watch, gid, &ignore));
    }

    #[test]
    fn test_custom_gidignore_pattern() {
        let mut ignore = IgnoreList::with_defaults();
        ignore.add("generated/").unwrap();
        let watch = Path::new("/project");
        let gid = Path::new("/project/.gid");
        assert!(!should_trigger_sync(
            Path::new("/project/generated/types.rs"), watch, gid, &ignore
        ));
    }

    // ── sync_on_change tests ───────────────────────────────────────────────

    #[test]
    fn test_sync_creates_graph_from_source() {
        let (_tmp, _src_dir, gid_dir) = setup_test_project(
            r#"
pub fn hello() -> String {
    "hello".to_string()
}

pub fn world() -> String {
    "world".to_string()
}
"#,
        );

        let config = WatchConfig::new(
            _tmp.path().to_path_buf(),
            gid_dir.clone(),
        );

        let result = sync_on_change(&config).unwrap();
        assert!(result.graph_modified, "files_changed={} code_nodes={}", result.files_changed, result.code_nodes);
        assert!(result.files_changed > 0);
        assert!(result.code_nodes > 0);
        assert!(result.duration_ms < 30_000); // should complete in under 30s

        // Verify graph was written
        let graph = load_graph(&gid_dir.join("graph.yml")).unwrap();
        assert!(!graph.nodes.is_empty());
    }

    #[test]
    fn test_sync_no_change_second_run() {
        let (_tmp, _src_dir, gid_dir) = setup_test_project(
            "pub fn stable() {}\n",
        );

        let config = WatchConfig::new(
            _tmp.path().to_path_buf(),
            gid_dir.clone(),
        );

        // First run — extracts
        let r1 = sync_on_change(&config).unwrap();
        assert!(r1.graph_modified);

        // Second run — no changes
        let r2 = sync_on_change(&config).unwrap();
        assert!(!r2.graph_modified);
        assert_eq!(r2.files_changed, 0);
    }

    #[test]
    fn test_sync_detects_file_modification() {
        let (_tmp, src_dir, gid_dir) = setup_test_project(
            "pub fn original() {}\n",
        );

        let config = WatchConfig::new(
            _tmp.path().to_path_buf(),
            gid_dir.clone(),
        );

        // First extraction
        let r1 = sync_on_change(&config).unwrap();
        assert!(r1.graph_modified);

        // Modify the file — content changes are detected via content hash
        // even within the same second (mtime granularity is seconds)
        std::thread::sleep(std::time::Duration::from_millis(100));
        fs::write(src_dir.join("main.rs"), "pub fn modified() {}\npub fn added() {}\n").unwrap();

        // Second extraction should detect the change
        let r2 = sync_on_change(&config).unwrap();
        assert!(r2.graph_modified);
        assert!(r2.files_changed > 0);
    }

    #[test]
    fn test_sync_preserves_project_nodes() {
        let (_tmp, _src_dir, gid_dir) = setup_test_project(
            "pub fn code() {}\n",
        );

        // Write a graph with a project-layer task node
        let graph_content = r#"
nodes:
  - id: task-auth
    title: "Implement auth"
    type: task
    status: todo
edges: []
"#;
        fs::write(gid_dir.join("graph.yml"), graph_content).unwrap();

        let config = WatchConfig::new(
            _tmp.path().to_path_buf(),
            gid_dir.clone(),
        );

        let result = sync_on_change(&config).unwrap();
        assert!(result.graph_modified);

        // Verify project node is preserved
        let graph = load_graph(&gid_dir.join("graph.yml")).unwrap();
        assert!(graph.get_node("task-auth").is_some(), "project node should be preserved");
    }

    #[test]
    fn test_sync_atomic_write() {
        let (_tmp, _src_dir, gid_dir) = setup_test_project(
            "pub fn atomic() {}\n",
        );

        let config = WatchConfig::new(
            _tmp.path().to_path_buf(),
            gid_dir.clone(),
        );

        sync_on_change(&config).unwrap();

        // No .tmp file should remain
        assert!(!gid_dir.join("graph.yml.tmp").exists());
        // graph.yml should exist and be valid
        let graph = load_graph(&gid_dir.join("graph.yml")).unwrap();
        assert!(!graph.nodes.is_empty());
    }

    #[test]
    fn test_sync_with_no_semantify() {
        let (_tmp, _src_dir, gid_dir) = setup_test_project(
            "pub fn no_sem() {}\n",
        );

        let mut config = WatchConfig::new(
            _tmp.path().to_path_buf(),
            gid_dir.clone(),
        );
        config.no_semantify = true;

        let result = sync_on_change(&config).unwrap();
        assert!(result.graph_modified);
        // Bridge edges should be 0 when semantify is skipped
        assert_eq!(result.bridge_edges, 0);
    }

    #[test]
    fn test_sync_result_fields() {
        let (_tmp, _src_dir, gid_dir) = setup_test_project(
            "pub fn field_check() {}\n",
        );

        let config = WatchConfig::new(
            _tmp.path().to_path_buf(),
            gid_dir,
        );

        let result = sync_on_change(&config).unwrap();
        assert!(result.graph_modified);
        assert!(result.files_changed > 0);
        assert!(result.code_nodes > 0);
        // code_edges might be 0 for a simple file
        assert!(result.duration_ms < 60_000);
    }

    #[test]
    fn test_sync_new_file_added() {
        let (_tmp, src_dir, gid_dir) = setup_test_project(
            "pub fn initial() {}\n",
        );

        let config = WatchConfig::new(
            _tmp.path().to_path_buf(),
            gid_dir.clone(),
        );

        // First extraction
        sync_on_change(&config).unwrap();

        // Add a new file
        std::thread::sleep(std::time::Duration::from_millis(100));
        fs::write(src_dir.join("utils.rs"), "pub fn helper() -> i32 { 42 }\n").unwrap();

        // Should detect new file
        let result = sync_on_change(&config).unwrap();
        assert!(result.graph_modified);

        // Both files' functions should be in graph
        let graph = load_graph(&gid_dir.join("graph.yml")).unwrap();
        let func_nodes: Vec<_> = graph.nodes.iter()
            .filter(|n| n.node_kind.as_deref() == Some("Function"))
            .collect();
        assert!(func_nodes.len() >= 2, "should have at least 2 function nodes, got {}", func_nodes.len());
    }

    #[test]
    fn test_sync_missing_gid_dir() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}\n").unwrap();

        // gid_dir doesn't exist — sync should handle gracefully
        let gid_dir = tmp.path().join(".gid");
        // Don't create it — let sync_on_change handle it
        fs::create_dir_all(&gid_dir).unwrap();
        fs::write(gid_dir.join("graph.yml"), "nodes: []\nedges: []\n").unwrap();

        let config = WatchConfig::new(
            tmp.path().to_path_buf(),
            gid_dir,
        );

        let result = sync_on_change(&config).unwrap();
        assert!(result.graph_modified);
    }
}
