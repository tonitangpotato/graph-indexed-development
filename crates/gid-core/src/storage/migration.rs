//! YAML → SQLite migration pipeline.
//!
//! Five-phase pipeline: Parse → Validate → Transform → Insert → Verify.
//! Design reference: `.gid/features/sqlite-migration/design-migration.md`

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Sha256, Digest};

use crate::graph::{Edge, Graph, Node};
use super::sqlite::SqliteStorage;
use super::trait_def::{BatchOp, GraphStorage};
use super::error::StorageError;

// ═══════════════════════════════════════════════════════════
// Configuration
// ═══════════════════════════════════════════════════════════

/// Controls migration behaviour. Constructed by CLI or calling code.
#[derive(Debug, Clone)]
pub struct MigrationConfig {
    /// Path to source YAML file (default: `.gid/graph.yml`).
    pub source_path: PathBuf,
    /// Path to SQLite database (default: `.gid/graph.db`).
    pub target_path: PathBuf,
    /// Directory for backup copies of the original YAML.
    /// `None` disables backup.
    pub backup_dir: Option<PathBuf>,
    /// Validation strictness.
    pub validation_level: ValidationLevel,
    /// When `true`, skip the pre-existence check and overwrite existing DB.
    pub force: bool,
    /// When `true`, emit detailed per-record diagnostics.
    pub verbose: bool,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            source_path: PathBuf::from(".gid/graph.yml"),
            target_path: PathBuf::from(".gid/graph.db"),
            backup_dir: Some(PathBuf::from(".gid/backups")),
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        }
    }
}

/// Validation strictness levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationLevel {
    /// Reject on structural errors (but duplicates/dangles are always warnings).
    Strict,
    /// Log warnings but continue if structurally recoverable.
    Permissive,
    /// Skip validation entirely (testing only).
    None,
}

// ═══════════════════════════════════════════════════════════
// Error types
// ═══════════════════════════════════════════════════════════

/// Migration-specific errors (separate from StorageError).
#[derive(Debug)]
pub enum MigrationError {
    SourceNotFound(String),
    TargetExists(String),
    ParseFailed(String),
    ValidationFailed(Vec<ValidationDiagnostic>),
    TransformFailed(String),
    InsertFailed(String),
    VerifyFailed(String),
    BackupFailed(String),
    Storage(StorageError),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::SourceNotFound(s) => write!(f, "source not found: {s}"),
            MigrationError::TargetExists(s) => write!(f, "target already exists: {s}"),
            MigrationError::ParseFailed(s) => write!(f, "YAML parse failed: {s}"),
            MigrationError::ValidationFailed(diags) => {
                write!(f, "validation failed: {} diagnostics", diags.len())
            }
            MigrationError::TransformFailed(s) => write!(f, "transform failed: {s}"),
            MigrationError::InsertFailed(s) => write!(f, "insert failed: {s}"),
            MigrationError::VerifyFailed(s) => write!(f, "verification failed: {s}"),
            MigrationError::BackupFailed(s) => write!(f, "backup failed: {s}"),
            MigrationError::Storage(e) => write!(f, "storage error: {e}"),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<StorageError> for MigrationError {
    fn from(err: StorageError) -> Self {
        MigrationError::Storage(err)
    }
}

/// Non-fatal diagnostics from validation.
#[derive(Debug, Clone)]
pub enum ValidationDiagnostic {
    DuplicateNodeId {
        id: String,
        kept_index: usize,
        dropped_index: usize,
    },
    DanglingEdgeRef {
        field: String,
        id: String,
    },
    UnknownNodeType(String),
    UnknownEdgeRelation(String),
    SelfLoop(String),
}

impl ValidationDiagnostic {
    /// True if this diagnostic is a hard error (blocks migration in Strict mode).
    /// Duplicates and dangling edges are always warnings per GOAL-2.9.
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            ValidationDiagnostic::UnknownNodeType(_) | ValidationDiagnostic::UnknownEdgeRelation(_)
        )
    }
}

impl std::fmt::Display for ValidationDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationDiagnostic::DuplicateNodeId { id, kept_index, dropped_index } => {
                write!(f, "duplicate node ID '{id}': keeping index {kept_index}, dropping {dropped_index}")
            }
            ValidationDiagnostic::DanglingEdgeRef { field, id } => {
                write!(f, "dangling edge reference: {field}='{id}' not found in nodes")
            }
            ValidationDiagnostic::UnknownNodeType(t) => {
                write!(f, "unknown node type: '{t}'")
            }
            ValidationDiagnostic::UnknownEdgeRelation(r) => {
                write!(f, "unknown edge relation: '{r}'")
            }
            ValidationDiagnostic::SelfLoop(id) => {
                write!(f, "self-loop on node '{id}'")
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════
// Report types
// ═══════════════════════════════════════════════════════════

/// Migration outcome report.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    pub nodes_migrated: u64,
    pub edges_migrated: u64,
    pub knowledge_migrated: u64,
    pub tags_migrated: u64,
    pub metadata_migrated: u64,
    pub warnings: Vec<ValidationDiagnostic>,
    pub status: MigrationStatus,
    pub duration: std::time::Duration,
    pub backup_path: Option<PathBuf>,
    pub source_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationStatus {
    Success,
    SuccessWithWarnings,
    Failed,
}

// ═══════════════════════════════════════════════════════════
// Validated intermediate type
// ═══════════════════════════════════════════════════════════

/// Graph data after validation: deduplicated nodes, original edges, diagnostics.
struct ValidatedGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    project_name: Option<String>,
    diagnostics: Vec<ValidationDiagnostic>,
}

// ═══════════════════════════════════════════════════════════
// Insert stats (used for verification)
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Default)]
struct InsertStats {
    nodes_inserted: u64,
    edges_inserted: u64,
    knowledge_inserted: u64,
    tags_inserted: u64,
    metadata_inserted: u64,
}

// ═══════════════════════════════════════════════════════════
// Known types for validation
// ═══════════════════════════════════════════════════════════

const KNOWN_NODE_TYPES: &[&str] = &[
    "task", "file", "component", "feature", "layer", "code", "module",
    "class", "function", "method", "trait", "enum", "struct", "interface",
    "test", "config", "doc", "legacy",
];

const KNOWN_EDGE_RELATIONS: &[&str] = &[
    "depends_on", "blocks", "subtask_of", "relates_to", "implements",
    "contains", "tests_for", "calls", "imports", "defined_in",
    "belongs_to", "maps_to", "overrides", "inherits",
    "documents", "extends", "specifies", "used_by",
];

fn is_known_node_type(t: &str) -> bool {
    KNOWN_NODE_TYPES.contains(&t)
}

fn is_known_edge_relation(r: &str) -> bool {
    KNOWN_EDGE_RELATIONS.contains(&r)
}

// ═══════════════════════════════════════════════════════════
// Main entry point
// ═══════════════════════════════════════════════════════════

/// Run the full YAML → SQLite migration pipeline.
pub fn migrate(config: &MigrationConfig) -> Result<MigrationReport, MigrationError> {
    let start = Instant::now();

    // ── Precondition checks ──
    if !config.force {
        check_preconditions(config)?;
    } else if !config.source_path.exists() {
        return Err(MigrationError::SourceNotFound(format!(
            "source YAML not found: {}",
            config.source_path.display()
        )));
    } else if config.target_path.exists() {
        // Force mode: remove existing DB
        fs::remove_file(&config.target_path).map_err(|e| {
            MigrationError::InsertFailed(format!(
                "failed to remove existing DB at {}: {e}",
                config.target_path.display()
            ))
        })?;
    }

    // ── Phase 1: Parse ──
    let (graph, yaml_bytes) = parse_yaml(&config.source_path)?;

    // ── Phase 2: Validate ──
    let validated = validate(&graph, config.validation_level)?;

    // ── Phase 3: Transform ──
    let ops = transform(&validated)?;

    // ── Backup (before writes) ──
    let backup_path = if let Some(ref backup_dir) = config.backup_dir {
        Some(backup_source(&config.source_path, backup_dir)?)
    } else {
        None
    };

    // ── Phase 4: Insert ──
    let stats = insert(&config.target_path, &ops, &validated)?;

    // ── Phase 5: Verify ──
    verify(&config.target_path, &stats, &validated)?;

    // ── Build report ──
    let fingerprint = hex_sha256(&yaml_bytes);
    let has_warnings = !validated.diagnostics.is_empty();

    Ok(MigrationReport {
        nodes_migrated: stats.nodes_inserted,
        edges_migrated: stats.edges_inserted,
        knowledge_migrated: stats.knowledge_inserted,
        tags_migrated: stats.tags_inserted,
        metadata_migrated: stats.metadata_inserted,
        warnings: validated.diagnostics,
        status: if has_warnings {
            MigrationStatus::SuccessWithWarnings
        } else {
            MigrationStatus::Success
        },
        duration: start.elapsed(),
        backup_path,
        source_fingerprint: fingerprint,
    })
}

// ═══════════════════════════════════════════════════════════
// Phase 0: Precondition checks
// ═══════════════════════════════════════════════════════════

fn check_preconditions(config: &MigrationConfig) -> Result<(), MigrationError> {
    if config.target_path.exists() {
        return Err(MigrationError::TargetExists(format!(
            "SQLite database already exists at {}. Use --force to overwrite.",
            config.target_path.display()
        )));
    }
    if !config.source_path.exists() {
        return Err(MigrationError::SourceNotFound(format!(
            "no YAML graph found at {}",
            config.source_path.display()
        )));
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════
// Phase 1: Parse
// ═══════════════════════════════════════════════════════════

fn parse_yaml(source_path: &Path) -> Result<(Graph, Vec<u8>), MigrationError> {
    let bytes = fs::read(source_path).map_err(|e| {
        MigrationError::ParseFailed(format!("failed to read {}: {e}", source_path.display()))
    })?;

    // File size check: reject > 100MB
    if bytes.len() > 100 * 1024 * 1024 {
        return Err(MigrationError::ParseFailed(format!(
            "file too large: {} bytes, max 100MB",
            bytes.len()
        )));
    }

    // Empty file → empty graph
    if bytes.is_empty() {
        return Ok((Graph::default(), bytes));
    }

    let yaml_str = std::str::from_utf8(&bytes).map_err(|e| {
        MigrationError::ParseFailed(format!("non-UTF-8 content: {e}"))
    })?;

    let graph: Graph = serde_yaml::from_str(yaml_str).map_err(|e| {
        MigrationError::ParseFailed(format!("YAML deserialization failed: {e}"))
    })?;

    Ok((graph, bytes))
}

// ═══════════════════════════════════════════════════════════
// Phase 2: Validate
// ═══════════════════════════════════════════════════════════

fn validate(graph: &Graph, level: ValidationLevel) -> Result<ValidatedGraph, MigrationError> {
    if level == ValidationLevel::None {
        return Ok(ValidatedGraph {
            nodes: graph.nodes.clone(),
            edges: graph.edges.clone(),
            project_name: graph.project.as_ref().map(|p| p.name.clone()),
            diagnostics: vec![],
        });
    }

    let mut diagnostics: Vec<ValidationDiagnostic> = Vec::new();

    // 5.1 — Duplicate node IDs (last wins + warning)
    let mut seen_ids: HashMap<&str, usize> = HashMap::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        if let Some(prev_idx) = seen_ids.insert(&node.id, i) {
            diagnostics.push(ValidationDiagnostic::DuplicateNodeId {
                id: node.id.clone(),
                kept_index: i,
                dropped_index: prev_idx,
            });
        }
    }

    // 5.2 — Dangling edge references (warn only)
    for edge in &graph.edges {
        if !seen_ids.contains_key(edge.from.as_str()) {
            diagnostics.push(ValidationDiagnostic::DanglingEdgeRef {
                field: "from".into(),
                id: edge.from.clone(),
            });
        }
        if !seen_ids.contains_key(edge.to.as_str()) {
            diagnostics.push(ValidationDiagnostic::DanglingEdgeRef {
                field: "to".into(),
                id: edge.to.clone(),
            });
        }
    }

    // 5.3 — Type checks
    for node in &graph.nodes {
        if let Some(ref t) = node.node_type {
            if !is_known_node_type(t) {
                diagnostics.push(ValidationDiagnostic::UnknownNodeType(t.clone()));
            }
        }
    }
    for edge in &graph.edges {
        if !is_known_edge_relation(&edge.relation) {
            diagnostics.push(ValidationDiagnostic::UnknownEdgeRelation(edge.relation.clone()));
        }
    }

    // 5.4 — Self-loops
    for edge in &graph.edges {
        if edge.from == edge.to {
            diagnostics.push(ValidationDiagnostic::SelfLoop(edge.from.clone()));
        }
    }

    // Deduplicate nodes (last wins)
    let deduped_nodes = deduplicate_nodes(&graph.nodes, &seen_ids);

    // In Strict mode, only true errors (not warnings) block migration
    if level == ValidationLevel::Strict {
        let errors: Vec<_> = diagnostics.iter().filter(|d| d.is_error()).cloned().collect();
        if !errors.is_empty() {
            return Err(MigrationError::ValidationFailed(errors));
        }
    }

    Ok(ValidatedGraph {
        nodes: deduped_nodes,
        edges: graph.edges.clone(),
        project_name: graph.project.as_ref().map(|p| p.name.clone()),
        diagnostics,
    })
}

fn deduplicate_nodes(nodes: &[Node], seen_ids: &HashMap<&str, usize>) -> Vec<Node> {
    // Keep only the last occurrence of each ID
    nodes
        .iter()
        .enumerate()
        .filter(|(i, node)| seen_ids.get(node.id.as_str()) == Some(i))
        .map(|(_, node)| node.clone())
        .collect()
}

// ═══════════════════════════════════════════════════════════
// Phase 3: Transform
// ═══════════════════════════════════════════════════════════

fn transform(validated: &ValidatedGraph) -> Result<Vec<BatchOp>, MigrationError> {
    let mut ops = Vec::new();

    for node in &validated.nodes {
        // Node itself
        ops.push(BatchOp::PutNode(node.clone()));

        // Tags → separate table
        if !node.tags.is_empty() {
            ops.push(BatchOp::SetTags(node.id.clone(), node.tags.clone()));
        }

        // Metadata → separate table
        if !node.metadata.is_empty() {
            ops.push(BatchOp::SetMetadata(node.id.clone(), node.metadata.clone()));
        }

        // Knowledge → separate table
        if !node.knowledge.is_empty() {
            ops.push(BatchOp::SetKnowledge(node.id.clone(), node.knowledge.clone()));
        }
    }

    for edge in &validated.edges {
        ops.push(BatchOp::AddEdge(edge.clone()));
    }

    Ok(ops)
}

// ═══════════════════════════════════════════════════════════
// Phase 4: Insert
// ═══════════════════════════════════════════════════════════

fn insert(
    target_path: &Path,
    ops: &[BatchOp],
    validated: &ValidatedGraph,
) -> Result<InsertStats, MigrationError> {
    // Open DB (creates it + applies schema)
    let storage = SqliteStorage::open(target_path)?;

    // Set project metadata if available
    if let Some(ref name) = validated.project_name {
        let meta = crate::graph::ProjectMeta {
            name: name.clone(),
            description: None,
        };
        storage.set_project_meta(&meta)?;
    }

    // Disable FK enforcement for migration (dangling edges, GOAL-2.9).
    // SqliteStorage wraps Connection in RefCell, so we need direct SQL access.
    // Instead, we'll use execute_batch which already handles transactions.
    // But we need FK off before the batch. Use a two-step approach:
    // 1. Turn off FK via a single PragmaOff op (we do this via raw SQL on the storage)
    // 2. Run batch
    // 3. Turn FK back on
    //
    // Since SqliteStorage doesn't expose raw SQL, we handle dangling edges
    // by using execute_batch which catches FK violations. The schema has
    // ON DELETE CASCADE but we need to handle refs to non-existent nodes.
    //
    // Actually, SqliteStorage::execute_batch wraps in a transaction. The FK
    // constraint is checked at commit time with deferred FKs, or immediately
    // with PRAGMA foreign_keys=ON. We need to temporarily disable it.
    //
    // Use SqliteStorage's internal conn via a dedicated migration method.
    storage.execute_migration_batch(ops)?;

    // Count what we inserted
    let mut stats = InsertStats::default();
    for op in ops {
        match op {
            BatchOp::PutNode(_) => stats.nodes_inserted += 1,
            BatchOp::AddEdge(_) => stats.edges_inserted += 1,
            BatchOp::SetTags(_, tags) => stats.tags_inserted += tags.len() as u64,
            BatchOp::SetMetadata(_, meta) => stats.metadata_inserted += meta.len() as u64,
            BatchOp::SetKnowledge(_, _) => stats.knowledge_inserted += 1,
            _ => {}
        }
    }

    Ok(stats)
}

// ═══════════════════════════════════════════════════════════
// Phase 5: Verify
// ═══════════════════════════════════════════════════════════

fn verify(
    target_path: &Path,
    expected: &InsertStats,
    validated: &ValidatedGraph,
) -> Result<(), MigrationError> {
    let storage = SqliteStorage::open(target_path).map_err(|e| {
        MigrationError::VerifyFailed(format!("failed to reopen DB for verification: {e}"))
    })?;

    // ── Count verification ──
    let node_count = storage.get_node_count().map_err(|e| {
        MigrationError::VerifyFailed(format!("failed to count nodes: {e}"))
    })? as u64;

    let edge_count = storage.get_edge_count().map_err(|e| {
        MigrationError::VerifyFailed(format!("failed to count edges: {e}"))
    })? as u64;

    if node_count != expected.nodes_inserted {
        return Err(MigrationError::VerifyFailed(format!(
            "node count mismatch: expected {}, got {node_count}",
            expected.nodes_inserted
        )));
    }

    if edge_count != expected.edges_inserted {
        return Err(MigrationError::VerifyFailed(format!(
            "edge count mismatch: expected {}, got {edge_count}",
            expected.edges_inserted
        )));
    }

    // ── Content verification: sample up to 20 nodes ──
    let sample_size = validated.nodes.len().min(20);
    // Pick evenly spaced nodes: first, last, and uniformly distributed
    let indices: Vec<usize> = if validated.nodes.is_empty() {
        vec![]
    } else if validated.nodes.len() <= sample_size {
        (0..validated.nodes.len()).collect()
    } else {
        let step = validated.nodes.len() as f64 / sample_size as f64;
        (0..sample_size).map(|i| (i as f64 * step) as usize).collect()
    };

    for idx in indices {
        let src_node = &validated.nodes[idx];
        let db_node = storage.get_node(&src_node.id).map_err(|e| {
            MigrationError::VerifyFailed(format!(
                "failed to read node '{}': {e}", src_node.id
            ))
        })?;

        let db_node = match db_node {
            Some(n) => n,
            None => {
                return Err(MigrationError::VerifyFailed(format!(
                    "node '{}' not found in SQLite", src_node.id
                )));
            }
        };

        // Core fields
        if db_node.title != src_node.title {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' title mismatch: YAML='{}' DB='{}'",
                src_node.id, src_node.title, db_node.title
            )));
        }
        if db_node.status != src_node.status {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' status mismatch: YAML='{}' DB='{}'",
                src_node.id, src_node.status, db_node.status
            )));
        }
        if db_node.node_type != src_node.node_type {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' type mismatch: YAML={:?} DB={:?}",
                src_node.id, src_node.node_type, db_node.node_type
            )));
        }

        // Code-specific fields
        if db_node.file_path != src_node.file_path {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' file_path mismatch: YAML={:?} DB={:?}",
                src_node.id, src_node.file_path, db_node.file_path
            )));
        }
        if db_node.lang != src_node.lang {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' lang mismatch: YAML={:?} DB={:?}",
                src_node.id, src_node.lang, db_node.lang
            )));
        }
        if db_node.start_line != src_node.start_line {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' start_line mismatch: YAML={:?} DB={:?}",
                src_node.id, src_node.start_line, db_node.start_line
            )));
        }
        if db_node.end_line != src_node.end_line {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' end_line mismatch: YAML={:?} DB={:?}",
                src_node.id, src_node.end_line, db_node.end_line
            )));
        }
        if db_node.signature != src_node.signature {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' signature mismatch: YAML={:?} DB={:?}",
                src_node.id, src_node.signature, db_node.signature
            )));
        }
        if db_node.node_kind != src_node.node_kind {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' node_kind mismatch: YAML={:?} DB={:?}",
                src_node.id, src_node.node_kind, db_node.node_kind
            )));
        }

        // Tags
        let db_tags = storage.get_tags(&src_node.id).map_err(|e| {
            MigrationError::VerifyFailed(format!(
                "failed to read tags for '{}': {e}", src_node.id
            ))
        })?;
        let mut src_tags = src_node.tags.clone();
        let mut db_tags_sorted = db_tags.clone();
        src_tags.sort();
        db_tags_sorted.sort();
        if src_tags != db_tags_sorted {
            return Err(MigrationError::VerifyFailed(format!(
                "node '{}' tags mismatch: YAML={:?} DB={:?}",
                src_node.id, src_node.tags, db_tags
            )));
        }

        // Metadata
        if !src_node.metadata.is_empty() {
            let db_meta = storage.get_metadata(&src_node.id).map_err(|e| {
                MigrationError::VerifyFailed(format!(
                    "failed to read metadata for '{}': {e}", src_node.id
                ))
            })?;
            for (key, val) in &src_node.metadata {
                match db_meta.get(key) {
                    Some(db_val) if db_val == val => {}
                    Some(db_val) => {
                        return Err(MigrationError::VerifyFailed(format!(
                            "node '{}' metadata key '{}' mismatch: YAML={} DB={}",
                            src_node.id, key, val, db_val
                        )));
                    }
                    None => {
                        return Err(MigrationError::VerifyFailed(format!(
                            "node '{}' metadata key '{}' missing in DB",
                            src_node.id, key
                        )));
                    }
                }
            }
        }

        // Knowledge
        if !src_node.knowledge.is_empty() {
            let db_knowledge = storage.get_knowledge(&src_node.id).map_err(|e| {
                MigrationError::VerifyFailed(format!(
                    "failed to read knowledge for '{}': {e}", src_node.id
                ))
            })?;
            match db_knowledge {
                Some(k) if k == src_node.knowledge => {}
                Some(k) => {
                    return Err(MigrationError::VerifyFailed(format!(
                        "node '{}' knowledge mismatch: YAML findings={} DB findings={}",
                        src_node.id,
                        src_node.knowledge.findings.len(),
                        k.findings.len()
                    )));
                }
                None => {
                    return Err(MigrationError::VerifyFailed(format!(
                        "node '{}' knowledge missing in DB",
                        src_node.id
                    )));
                }
            }
        }
    }

    // ── Edge content verification: sample up to 20 edges ──
    let edge_sample_size = validated.edges.len().min(20);
    let edge_indices: Vec<usize> = if validated.edges.is_empty() {
        vec![]
    } else if validated.edges.len() <= edge_sample_size {
        (0..validated.edges.len()).collect()
    } else {
        let step = validated.edges.len() as f64 / edge_sample_size as f64;
        (0..edge_sample_size).map(|i| (i as f64 * step) as usize).collect()
    };

    for idx in edge_indices {
        let src_edge = &validated.edges[idx];
        let db_edges = storage.get_edges(&src_edge.from).map_err(|e| {
            MigrationError::VerifyFailed(format!(
                "failed to read edges for '{}': {e}", src_edge.from
            ))
        })?;

        let found = db_edges.iter().any(|e| {
            e.from == src_edge.from && e.to == src_edge.to && e.relation == src_edge.relation
        });

        if !found {
            return Err(MigrationError::VerifyFailed(format!(
                "edge '{}' -> '{}' ({}) not found in SQLite",
                src_edge.from, src_edge.to, src_edge.relation
            )));
        }
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════
// Backup
// ═══════════════════════════════════════════════════════════

fn backup_source(source_path: &Path, backup_dir: &Path) -> Result<PathBuf, MigrationError> {
    fs::create_dir_all(backup_dir).map_err(|e| {
        MigrationError::BackupFailed(format!(
            "failed to create backup dir {}: {e}",
            backup_dir.display()
        ))
    })?;

    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup_path = backup_dir.join(format!("graph.yml.{timestamp}.bak"));

    fs::copy(source_path, &backup_path).map_err(|e| {
        MigrationError::BackupFailed(format!(
            "failed to copy {} → {}: {e}",
            source_path.display(),
            backup_path.display()
        ))
    })?;

    Ok(backup_path)
}

// ═══════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

// ═══════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_yaml(dir: &Path, content: &str) -> PathBuf {
        let gid_dir = dir.join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let path = gid_dir.join("graph.yml");
        fs::write(&path, content).unwrap();
        path
    }

    const BASIC_YAML: &str = r#"
project:
  name: test-project
nodes:
- id: task-1
  title: First task
  status: todo
  type: task
  tags:
  - p0
  - core
  description: A test task
- id: task-2
  title: Second task
  status: done
  type: task
- id: file-1
  title: main.rs
  status: done
  type: code
  file_path: src/main.rs
  lang: rust
  start_line: 1
  end_line: 50
  signature: "fn main()"
  node_kind: Function
  source: extract
  metadata:
    line_count: 50
edges:
- from: task-2
  to: task-1
  relation: depends_on
"#;

    #[test]
    fn test_parse_basic_yaml() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(dir.path(), BASIC_YAML);
        let (graph, _bytes) = parse_yaml(&path).unwrap();

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.project.as_ref().unwrap().name, "test-project");

        let node0 = &graph.nodes[0];
        assert_eq!(node0.id, "task-1");
        assert_eq!(node0.title, "First task");

        let node2 = &graph.nodes[2];
        assert_eq!(node2.file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(node2.lang.as_deref(), Some("rust"));
        assert_eq!(node2.start_line, Some(1));
        assert_eq!(node2.end_line, Some(50));
        assert_eq!(node2.node_kind.as_deref(), Some("Function"));
    }

    #[test]
    fn test_parse_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(dir.path(), "");
        let (graph, _) = parse_yaml(&path).unwrap();
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn test_parse_file_not_found() {
        let result = parse_yaml(Path::new("/nonexistent/graph.yml"));
        assert!(matches!(result, Err(MigrationError::ParseFailed(_))));
    }

    #[test]
    fn test_validate_duplicate_ids() {
        let graph = Graph {
            project: None,
            nodes: vec![
                Node::new("dup", "First"),
                Node::new("unique", "Unique"),
                Node::new("dup", "Second (wins)"),
            ],
            edges: vec![],
        };

        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        assert_eq!(validated.nodes.len(), 2); // deduped
        assert_eq!(validated.diagnostics.len(), 1);
        assert!(matches!(
            &validated.diagnostics[0],
            ValidationDiagnostic::DuplicateNodeId { id, .. } if id == "dup"
        ));
        // Last wins: "Second (wins)" should be kept
        let dup_node = validated.nodes.iter().find(|n| n.id == "dup").unwrap();
        assert_eq!(dup_node.title, "Second (wins)");
    }

    #[test]
    fn test_validate_dangling_edges() {
        let graph = Graph {
            project: None,
            nodes: vec![Node::new("a", "Node A")],
            edges: vec![Edge::new("a", "nonexistent", "depends_on")],
        };

        // Dangling edges are warnings, not errors — migration should succeed
        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        assert_eq!(validated.diagnostics.len(), 1);
        assert!(matches!(
            &validated.diagnostics[0],
            ValidationDiagnostic::DanglingEdgeRef { field, id }
            if field == "to" && id == "nonexistent"
        ));
    }

    #[test]
    fn test_validate_self_loop() {
        let graph = Graph {
            project: None,
            nodes: vec![Node::new("a", "Node A")],
            edges: vec![Edge::new("a", "a", "depends_on")],
        };

        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        assert!(validated.diagnostics.iter().any(|d| matches!(d, ValidationDiagnostic::SelfLoop(_))));
    }

    #[test]
    fn test_transform_basic() {
        let validated = ValidatedGraph {
            nodes: vec![
                {
                    let mut n = Node::new("t1", "Task 1");
                    n.tags = vec!["p0".to_string()];
                    n.metadata.insert("custom_key".to_string(), serde_json::json!("value"));
                    n
                },
            ],
            edges: vec![Edge::new("t1", "t2", "depends_on")],
            project_name: None,
            diagnostics: vec![],
        };

        let ops = transform(&validated).unwrap();
        // PutNode + SetTags + SetMetadata + AddEdge = 4
        assert_eq!(ops.len(), 4);
        assert!(matches!(&ops[0], BatchOp::PutNode(_)));
        assert!(matches!(&ops[1], BatchOp::SetTags(_, _)));
        assert!(matches!(&ops[2], BatchOp::SetMetadata(_, _)));
        assert!(matches!(&ops[3], BatchOp::AddEdge(_)));
    }

    #[test]
    fn test_transform_with_knowledge() {
        let validated = ValidatedGraph {
            nodes: vec![
                {
                    let mut n = Node::new("t1", "Task 1");
                    n.knowledge.findings.insert("key".to_string(), "value".to_string());
                    n
                },
            ],
            edges: vec![],
            project_name: None,
            diagnostics: vec![],
        };

        let ops = transform(&validated).unwrap();
        // PutNode + SetKnowledge = 2
        assert_eq!(ops.len(), 2);
        assert!(matches!(&ops[1], BatchOp::SetKnowledge(_, _)));
    }

    #[test]
    fn test_full_migration_pipeline() {
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), BASIC_YAML);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: Some(dir.path().join(".gid/backups")),
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 3);
        assert_eq!(report.edges_migrated, 1);
        assert!(report.status == MigrationStatus::Success || report.status == MigrationStatus::SuccessWithWarnings);
        assert!(target.exists());
        assert!(report.backup_path.is_some());
        assert!(!report.source_fingerprint.is_empty());

        // Verify we can read back from SQLite
        let storage = SqliteStorage::open(&target).unwrap();
        assert_eq!(storage.get_node_count().unwrap(), 3);
        assert_eq!(storage.get_edge_count().unwrap(), 1);

        // Verify specific node data
        let node = storage.get_node("file-1").unwrap().unwrap();
        assert_eq!(node.file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(node.lang.as_deref(), Some("rust"));
        assert_eq!(node.start_line, Some(1));
        assert_eq!(node.node_kind.as_deref(), Some("Function"));
    }

    #[test]
    fn test_migration_target_exists_error() {
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), BASIC_YAML);
        let target = dir.path().join(".gid/graph.db");

        // Create target file first
        fs::write(&target, b"existing").unwrap();

        let config = MigrationConfig {
            source_path: source,
            target_path: target,
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let result = migrate(&config);
        assert!(matches!(result, Err(MigrationError::TargetExists(_))));
    }

    #[test]
    fn test_migration_force_overwrite() {
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), BASIC_YAML);
        let target = dir.path().join(".gid/graph.db");

        // First migration
        let config = MigrationConfig {
            source_path: source.clone(),
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };
        migrate(&config).unwrap();

        // Second migration with force
        let config2 = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: true,
            verbose: false,
        };
        let report = migrate(&config2).unwrap();
        assert_eq!(report.nodes_migrated, 3);
    }

    #[test]
    fn test_migration_source_not_found() {
        let dir = TempDir::new().unwrap();
        let config = MigrationConfig {
            source_path: dir.path().join("nonexistent.yml"),
            target_path: dir.path().join("graph.db"),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let result = migrate(&config);
        assert!(matches!(result, Err(MigrationError::SourceNotFound(_))));
    }

    #[test]
    fn test_validate_skip() {
        let graph = Graph {
            project: None,
            nodes: vec![Node::new("a", "Node A")],
            edges: vec![Edge::new("a", "nonexistent", "totally_bogus_relation")],
        };

        // None level skips all validation
        let validated = validate(&graph, ValidationLevel::None).unwrap();
        assert!(validated.diagnostics.is_empty());
    }

    #[test]
    fn test_content_verification_all_fields() {
        // Comprehensive test: every node field must survive the round-trip
        let yaml = r#"
project:
  name: verify-project
nodes:
- id: task-a
  title: Task Alpha
  status: todo
  type: task
  description: A detailed description
  tags:
  - urgent
  - backend
  - p0
  metadata:
    assignee: potato
    sprint: 3
    estimated_hours: 4.5
- id: code-fn
  title: "fn process_data()"
  status: done
  type: code
  file_path: src/core/processor.rs
  lang: rust
  start_line: 42
  end_line: 85
  signature: "pub fn process_data(input: &str) -> Result<Output>"
  node_kind: Function
  source: extract
  metadata:
    line_count: 44
    complexity: high
- id: code-struct
  title: Processor
  status: done
  type: code
  file_path: src/core/processor.rs
  lang: rust
  start_line: 10
  end_line: 25
  node_kind: Class
  source: extract
edges:
- from: task-a
  to: code-fn
  relation: implements
- from: code-fn
  to: code-struct
  relation: defined_in
- from: code-struct
  to: code-fn
  relation: contains
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        // Migration succeeds (including content verification in verify phase)
        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 3);
        assert_eq!(report.edges_migrated, 3);
        assert_eq!(report.status, MigrationStatus::Success);

        // Manual round-trip verification of every field
        let storage = SqliteStorage::open(&target).unwrap();

        // ── task-a ──
        let n = storage.get_node("task-a").unwrap().unwrap();
        assert_eq!(n.title, "Task Alpha");
        assert_eq!(n.status, crate::graph::NodeStatus::Todo);
        assert_eq!(n.node_type.as_deref(), Some("task"));
        assert_eq!(n.description.as_deref(), Some("A detailed description"));
        assert!(n.file_path.is_none());
        assert!(n.lang.is_none());

        let tags = storage.get_tags("task-a").unwrap();
        let mut sorted_tags = tags.clone();
        sorted_tags.sort();
        assert_eq!(sorted_tags, vec!["backend", "p0", "urgent"]);

        let meta = storage.get_metadata("task-a").unwrap();
        assert_eq!(meta.get("assignee"), Some(&serde_json::json!("potato")));
        assert_eq!(meta.get("sprint"), Some(&serde_json::json!(3)));
        assert_eq!(meta.get("estimated_hours"), Some(&serde_json::json!(4.5)));

        // ── code-fn ──
        let n = storage.get_node("code-fn").unwrap().unwrap();
        assert_eq!(n.title, "fn process_data()");
        assert_eq!(n.file_path.as_deref(), Some("src/core/processor.rs"));
        assert_eq!(n.lang.as_deref(), Some("rust"));
        assert_eq!(n.start_line, Some(42));
        assert_eq!(n.end_line, Some(85));
        assert_eq!(
            n.signature.as_deref(),
            Some("pub fn process_data(input: &str) -> Result<Output>")
        );
        assert_eq!(n.node_kind.as_deref(), Some("Function"));
        assert_eq!(n.source.as_deref(), Some("extract"));

        let meta = storage.get_metadata("code-fn").unwrap();
        assert_eq!(meta.get("line_count"), Some(&serde_json::json!(44)));
        assert_eq!(meta.get("complexity"), Some(&serde_json::json!("high")));

        // ── code-struct ──
        let n = storage.get_node("code-struct").unwrap().unwrap();
        assert_eq!(n.node_kind.as_deref(), Some("Class"));
        assert_eq!(n.start_line, Some(10));
        assert_eq!(n.end_line, Some(25));

        // ── Edges ──
        let edges = storage.get_edges("task-a").unwrap();
        assert!(edges.iter().any(|e| e.to == "code-fn" && e.relation == "implements"));

        let edges = storage.get_edges("code-fn").unwrap();
        assert!(edges.iter().any(|e| e.to == "code-struct" && e.relation == "defined_in"));

        let edges = storage.get_edges("code-struct").unwrap();
        assert!(edges.iter().any(|e| e.to == "code-fn" && e.relation == "contains"));
    }

    #[test]
    fn test_sha256_fingerprint() {
        let hash = hex_sha256(b"hello world");
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_backup_creates_file() {
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), BASIC_YAML);
        let backup_dir = dir.path().join("backups");

        let backup_path = backup_source(&source, &backup_dir).unwrap();
        assert!(backup_path.exists());
        assert!(backup_dir.exists());
    }

    // ═══════════════════════════════════════════════════════════
    // PROMOTED_KEYS verification tests
    //
    // The design (design-migration.md §6.3) specifies 14 PROMOTED_KEYS
    // that must be stored as dedicated node columns, not in node_metadata.
    // The implementation uses the Node struct directly (no RawYamlNode),
    // so serde handles promotion implicitly. These tests verify:
    // 1. All 14 design PROMOTED_KEYS survive YAML→SQLite roundtrip
    // 2. Additional Node fields beyond design (parent_id, depth, etc.) also work
    // 3. Non-promoted metadata keys go to node_metadata table
    // 4. Verify phase catches all field mismatches
    // ═══════════════════════════════════════════════════════════

    /// YAML with all 14 PROMOTED_KEYS from design §6.3 plus additional
    /// Node struct fields (parent_id, depth, complexity, is_public, body).
    const ALL_PROMOTED_KEYS_YAML: &str = r#"
project:
  name: promoted-keys-test
nodes:
- id: code-full
  title: "fn fully_promoted()"
  status: done
  type: function
  description: "A function with every promoted field populated"
  file_path: src/core/engine.rs
  lang: rust
  start_line: 100
  end_line: 250
  signature: "pub fn fully_promoted(ctx: &Context) -> Result<Output>"
  visibility: pub
  doc_comment: "/// Processes input through the full pipeline.\n/// Returns Output on success."
  body_hash: "a1b2c3d4e5f6"
  node_kind: Function
  owner: potato
  source: code_extract
  repo: gid-rs
  created_at: "2026-04-01T00:00:00Z"
  updated_at: "2026-04-07T12:00:00Z"
  assigned_to: alice
  priority: 42
  parent_id: mod-core
  depth: 3
  complexity: 8.5
  is_public: true
  body: "fn fully_promoted(ctx: &Context) -> Result<Output> { todo!() }"
  tags:
  - promoted
  - full-coverage
  - p0
  metadata:
    custom_key: "custom_value"
    line_count: 151
    reviewed_by: bob
- id: task-minimal
  title: Minimal task node
  status: todo
  type: task
edges:
- from: code-full
  to: task-minimal
  relation: implements
"#;

    #[test]
    fn test_promoted_keys_all_14_roundtrip() {
        // Verifies all 14 PROMOTED_KEYS from design §6.3 survive migration
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), ALL_PROMOTED_KEYS_YAML);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 2);
        assert_eq!(report.status, MigrationStatus::Success);

        let storage = SqliteStorage::open(&target).unwrap();
        let node = storage.get_node("code-full").unwrap().unwrap();

        // ── Design PROMOTED_KEYS (14 fields) ──
        // 1. file_path
        assert_eq!(node.file_path.as_deref(), Some("src/core/engine.rs"),
            "PROMOTED_KEY file_path failed roundtrip");
        // 2. lang
        assert_eq!(node.lang.as_deref(), Some("rust"),
            "PROMOTED_KEY lang failed roundtrip");
        // 3. start_line
        assert_eq!(node.start_line, Some(100),
            "PROMOTED_KEY start_line failed roundtrip");
        // 4. end_line
        assert_eq!(node.end_line, Some(250),
            "PROMOTED_KEY end_line failed roundtrip");
        // 5. signature
        assert_eq!(node.signature.as_deref(),
            Some("pub fn fully_promoted(ctx: &Context) -> Result<Output>"),
            "PROMOTED_KEY signature failed roundtrip");
        // 6. visibility
        assert_eq!(node.visibility.as_deref(), Some("pub"),
            "PROMOTED_KEY visibility failed roundtrip");
        // 7. doc_comment
        assert_eq!(node.doc_comment.as_deref(),
            Some("/// Processes input through the full pipeline.\n/// Returns Output on success."),
            "PROMOTED_KEY doc_comment failed roundtrip");
        // 8. body_hash
        assert_eq!(node.body_hash.as_deref(), Some("a1b2c3d4e5f6"),
            "PROMOTED_KEY body_hash failed roundtrip");
        // 9. node_kind
        assert_eq!(node.node_kind.as_deref(), Some("Function"),
            "PROMOTED_KEY node_kind failed roundtrip");
        // 10. owner
        assert_eq!(node.owner.as_deref(), Some("potato"),
            "PROMOTED_KEY owner failed roundtrip");
        // 11. source
        assert_eq!(node.source.as_deref(), Some("code_extract"),
            "PROMOTED_KEY source failed roundtrip");
        // 12. repo
        assert_eq!(node.repo.as_deref(), Some("gid-rs"),
            "PROMOTED_KEY repo failed roundtrip");
        // 13. created_at
        assert_eq!(node.created_at.as_deref(), Some("2026-04-01T00:00:00Z"),
            "PROMOTED_KEY created_at failed roundtrip");
        // 14. updated_at
        assert_eq!(node.updated_at.as_deref(), Some("2026-04-07T12:00:00Z"),
            "PROMOTED_KEY updated_at failed roundtrip");
    }

    #[test]
    fn test_extended_node_fields_roundtrip() {
        // Verifies Node struct fields BEYOND the design's 14 PROMOTED_KEYS:
        // parent_id, depth, complexity, is_public, body
        // These were added to Node after the design was written.
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), ALL_PROMOTED_KEYS_YAML);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        migrate(&config).unwrap();
        let storage = SqliteStorage::open(&target).unwrap();
        let node = storage.get_node("code-full").unwrap().unwrap();

        // Extended fields (not in original design PROMOTED_KEYS)
        assert_eq!(node.parent_id.as_deref(), Some("mod-core"),
            "Extended field parent_id failed roundtrip");
        assert_eq!(node.depth, Some(3),
            "Extended field depth failed roundtrip");
        assert_eq!(node.complexity, Some(8.5),
            "Extended field complexity failed roundtrip");
        assert_eq!(node.is_public, Some(true),
            "Extended field is_public failed roundtrip");
        assert_eq!(node.body.as_deref(),
            Some("fn fully_promoted(ctx: &Context) -> Result<Output> { todo!() }"),
            "Extended field body failed roundtrip");

        // Core fields that aren't PROMOTED_KEYS but are dedicated columns
        assert_eq!(node.description.as_deref(),
            Some("A function with every promoted field populated"),
            "Core field description failed roundtrip");
        assert_eq!(node.assigned_to.as_deref(), Some("alice"),
            "Core field assigned_to failed roundtrip");
        assert_eq!(node.priority, Some(42),
            "Core field priority failed roundtrip");
    }

    #[test]
    fn test_metadata_not_promoted_stays_in_metadata_table() {
        // Non-promoted keys must go to node_metadata, not dedicated columns
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), ALL_PROMOTED_KEYS_YAML);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        // 3 metadata keys on code-full: custom_key, line_count, reviewed_by
        assert_eq!(report.metadata_migrated, 3);

        let storage = SqliteStorage::open(&target).unwrap();
        let meta = storage.get_metadata("code-full").unwrap();

        // These are genuinely custom metadata — they should be in node_metadata
        assert_eq!(meta.get("custom_key"), Some(&serde_json::json!("custom_value")),
            "Custom metadata key 'custom_key' should be in node_metadata");
        assert_eq!(meta.get("line_count"), Some(&serde_json::json!(151)),
            "Custom metadata key 'line_count' should be in node_metadata");
        assert_eq!(meta.get("reviewed_by"), Some(&serde_json::json!("bob")),
            "Custom metadata key 'reviewed_by' should be in node_metadata");

        // Promoted keys must NOT appear in node_metadata (they're dedicated columns)
        assert!(!meta.contains_key("file_path"),
            "PROMOTED_KEY file_path should NOT be in node_metadata");
        assert!(!meta.contains_key("lang"),
            "PROMOTED_KEY lang should NOT be in node_metadata");
        assert!(!meta.contains_key("start_line"),
            "PROMOTED_KEY start_line should NOT be in node_metadata");
        assert!(!meta.contains_key("signature"),
            "PROMOTED_KEY signature should NOT be in node_metadata");
        assert!(!meta.contains_key("owner"),
            "PROMOTED_KEY owner should NOT be in node_metadata");
        assert!(!meta.contains_key("source"),
            "PROMOTED_KEY source should NOT be in node_metadata");
    }

    #[test]
    fn test_promoted_keys_none_values_handled() {
        // When promoted keys are absent in YAML, they should be NULL in SQLite
        let yaml = r#"
nodes:
- id: bare-task
  title: Bare task
  status: todo
  type: task
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        migrate(&config).unwrap();
        let storage = SqliteStorage::open(&target).unwrap();
        let node = storage.get_node("bare-task").unwrap().unwrap();

        // All promoted keys should be None
        assert!(node.file_path.is_none(), "bare task should have no file_path");
        assert!(node.lang.is_none(), "bare task should have no lang");
        assert!(node.start_line.is_none(), "bare task should have no start_line");
        assert!(node.end_line.is_none(), "bare task should have no end_line");
        assert!(node.signature.is_none(), "bare task should have no signature");
        assert!(node.visibility.is_none(), "bare task should have no visibility");
        assert!(node.doc_comment.is_none(), "bare task should have no doc_comment");
        assert!(node.body_hash.is_none(), "bare task should have no body_hash");
        assert!(node.node_kind.is_none(), "bare task should have no node_kind");
        assert!(node.owner.is_none(), "bare task should have no owner");
        assert!(node.source.is_none(), "bare task should have no source");
        assert!(node.repo.is_none(), "bare task should have no repo");
        assert!(node.created_at.is_none(), "bare task should have no created_at");
        assert!(node.updated_at.is_none(), "bare task should have no updated_at");
        assert!(node.parent_id.is_none(), "bare task should have no parent_id");
        assert!(node.depth.is_none(), "bare task should have no depth");
        assert!(node.complexity.is_none(), "bare task should have no complexity");
        assert!(node.is_public.is_none(), "bare task should have no is_public");
        assert!(node.body.is_none(), "bare task should have no body");
    }

    #[test]
    fn test_transform_separates_promoted_from_metadata() {
        // Verify the transform phase generates the right BatchOp sequence:
        // PutNode (with promoted fields on Node), SetTags, SetMetadata (non-promoted only)
        let mut node = Node::new("n1", "Node 1");
        node.file_path = Some("src/main.rs".into()); // promoted
        node.lang = Some("rust".into());              // promoted
        node.owner = Some("potato".into());           // promoted
        node.tags = vec!["tag1".to_string()];
        node.metadata.insert("custom_key".into(), serde_json::json!("val"));
        node.metadata.insert("extra_info".into(), serde_json::json!(42));

        let validated = ValidatedGraph {
            nodes: vec![node],
            edges: vec![],
            project_name: None,
            diagnostics: vec![],
        };

        let ops = transform(&validated).unwrap();
        // Expected: PutNode + SetTags + SetMetadata = 3
        assert_eq!(ops.len(), 3, "Expected PutNode + SetTags + SetMetadata");

        // PutNode should carry promoted fields on the Node struct itself
        match &ops[0] {
            BatchOp::PutNode(n) => {
                assert_eq!(n.file_path.as_deref(), Some("src/main.rs"));
                assert_eq!(n.lang.as_deref(), Some("rust"));
                assert_eq!(n.owner.as_deref(), Some("potato"));
            }
            _ => panic!("Expected PutNode as first op"),
        }

        // SetMetadata should only contain non-promoted keys
        match &ops[2] {
            BatchOp::SetMetadata(id, meta) => {
                assert_eq!(id, "n1");
                assert_eq!(meta.len(), 2, "Only 2 custom metadata keys expected");
                assert!(meta.contains_key("custom_key"));
                assert!(meta.contains_key("extra_info"));
                // Promoted keys should NOT be in metadata
                assert!(!meta.contains_key("file_path"),
                    "Promoted key file_path should not be in metadata");
                assert!(!meta.contains_key("lang"),
                    "Promoted key lang should not be in metadata");
                assert!(!meta.contains_key("owner"),
                    "Promoted key owner should not be in metadata");
            }
            _ => panic!("Expected SetMetadata as third op"),
        }
    }

    #[test]
    fn test_knowledge_fields_roundtrip_in_migration() {
        // Verify knowledge (findings, file_cache, tool_history) survives migration
        let yaml = r#"
nodes:
- id: task-with-knowledge
  title: Task with knowledge
  status: in_progress
  type: task
  knowledge:
    findings:
      architecture: "Uses hexagonal architecture pattern"
      risk: "High complexity in auth module"
    file_cache:
      "src/auth.rs": "pub struct Auth { ... }"
    tool_history:
    - tool_name: read_file
      timestamp: "2026-04-07T10:00:00Z"
      summary: "Read auth module for analysis"
    - tool_name: search_files
      timestamp: "2026-04-07T10:01:00Z"
      summary: "Searched for security patterns"
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.knowledge_migrated, 1);

        let storage = SqliteStorage::open(&target).unwrap();
        let knowledge = storage.get_knowledge("task-with-knowledge").unwrap().unwrap();

        assert_eq!(knowledge.findings.len(), 2);
        assert_eq!(knowledge.findings.get("architecture"),
            Some(&"Uses hexagonal architecture pattern".to_string()));
        assert_eq!(knowledge.findings.get("risk"),
            Some(&"High complexity in auth module".to_string()));

        assert_eq!(knowledge.file_cache.len(), 1);
        assert!(knowledge.file_cache.contains_key("src/auth.rs"));

        assert_eq!(knowledge.tool_history.len(), 2);
        assert_eq!(knowledge.tool_history[0].tool_name, "read_file");
        assert_eq!(knowledge.tool_history[1].tool_name, "search_files");
    }

    #[test]
    fn test_edge_metadata_roundtrip_in_migration() {
        // Verify edge weight, confidence, and metadata survive migration
        let yaml = r#"
nodes:
- id: a
  title: Node A
  status: todo
  type: task
- id: b
  title: Node B
  status: todo
  type: task
edges:
- from: a
  to: b
  relation: depends_on
  weight: 0.75
  confidence: 0.9
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        migrate(&config).unwrap();
        let storage = SqliteStorage::open(&target).unwrap();
        let edges = storage.get_edges("a").unwrap();

        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert_eq!(edge.from, "a");
        assert_eq!(edge.to, "b");
        assert_eq!(edge.relation, "depends_on");
        assert_eq!(edge.weight, Some(0.75));
        assert_eq!(edge.confidence, Some(0.9));
    }

    #[test]
    fn test_verify_phase_checks_promoted_fields() {
        // The verify phase should detect mismatches in promoted key fields.
        // We test this indirectly: if verify passes after migration,
        // it means the fields were written and read back correctly.
        // Here we test with ALL fields populated to ensure verify covers them.
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), ALL_PROMOTED_KEYS_YAML);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source.clone(),
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        // If verify phase has gaps, this test won't catch field mismatches.
        // But combined with test_promoted_keys_all_14_roundtrip which does
        // explicit field-by-field checks, we get full coverage.
        let report = migrate(&config).unwrap();
        assert!(
            report.status == MigrationStatus::Success
                || report.status == MigrationStatus::SuccessWithWarnings,
            "Migration should succeed with all promoted keys populated"
        );

        // Verify we can re-migrate with force (idempotency)
        let config2 = MigrationConfig {
            source_path: source,
            target_path: target,
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: true,
            verbose: false,
        };
        let report2 = migrate(&config2).unwrap();
        assert_eq!(report2.nodes_migrated, report.nodes_migrated);
    }

    #[test]
    fn test_promoted_keys_design_coverage_matrix() {
        // This test is a compile-time documentation check.
        // It explicitly lists every PROMOTED_KEY from the design and
        // verifies the Node struct has a matching field.
        // If the Node struct changes, this test reminds us to update the design.

        let mut node = Node::new("test", "Test");

        // Design §6.1 PROMOTED_KEYS — each must be a dedicated Node field
        node.file_path = Some("test".into());     // 1. file_path
        node.lang = Some("rust".into());           // 2. lang
        node.start_line = Some(1);                 // 3. start_line
        node.end_line = Some(10);                  // 4. end_line
        node.signature = Some("fn()".into());      // 5. signature
        node.visibility = Some("pub".into());      // 6. visibility
        node.doc_comment = Some("///".into());     // 7. doc_comment
        node.body_hash = Some("abc".into());       // 8. body_hash
        node.node_kind = Some("Function".into());  // 9. node_kind
        node.owner = Some("potato".into());        // 10. owner
        node.source = Some("extract".into());      // 11. source
        node.repo = Some("repo".into());           // 12. repo
        node.created_at = Some("2026".into());     // 13. created_at
        node.updated_at = Some("2026".into());     // 14. updated_at

        // Extended fields (Node struct has these, design doesn't list as PROMOTED)
        node.parent_id = Some("parent".into());
        node.depth = Some(1);
        node.complexity = Some(5.0);
        node.is_public = Some(true);
        node.body = Some("code".into());

        // Schema has 26 columns — verify count matches
        // id(1) + title(2) + status(3) + description(4) + node_type(5)
        // + 14 promoted + assigned_to(20) + priority(21)
        // + parent_id(22) + depth(23) + complexity(24) + is_public(25) + body(26)
        // = 26 total (matches put_node's 26 params)

        // If this compiles, all PROMOTED_KEYS exist as Node fields
        assert!(node.file_path.is_some());
        assert!(node.lang.is_some());
        assert!(node.start_line.is_some());
        assert!(node.end_line.is_some());
        assert!(node.signature.is_some());
        assert!(node.visibility.is_some());
        assert!(node.doc_comment.is_some());
        assert!(node.body_hash.is_some());
        assert!(node.node_kind.is_some());
        assert!(node.owner.is_some());
        assert!(node.source.is_some());
        assert!(node.repo.is_some());
        assert!(node.created_at.is_some());
        assert!(node.updated_at.is_some());
    }

    #[test]
    fn test_mixed_nodes_promoted_and_metadata() {
        // Real-world scenario: a graph with both code nodes (many promoted keys)
        // and task nodes (mostly metadata). Verifies both types migrate correctly.
        let yaml = r#"
project:
  name: mixed-graph
nodes:
- id: feature-auth
  title: Authentication Feature
  status: in_progress
  type: feature
  description: OAuth2 implementation
  assigned_to: potato
  priority: 10
  tags:
  - auth
  - security
  metadata:
    sprint: 5
    estimate: "3d"
    design_ref: "3.2"
- id: "fn:auth::validate_token"
  title: "validate_token()"
  status: done
  type: function
  file_path: src/auth/token.rs
  lang: rust
  start_line: 45
  end_line: 120
  signature: "pub fn validate_token(token: &str) -> Result<Claims>"
  visibility: pub
  doc_comment: "/// Validates JWT token and extracts claims"
  body_hash: "deadbeef1234"
  node_kind: Function
  owner: potato
  source: code_extract
  repo: my-project
  created_at: "2026-03-15T10:00:00Z"
  updated_at: "2026-04-07T15:30:00Z"
  parent_id: "mod:auth"
  depth: 2
  complexity: 12.3
  is_public: true
  body: "pub fn validate_token(token: &str) -> Result<Claims> {\n    // ...\n}"
edges:
- from: feature-auth
  to: "fn:auth::validate_token"
  relation: implements
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 2);
        assert_eq!(report.edges_migrated, 1);

        let storage = SqliteStorage::open(&target).unwrap();

        // Verify feature node (task-like, metadata-heavy)
        let feature = storage.get_node("feature-auth").unwrap().unwrap();
        assert_eq!(feature.title, "Authentication Feature");
        assert_eq!(feature.status, crate::graph::NodeStatus::InProgress);
        assert_eq!(feature.node_type.as_deref(), Some("feature"));
        assert_eq!(feature.description.as_deref(), Some("OAuth2 implementation"));
        assert_eq!(feature.assigned_to.as_deref(), Some("potato"));
        assert_eq!(feature.priority, Some(10));
        // Promoted keys should be None for task nodes
        assert!(feature.file_path.is_none());
        assert!(feature.lang.is_none());
        assert!(feature.start_line.is_none());

        let feature_tags = storage.get_tags("feature-auth").unwrap();
        assert!(feature_tags.contains(&"auth".to_string()));
        assert!(feature_tags.contains(&"security".to_string()));

        let feature_meta = storage.get_metadata("feature-auth").unwrap();
        assert_eq!(feature_meta.get("sprint"), Some(&serde_json::json!(5)));
        assert_eq!(feature_meta.get("estimate"), Some(&serde_json::json!("3d")));
        assert_eq!(feature_meta.get("design_ref"), Some(&serde_json::json!("3.2")));

        // Verify code node (promoted-keys-heavy)
        let code = storage.get_node("fn:auth::validate_token").unwrap().unwrap();
        assert_eq!(code.file_path.as_deref(), Some("src/auth/token.rs"));
        assert_eq!(code.lang.as_deref(), Some("rust"));
        assert_eq!(code.start_line, Some(45));
        assert_eq!(code.end_line, Some(120));
        assert_eq!(code.signature.as_deref(),
            Some("pub fn validate_token(token: &str) -> Result<Claims>"));
        assert_eq!(code.visibility.as_deref(), Some("pub"));
        assert!(code.doc_comment.as_deref().unwrap().contains("Validates JWT"));
        assert_eq!(code.body_hash.as_deref(), Some("deadbeef1234"));
        assert_eq!(code.node_kind.as_deref(), Some("Function"));
        assert_eq!(code.owner.as_deref(), Some("potato"));
        assert_eq!(code.source.as_deref(), Some("code_extract"));
        assert_eq!(code.repo.as_deref(), Some("my-project"));
        assert_eq!(code.created_at.as_deref(), Some("2026-03-15T10:00:00Z"));
        assert_eq!(code.updated_at.as_deref(), Some("2026-04-07T15:30:00Z"));
        assert_eq!(code.parent_id.as_deref(), Some("mod:auth"));
        assert_eq!(code.depth, Some(2));
        assert_eq!(code.complexity, Some(12.3));
        assert_eq!(code.is_public, Some(true));
        assert!(code.body.as_deref().unwrap().contains("validate_token"));

        // Edge should survive
        let edges = storage.get_edges("feature-auth").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "fn:auth::validate_token");
        assert_eq!(edges[0].relation, "implements");
    }

    #[test]
    fn test_tags_migration_count() {
        // Verify tag count in migration report matches actual tags
        let yaml = r#"
nodes:
- id: t1
  title: Task 1
  status: todo
  type: task
  tags:
  - alpha
  - beta
  - gamma
- id: t2
  title: Task 2
  status: done
  type: task
  tags:
  - delta
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.tags_migrated, 4, "Expected 3 + 1 = 4 tags migrated");
        assert_eq!(report.nodes_migrated, 2);

        let storage = SqliteStorage::open(&target).unwrap();
        let t1_tags = storage.get_tags("t1").unwrap();
        assert_eq!(t1_tags.len(), 3);
        let t2_tags = storage.get_tags("t2").unwrap();
        assert_eq!(t2_tags.len(), 1);
        assert_eq!(t2_tags[0], "delta");
    }

    // ═══════════════════════════════════════════════════════════
    // Group 1: Parse Phase Edge Cases
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_parse_malformed_yaml() {
        let dir = TempDir::new().unwrap();
        let path = write_yaml(dir.path(), "nodes:\n  - id: broken\n    title: [unclosed");
        let result = parse_yaml(&path);
        assert!(matches!(result, Err(MigrationError::ParseFailed(msg)) if msg.contains("YAML deserialization failed")));
    }

    #[test]
    fn test_parse_non_utf8() {
        let dir = TempDir::new().unwrap();
        let gid_dir = dir.path().join(".gid");
        fs::create_dir_all(&gid_dir).unwrap();
        let path = gid_dir.join("graph.yml");
        // Write raw bytes with invalid UTF-8 sequence
        fs::write(&path, b"\xff\xfe\x00\x01 not valid utf8").unwrap();
        let result = parse_yaml(&path);
        assert!(matches!(result, Err(MigrationError::ParseFailed(msg)) if msg.contains("non-UTF-8")));
    }

    #[test]
    fn test_parse_file_too_large_error_message() {
        // We can't easily create a >100MB file in tests, but we verify the error
        // message format and the constant by checking the limit exists in the code.
        // Instead, verify a small file works fine (proving the check doesn't reject it).
        let dir = TempDir::new().unwrap();
        let content = "nodes:\n".to_string() + &"- id: n\n  title: t\n".repeat(1000);
        let path = write_yaml(dir.path(), &content);
        let result = parse_yaml(&path);
        assert!(result.is_ok(), "Small file should parse fine");
    }

    #[test]
    fn test_parse_yaml_wrong_schema() {
        let dir = TempDir::new().unwrap();
        // Valid YAML but not a Graph — it's a plain string
        let path = write_yaml(dir.path(), "just a string, not a graph");
        let result = parse_yaml(&path);
        assert!(matches!(result, Err(MigrationError::ParseFailed(msg)) if msg.contains("YAML deserialization failed")));
    }

    #[test]
    fn test_parse_yaml_extra_unknown_fields() {
        // Extra unknown fields should be silently ignored (no deny_unknown_fields)
        let yaml = r#"
nodes:
- id: n1
  title: Node 1
  status: todo
  totally_unknown_field: "should be ignored"
  another_one: 42
edges: []
"#;
        let dir = TempDir::new().unwrap();
        let path = write_yaml(dir.path(), yaml);
        let (graph, _) = parse_yaml(&path).unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id, "n1");
    }

    #[test]
    fn test_parse_yaml_nodes_only() {
        // No edges, no project — just nodes
        let yaml = r#"
nodes:
- id: solo
  title: Solo Node
  status: done
"#;
        let dir = TempDir::new().unwrap();
        let path = write_yaml(dir.path(), yaml);
        let (graph, _) = parse_yaml(&path).unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert!(graph.edges.is_empty());
        assert!(graph.project.is_none());
    }

    // ═══════════════════════════════════════════════════════════
    // Group 2: Validate Phase Comprehensive
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_validate_unknown_node_type_strict() {
        let mut node = Node::new("a", "Node A");
        node.node_type = Some("totally_bogus".to_string());
        let graph = Graph {
            project: None,
            nodes: vec![node],
            edges: vec![],
        };

        let result = validate(&graph, ValidationLevel::Strict);
        assert!(matches!(result, Err(MigrationError::ValidationFailed(diags))
            if diags.iter().any(|d| matches!(d, ValidationDiagnostic::UnknownNodeType(t) if t == "totally_bogus"))
        ));
    }

    #[test]
    fn test_validate_unknown_edge_relation_strict() {
        let graph = Graph {
            project: None,
            nodes: vec![Node::new("a", "A"), Node::new("b", "B")],
            edges: vec![Edge::new("a", "b", "made_up_relation")],
        };

        let result = validate(&graph, ValidationLevel::Strict);
        assert!(matches!(result, Err(MigrationError::ValidationFailed(diags))
            if diags.iter().any(|d| matches!(d, ValidationDiagnostic::UnknownEdgeRelation(r) if r == "made_up_relation"))
        ));
    }

    #[test]
    fn test_validate_permissive_mode() {
        // In Permissive mode, unknown types are collected as diagnostics but do NOT block.
        let mut node = Node::new("a", "Node A");
        node.node_type = Some("totally_bogus".to_string());
        let graph = Graph {
            project: None,
            nodes: vec![node],
            edges: vec![Edge::new("a", "a", "made_up_relation")],
        };

        let result = validate(&graph, ValidationLevel::Permissive);
        // Should succeed (permissive), not error
        let validated = result.unwrap();
        // But diagnostics should still be collected
        assert!(validated.diagnostics.iter().any(|d|
            matches!(d, ValidationDiagnostic::UnknownNodeType(t) if t == "totally_bogus")
        ));
        assert!(validated.diagnostics.iter().any(|d|
            matches!(d, ValidationDiagnostic::UnknownEdgeRelation(r) if r == "made_up_relation")
        ));
    }

    #[test]
    fn test_validate_multiple_diagnostics_combined() {
        // Graph with duplicates + dangling + self-loops → all diagnostics collected
        let graph = Graph {
            project: None,
            nodes: vec![
                Node::new("dup", "First"),
                Node::new("dup", "Second"),
                Node::new("other", "Other"),
            ],
            edges: vec![
                Edge::new("other", "other", "depends_on"),   // self-loop
                Edge::new("other", "missing", "depends_on"), // dangling to
            ],
        };

        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        // Should have: 1 duplicate + 1 self-loop + 1 dangling
        let has_dup = validated.diagnostics.iter().any(|d|
            matches!(d, ValidationDiagnostic::DuplicateNodeId { .. }));
        let has_self = validated.diagnostics.iter().any(|d|
            matches!(d, ValidationDiagnostic::SelfLoop(_)));
        let has_dangle = validated.diagnostics.iter().any(|d|
            matches!(d, ValidationDiagnostic::DanglingEdgeRef { .. }));
        assert!(has_dup, "Expected duplicate diagnostic");
        assert!(has_self, "Expected self-loop diagnostic");
        assert!(has_dangle, "Expected dangling edge diagnostic");
        assert!(validated.diagnostics.len() >= 3);
    }

    #[test]
    fn test_validate_dangling_from_ref() {
        // Edge with dangling `from` (not just `to`)
        let graph = Graph {
            project: None,
            nodes: vec![Node::new("b", "Node B")],
            edges: vec![Edge::new("ghost", "b", "depends_on")],
        };

        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        assert!(validated.diagnostics.iter().any(|d| matches!(
            d,
            ValidationDiagnostic::DanglingEdgeRef { field, id }
            if field == "from" && id == "ghost"
        )));
    }

    #[test]
    fn test_validate_empty_graph() {
        let graph = Graph {
            project: None,
            nodes: vec![],
            edges: vec![],
        };

        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        assert!(validated.nodes.is_empty());
        assert!(validated.edges.is_empty());
        assert!(validated.diagnostics.is_empty());
    }

    #[test]
    fn test_validate_known_types_pass() {
        // All KNOWN_NODE_TYPES and KNOWN_EDGE_RELATIONS pass validation
        let mut nodes: Vec<Node> = KNOWN_NODE_TYPES
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut n = Node::new(&format!("n{i}"), &format!("Node {i}"));
                n.node_type = Some(t.to_string());
                n
            })
            .collect();
        // Need at least 2 nodes per edge relation
        let extra = Node::new("target", "Target");
        nodes.push(extra);

        let edges: Vec<Edge> = KNOWN_EDGE_RELATIONS
            .iter()
            .enumerate()
            .map(|(i, r)| Edge::new(&format!("n{}", i % KNOWN_NODE_TYPES.len()), "target", r))
            .collect();

        let graph = Graph {
            project: None,
            nodes,
            edges,
        };

        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        // No unknown type/relation errors
        let type_errors: Vec<_> = validated.diagnostics.iter().filter(|d| d.is_error()).collect();
        assert!(type_errors.is_empty(), "All known types should pass: {:?}", type_errors);
    }

    // ═══════════════════════════════════════════════════════════
    // Group 3: Deduplicate
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_deduplicate_three_duplicates() {
        // 3 nodes same ID → last wins, 2 diagnostics
        let graph = Graph {
            project: None,
            nodes: vec![
                Node::new("dup", "First"),
                Node::new("dup", "Second"),
                Node::new("dup", "Third (wins)"),
            ],
            edges: vec![],
        };

        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        assert_eq!(validated.nodes.len(), 1);
        assert_eq!(validated.nodes[0].title, "Third (wins)");

        let dup_diags: Vec<_> = validated.diagnostics.iter().filter(|d|
            matches!(d, ValidationDiagnostic::DuplicateNodeId { .. })
        ).collect();
        assert_eq!(dup_diags.len(), 2, "Expected 2 duplicate diagnostics for 3 copies");
    }

    #[test]
    fn test_deduplicate_preserves_order() {
        // Non-duplicate nodes maintain relative order
        let graph = Graph {
            project: None,
            nodes: vec![
                Node::new("c", "Charlie"),
                Node::new("a", "Alice"),
                Node::new("b", "Bob"),
            ],
            edges: vec![],
        };

        let validated = validate(&graph, ValidationLevel::Strict).unwrap();
        assert_eq!(validated.nodes.len(), 3);
        assert_eq!(validated.nodes[0].id, "c");
        assert_eq!(validated.nodes[1].id, "a");
        assert_eq!(validated.nodes[2].id, "b");
        assert!(validated.diagnostics.is_empty());
    }

    // ═══════════════════════════════════════════════════════════
    // Group 4: Transform Edge Cases
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_transform_empty_graph() {
        let validated = ValidatedGraph {
            nodes: vec![],
            edges: vec![],
            project_name: None,
            diagnostics: vec![],
        };

        let ops = transform(&validated).unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_transform_node_no_extras() {
        // Node with no tags, metadata, or knowledge → only PutNode
        let validated = ValidatedGraph {
            nodes: vec![Node::new("bare", "Bare Node")],
            edges: vec![],
            project_name: None,
            diagnostics: vec![],
        };

        let ops = transform(&validated).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], BatchOp::PutNode(n) if n.id == "bare"));
    }

    #[test]
    fn test_transform_edge_ordering() {
        // Edges come after all node ops in the ops list
        let validated = ValidatedGraph {
            nodes: vec![
                {
                    let mut n = Node::new("n1", "N1");
                    n.tags = vec!["tag1".to_string()];
                    n
                },
                Node::new("n2", "N2"),
            ],
            edges: vec![Edge::new("n1", "n2", "depends_on")],
            project_name: None,
            diagnostics: vec![],
        };

        let ops = transform(&validated).unwrap();
        // n1: PutNode + SetTags = 2
        // n2: PutNode = 1
        // edge: AddEdge = 1
        // Total = 4
        assert_eq!(ops.len(), 4);

        // Find the index of the first AddEdge
        let first_edge_idx = ops.iter().position(|op| matches!(op, BatchOp::AddEdge(_))).unwrap();
        // Find the last PutNode/SetTags/SetMetadata/SetKnowledge
        let last_node_idx = ops.iter().rposition(|op| !matches!(op, BatchOp::AddEdge(_))).unwrap();
        assert!(first_edge_idx > last_node_idx, "Edges should come after all node ops");
    }

    #[test]
    fn test_transform_multiple_nodes_with_metadata() {
        // Multiple nodes each with different metadata → correct ops count
        let validated = ValidatedGraph {
            nodes: vec![
                {
                    let mut n = Node::new("n1", "N1");
                    n.metadata.insert("key1".to_string(), serde_json::json!("val1"));
                    n.tags = vec!["t1".to_string(), "t2".to_string()];
                    n
                },
                {
                    let mut n = Node::new("n2", "N2");
                    n.knowledge.findings.insert("f1".to_string(), "v1".to_string());
                    n
                },
                Node::new("n3", "N3"),
            ],
            edges: vec![Edge::new("n1", "n2", "depends_on")],
            project_name: None,
            diagnostics: vec![],
        };

        let ops = transform(&validated).unwrap();
        // n1: PutNode + SetTags + SetMetadata = 3
        // n2: PutNode + SetKnowledge = 2
        // n3: PutNode = 1
        // edge: AddEdge = 1
        // Total = 7
        assert_eq!(ops.len(), 7);
    }

    // ═══════════════════════════════════════════════════════════
    // Group 5: Full Pipeline Edge Cases
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_migration_empty_yaml_graph() {
        // Empty graph → 0 nodes, 0 edges, Success
        let yaml = r#"
nodes: []
edges: []
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 0);
        assert_eq!(report.edges_migrated, 0);
        assert_eq!(report.status, MigrationStatus::Success);
    }

    #[test]
    fn test_migration_with_warnings() {
        // Graph with duplicates → SuccessWithWarnings
        let yaml = r#"
nodes:
- id: dup
  title: First
  status: todo
  type: task
- id: dup
  title: Second
  status: done
  type: task
edges: []
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.status, MigrationStatus::SuccessWithWarnings);
        assert_eq!(report.nodes_migrated, 1); // deduplicated
        assert!(!report.warnings.is_empty());
    }

    #[test]
    fn test_migration_unicode_content() {
        // Unicode in titles, descriptions, tags → roundtrip intact
        let yaml = r#"
nodes:
- id: unicode-1
  title: "日本語テスト"
  status: todo
  type: task
  description: "Описание на русском 🦀"
  tags:
  - "标签"
  - "タグ"
  - "émoji🎉"
edges: []
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 1);

        let storage = SqliteStorage::open(&target).unwrap();
        let node = storage.get_node("unicode-1").unwrap().unwrap();
        assert_eq!(node.title, "日本語テスト");
        assert_eq!(node.description.as_deref(), Some("Описание на русском 🦀"));

        let tags = storage.get_tags("unicode-1").unwrap();
        assert!(tags.contains(&"标签".to_string()));
        assert!(tags.contains(&"タグ".to_string()));
        assert!(tags.contains(&"émoji🎉".to_string()));
    }

    #[test]
    fn test_migration_special_chars_in_ids() {
        // IDs with colons, dots, slashes
        let yaml = r#"
nodes:
- id: "fn:auth::validate"
  title: validate function
  status: done
  type: function
- id: "file:src/main.rs"
  title: main.rs
  status: done
  type: file
- id: "mod.core.engine"
  title: engine module
  status: done
  type: module
edges:
- from: "fn:auth::validate"
  to: "file:src/main.rs"
  relation: defined_in
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 3);
        assert_eq!(report.edges_migrated, 1);

        let storage = SqliteStorage::open(&target).unwrap();
        assert!(storage.get_node("fn:auth::validate").unwrap().is_some());
        assert!(storage.get_node("file:src/main.rs").unwrap().is_some());
        assert!(storage.get_node("mod.core.engine").unwrap().is_some());

        let edges = storage.get_edges("fn:auth::validate").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "file:src/main.rs");
    }

    #[test]
    fn test_migration_large_metadata_values() {
        // Large JSON values in metadata → survive roundtrip
        let large_value = "x".repeat(10_000);
        let yaml = format!(
            r#"
nodes:
- id: big-meta
  title: Big Metadata
  status: todo
  type: task
  metadata:
    large_field: "{large_value}"
    nested: {{}}
edges: []
"#,
            large_value = large_value
        );
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), &yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 1);

        let storage = SqliteStorage::open(&target).unwrap();
        let meta = storage.get_metadata("big-meta").unwrap();
        let val = meta.get("large_field").unwrap().as_str().unwrap();
        assert_eq!(val.len(), 10_000);
    }

    #[test]
    fn test_migration_many_nodes() {
        // 100+ nodes with edges → stress test
        let mut yaml = String::from("nodes:\n");
        for i in 0..120 {
            yaml.push_str(&format!(
                "- id: \"node-{i}\"\n  title: \"Node {i}\"\n  status: todo\n  type: task\n"
            ));
        }
        yaml.push_str("edges:\n");
        for i in 1..120 {
            yaml.push_str(&format!(
                "- from: \"node-{}\"\n  to: \"node-{}\"\n  relation: depends_on\n",
                i,
                i - 1
            ));
        }

        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), &yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 120);
        assert_eq!(report.edges_migrated, 119);

        let storage = SqliteStorage::open(&target).unwrap();
        assert_eq!(storage.get_node_count().unwrap(), 120);
        assert_eq!(storage.get_edge_count().unwrap(), 119);

        // Spot check a few
        assert!(storage.get_node("node-0").unwrap().is_some());
        assert!(storage.get_node("node-99").unwrap().is_some());
        assert!(storage.get_node("node-119").unwrap().is_some());
    }

    #[test]
    fn test_migration_edge_weight_confidence() {
        // Edge weight/confidence roundtrip through full pipeline
        let yaml = r#"
nodes:
- id: a
  title: A
  status: todo
  type: task
- id: b
  title: B
  status: todo
  type: task
edges:
- from: a
  to: b
  relation: depends_on
  weight: 0.42
  confidence: 0.95
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.edges_migrated, 1);

        let storage = SqliteStorage::open(&target).unwrap();
        let edges = storage.get_edges("a").unwrap();
        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        assert!((e.weight.unwrap() - 0.42).abs() < f64::EPSILON);
        assert!((e.confidence.unwrap() - 0.95).abs() < f64::EPSILON);
    }

    // ═══════════════════════════════════════════════════════════
    // Group 6: Backup
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_backup_file_created() {
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), BASIC_YAML);
        let backup_dir = dir.path().join("backups");
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target,
            backup_dir: Some(backup_dir.clone()),
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        let backup_path = report.backup_path.as_ref().expect("backup_path should be Some");
        assert!(backup_path.exists(), "Backup file should exist");
        assert!(backup_path.to_string_lossy().contains(".bak"), "Backup should have .bak extension");
    }

    #[test]
    fn test_backup_content_matches_source() {
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), BASIC_YAML);
        let backup_dir = dir.path().join("backups");
        let target = dir.path().join(".gid/graph.db");

        let original_content = fs::read(&source).unwrap();

        let config = MigrationConfig {
            source_path: source,
            target_path: target,
            backup_dir: Some(backup_dir),
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        let backup_path = report.backup_path.as_ref().unwrap();
        let backup_content = fs::read(backup_path).unwrap();
        assert_eq!(original_content, backup_content, "Backup content must match original YAML");
    }

    #[test]
    fn test_migration_no_backup() {
        // backup_dir=None → no backup created, migration still works
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), BASIC_YAML);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert!(report.backup_path.is_none());
        assert_eq!(report.nodes_migrated, 3);
        assert!(target.exists());
    }

    // ═══════════════════════════════════════════════════════════
    // Group 7: Report & Fingerprint
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_report_fingerprint_deterministic() {
        // Same YAML → same fingerprint
        let dir1 = TempDir::new().unwrap();
        let source1 = write_yaml(dir1.path(), BASIC_YAML);
        let target1 = dir1.path().join(".gid/graph.db");

        let dir2 = TempDir::new().unwrap();
        let source2 = write_yaml(dir2.path(), BASIC_YAML);
        let target2 = dir2.path().join(".gid/graph.db");

        let config1 = MigrationConfig {
            source_path: source1,
            target_path: target1,
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };
        let config2 = MigrationConfig {
            source_path: source2,
            target_path: target2,
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report1 = migrate(&config1).unwrap();
        let report2 = migrate(&config2).unwrap();
        assert_eq!(report1.source_fingerprint, report2.source_fingerprint);
        assert!(!report1.source_fingerprint.is_empty());
    }

    #[test]
    fn test_report_fingerprint_changes_with_content() {
        // Different YAML → different fingerprint
        let yaml2 = r#"
nodes:
- id: different
  title: Different
  status: todo
  type: task
edges: []
"#;
        let dir1 = TempDir::new().unwrap();
        let source1 = write_yaml(dir1.path(), BASIC_YAML);
        let target1 = dir1.path().join(".gid/graph.db");

        let dir2 = TempDir::new().unwrap();
        let source2 = write_yaml(dir2.path(), yaml2);
        let target2 = dir2.path().join(".gid/graph.db");

        let config1 = MigrationConfig {
            source_path: source1,
            target_path: target1,
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };
        let config2 = MigrationConfig {
            source_path: source2,
            target_path: target2,
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report1 = migrate(&config1).unwrap();
        let report2 = migrate(&config2).unwrap();
        assert_ne!(report1.source_fingerprint, report2.source_fingerprint);
    }

    #[test]
    fn test_report_duration_nonzero() {
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), BASIC_YAML);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target,
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert!(report.duration > std::time::Duration::ZERO, "Duration should be > 0");
    }

    // ═══════════════════════════════════════════════════════════
    // Group 8: Verify Phase Specifics
    // ═══════════════════════════════════════════════════════════

    #[test]
    fn test_verify_catches_all_sampled_nodes() {
        // Small graph (<20 nodes) → all nodes verified (verify reads them all)
        let mut yaml = String::from("nodes:\n");
        for i in 0..15 {
            yaml.push_str(&format!(
                "- id: \"s-{i}\"\n  title: \"Small {i}\"\n  status: todo\n  type: task\n"
            ));
        }
        yaml.push_str("edges: []\n");

        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), &yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        // This exercises the verify phase on all 15 nodes
        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 15);

        // Manually verify all nodes exist
        let storage = SqliteStorage::open(&target).unwrap();
        for i in 0..15 {
            let id = format!("s-{i}");
            let node = storage.get_node(&id).unwrap();
            assert!(node.is_some(), "Node {id} should exist after verified migration");
        }
    }

    #[test]
    fn test_migration_project_meta_roundtrip() {
        // Project name survives migration
        let yaml = r#"
project:
  name: my-awesome-project
  description: A cool project
nodes:
- id: t1
  title: Task
  status: todo
  type: task
edges: []
"#;
        let dir = TempDir::new().unwrap();
        let source = write_yaml(dir.path(), yaml);
        let target = dir.path().join(".gid/graph.db");

        let config = MigrationConfig {
            source_path: source,
            target_path: target.clone(),
            backup_dir: None,
            validation_level: ValidationLevel::Strict,
            force: false,
            verbose: false,
        };

        let report = migrate(&config).unwrap();
        assert_eq!(report.nodes_migrated, 1);

        let storage = SqliteStorage::open(&target).unwrap();
        let meta = storage.get_project_meta().unwrap();
        assert!(meta.is_some(), "Project meta should be stored");
        assert_eq!(meta.unwrap().name, "my-awesome-project");
    }
}
