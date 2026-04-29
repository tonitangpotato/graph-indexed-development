//! GID CLI - Graph Indexed Development tool
//!
//! A unified graph-based project and task management CLI.

mod llm_client;
// ApiLlmClient now lives in gid-core (ritual::api_llm_client)

use std::path::PathBuf;
use std::path::Path;
use std::collections::HashSet;
use std::io::{self, Read};
use anyhow::{anyhow, Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use gid_core::{
    Graph, Node, Edge, NodeStatus, TaskSpec,
    load_graph, save_graph,
    parser::find_graph_file_walk_up,
    query::QueryEngine,
    validator::Validator,
    CodeGraph, CodeNode, NodeKind, analyze_impact_with_filters, format_impact_for_llm,
    assess_complexity_from_graph, assess_risk_level,
    unify::{codegraph_to_graph_nodes, merge_code_layer, graph_to_codegraph},
    // New modules
    HistoryManager,
    render, VisualFormat,
    analyze as advise_analyze,
    generate_graph_prompt, parse_llm_response,
    generate_semantify_prompt, apply_heuristic_layers,
    preview_rename, apply_rename,
    preview_merge, apply_merge,
    preview_split, apply_split, SplitDefinition,
    preview_extract, apply_extract,
    // Storage
    storage::{StorageBackend, load_graph_auto, save_graph_auto},
    // Harness
    harness::{
        create_plan,
        types::{ExecutionEvent, ExecutionStats},
        load_config,
        ExecutionState, ExecutionStatus,
    },
};

#[derive(Parser)]
#[command(name = "gid")]
#[command(author, version, about = "Graph Indexed Development - unified graph-based project tool")]
struct Cli {
    /// Path to graph file (default: auto-find .gid/graph.yml)
    #[arg(short, long, global = true)]
    graph: Option<PathBuf>,

    /// Storage backend: yaml or sqlite (default: auto-detect)
    #[arg(long, global = true)]
    backend: Option<String>,

    /// Output as JSON instead of human-readable text
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new .gid/graph.yml in current directory
    Init {
        /// Project name
        #[arg(short, long)]
        name: Option<String>,
        /// Project description
        #[arg(short, long)]
        desc: Option<String>,
    },

    /// Read and dump the graph as YAML
    Read {
        /// Filter by layer: code, project, all (default: all)
        #[arg(long, value_enum, default_value = "all")]
        layer: LayerFilter,
    },

    /// Validate the graph (cycles, orphans, missing refs).
    ///
    /// `--check-drift` (ISS-059) additionally compares the graph against
    /// the `.gid/` artifact tree and reports drift findings (artifacts
    /// without nodes, nodes without artifacts, status mismatches). Exits
    /// non-zero if any error-severity drift is found.
    Validate {
        /// Enable drift detection between graph and .gid/ artifacts (ISS-059).
        #[arg(long)]
        check_drift: bool,
    },

    /// Back-fill `doc_path` on graph nodes per ISS-058 §3.4 conventions.
    ///
    /// Walks every node where `doc_path IS NULL` and computes the canonical
    /// artifact path from `node_type` + `id` (issue → `.gid/issues/<id>/issue.md`,
    /// feature/design → `.gid/features/<slug>/design.md`, review →
    /// `.gid/features/<feat>/reviews/<name>.md`). Code-layer nodes (task, code,
    /// function, class, etc.) legitimately have no canonical doc and stay NULL.
    ///
    /// Default mode is dry-run — prints a per-node plan and totals without
    /// touching the database. Pass `--apply` to actually write the inferred
    /// paths back via the existing storage layer.
    BackfillDocPath {
        /// Apply the inferred updates. Without this flag the command is read-only.
        #[arg(long)]
        apply: bool,

        /// Print every entry (default: only fillable + skipped-missing rows).
        #[arg(long)]
        verbose: bool,
    },

    /// Repair the graph: remove orphan edges, duplicate nodes/edges, self-edges.
    ///
    /// By default runs in interactive mode: shows a plan and asks for confirmation
    /// before applying. Use --dry-run to preview without applying, or --yes to skip
    /// the prompt (CI use). At least one category flag must be selected, or use --all.
    Repair {
        /// Remove edges referencing missing nodes (orphan edges).
        #[arg(long)]
        orphan_edges: bool,

        /// Remove unconnected nodes (only safe types: code, file, function, etc.).
        /// User-authored nodes (task/issue/feature) are never auto-removed.
        #[arg(long)]
        orphan_nodes: bool,

        /// Drop duplicate node entries (same ID appearing multiple times).
        #[arg(long)]
        duplicate_nodes: bool,

        /// Drop duplicate edges (same from/to/relation triple).
        #[arg(long)]
        duplicate_edges: bool,

        /// Remove self-referential edges (from == to).
        #[arg(long)]
        self_edges: bool,

        /// Enable all repair categories.
        #[arg(long)]
        all: bool,

        /// Show the plan but do not modify the graph.
        #[arg(long)]
        dry_run: bool,

        /// Skip the confirmation prompt (for CI / scripts).
        #[arg(long, short = 'y')]
        yes: bool,

        /// Skip the automatic backup of the graph file before applying.
        #[arg(long)]
        no_backup: bool,
    },

    /// Show project overview: node/edge counts, languages, features
    About,

    /// List tasks with optional status filter
    Tasks {
        /// Filter by status (todo, in_progress, done, blocked, cancelled)
        #[arg(short, long)]
        status: Option<String>,
        /// Show only ready tasks (todo with all deps done)
        #[arg(short, long)]
        ready: bool,
        /// Filter by layer: code, project, all (default: project)
        #[arg(long, value_enum, default_value = "project")]
        layer: LayerFilter,
        /// Compact one-line-per-task output (icon id status)
        #[arg(short, long)]
        compact: bool,
    },

    /// Update a task's status
    TaskUpdate {
        /// Node ID
        id: String,
        /// New status
        #[arg(short, long)]
        status: String,
    },

    /// Mark a task as done and show newly unblocked tasks
    Complete {
        /// Node ID to complete
        id: String,
    },

    /// Add a new node to the graph
    AddNode {
        /// Node ID (unique identifier)
        id: String,
        /// Node title
        title: String,
        /// Description
        #[arg(short, long)]
        desc: Option<String>,
        /// Status (default: todo)
        #[arg(short, long)]
        status: Option<String>,
        /// Tags (comma-separated)
        #[arg(short, long)]
        tags: Option<String>,
        /// Node type (task, file, component, etc.)
        #[arg(long, name = "type")]
        node_type: Option<String>,
    },

    /// Add a feature with tasks in one command
    AddFeature {
        /// Feature name
        name: String,
        /// Task titles (repeat for each task)
        #[arg(short, long = "task")]
        tasks: Vec<String>,
        /// Task dependencies: "task-title:depends-on-title" (repeat for each dep)
        #[arg(short, long = "dep")]
        deps: Vec<String>,
    },

    /// Add a standalone task (with optional feature parent)
    AddTask {
        /// Task title
        title: String,
        /// Parent feature ID (e.g., feat-auth)
        #[arg(long = "for")]
        for_feature: Option<String>,
        /// Dependencies (node IDs or fuzzy references, repeat for each)
        #[arg(short, long = "depends")]
        depends: Vec<String>,
        /// Tags (comma-separated)
        #[arg(short, long)]
        tags: Option<String>,
        /// Priority (0=highest, 255=lowest)
        #[arg(short, long)]
        priority: Option<u8>,
    },

    /// Remove a node from the graph
    RemoveNode {
        /// Node ID to remove
        id: String,
    },

    /// Add an edge between two nodes
    AddEdge {
        /// Source node ID
        from: String,
        /// Target node ID
        to: String,
        /// Relation type (default: depends_on)
        #[arg(short, long, default_value = "depends_on")]
        relation: String,
    },

    /// Remove an edge between two nodes
    RemoveEdge {
        /// Source node ID
        from: String,
        /// Target node ID
        to: String,
        /// Relation type (if not specified, removes all edges between nodes)
        #[arg(short, long)]
        relation: Option<String>,
    },

    /// Query commands for graph analysis
    #[command(subcommand)]
    Query(QueryCommands),

    /// Batch operations on the graph (JSON array)
    EditGraph {
        /// JSON array of operations, e.g.:
        /// '[{"op":"add_node","id":"x","title":"X"},{"op":"add_edge","from":"x","to":"y"}]'
        operations: String,
    },

    /// Extract code graph from a directory
    Extract {
        /// Directory to extract from (default: current directory)
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Output format (yaml, json, summary)
        #[arg(short, long, default_value = "summary")]
        format: String,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Skip LSP refinement (LSP is enabled by default for precise call edges)
        #[arg(long)]
        no_lsp: bool,
        /// Force full rebuild (ignore cached metadata)
        #[arg(long)]
        force: bool,
        /// Skip auto-semantify after extract
        #[arg(long)]
        no_semantify: bool,
    },

    /// Analyze a file's code dependencies
    Analyze {
        /// File to analyze
        file: PathBuf,
        /// Show callers (who calls functions in this file)
        #[arg(short, long)]
        callers: bool,
        /// Show callees (what this file calls)
        #[arg(long)]
        callees: bool,
        /// Show impact analysis
        #[arg(short, long)]
        impact: bool,
    },

    /// Search for relevant code nodes by keywords
    CodeSearch {
        /// Keywords to search for (comma-separated)
        keywords: String,
        /// Directory to extract from (default: current directory)
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        /// Max characters for LLM-formatted output
        #[arg(long)]
        format_llm: Option<usize>,
    },

    /// Analyze test failures and trace causal chains
    CodeFailures {
        /// Changed node IDs (comma-separated, e.g. "repo::src/foo.py::MyClass")
        #[arg(long)]
        changed: String,
        /// Failed P2P test names (comma-separated, tests that passed before)
        #[arg(long)]
        p2p: Option<String>,
        /// Failed F2P test names (comma-separated, tests that should pass after fix)
        #[arg(long)]
        f2p: Option<String>,
        /// Directory to extract from
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },

    /// Find symptom nodes from problem description and test names
    CodeSymptoms {
        /// Problem statement / issue description
        problem: String,
        /// Test names (JSON array or newline-separated)
        #[arg(long, default_value = "[]")]
        tests: String,
        /// Directory to extract from
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },

    /// Trace causal chains from symptom nodes to root causes
    CodeTrace {
        /// Symptom node IDs (comma-separated)
        symptoms: String,
        /// Max search depth
        #[arg(long, default_value = "5")]
        depth: usize,
        /// Max chains to return
        #[arg(long, default_value = "10")]
        max_chains: usize,
        /// Directory to extract from
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },

    /// Assess complexity of changing specific nodes
    CodeComplexity {
        /// Node IDs to assess (comma-separated)
        nodes: String,
        /// Directory to extract from
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },

    /// Analyze impact of changing files
    CodeImpact {
        /// Files changed (comma-separated paths)
        files: String,
        /// Directory to extract from
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
        /// Filter by edge relation(s), comma-separated (e.g., calls,imports,tests_for)
        #[arg(long)]
        relation: Option<String>,
        /// Minimum edge confidence (0.0-1.0). Default 0.8 hides
        /// tree-sitter name-match fallback noise. Pass `0.0` to include
        /// everything. See ISS-035.
        #[arg(long, default_value_t = gid_core::DEFAULT_MIN_CONFIDENCE)]
        min_confidence: f64,
    },

    /// Extract code snippets for relevant nodes
    CodeSnippets {
        /// Keywords to find relevant nodes (comma-separated)
        keywords: String,
        /// Max lines per snippet
        #[arg(long, default_value = "30")]
        max_lines: usize,
        /// Directory to extract from
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },

    /// Show code graph schema (all files, classes, functions)
    Schema {
        /// Directory to extract from (default: current directory)
        #[arg(default_value = ".")]
        dir: PathBuf,
    },

    /// Show summary of a specific file in the code graph
    FileSummary {
        /// File path to summarize
        file: String,
        /// Directory to extract from (default: current directory)
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },

    // ═══════════════════════════════════════════════════════════════════════════════
    // New Commands
    // ═══════════════════════════════════════════════════════════════════════════════

    /// Manage graph history (snapshots, diff, restore)
    #[command(subcommand)]
    History(HistoryCommands),

    /// Visualize the graph (ASCII, DOT, Mermaid)
    Visual {
        /// Output format: ascii, dot, mermaid
        #[arg(short, long, default_value = "ascii")]
        format: String,
        /// Output to file instead of stdout
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Filter by layer: code, project, all (default: all)
        #[arg(long, value_enum, default_value = "all")]
        layer: LayerFilter,
    },

    /// Analyze graph and suggest improvements
    Advise {
        /// Show only errors
        #[arg(long)]
        errors_only: bool,
    },

    /// Generate LLM prompt for graph design from requirements
    Design {
        /// Requirements text (or read from stdin if not provided)
        requirements: Option<String>,
        /// Parse LLM response from stdin instead of generating prompt
        #[arg(long)]
        parse: bool,
        /// Merge into existing graph instead of overwriting
        #[arg(long)]
        merge: bool,
        /// Feature scope for merge (requires --merge)
        #[arg(long, requires = "merge")]
        scope: Option<String>,
        /// Preview merge without saving (requires --merge)
        #[arg(long, requires = "merge")]
        dry_run: bool,
    },

    /// Generate LLM prompt to semantify the graph
    Semantify {
        /// Apply heuristic layer assignments (no LLM needed)
        #[arg(long)]
        heuristic: bool,
        /// Parse LLM response from stdin
        #[arg(long)]
        parse: bool,
    },

    /// Refactor operations on the graph
    #[command(subcommand)]
    Refactor(RefactorCommands),

    // ═══════════════════════════════════════════════════════════════════════════════
    // Task Harness Commands
    // ═══════════════════════════════════════════════════════════════════════════════

    /// Show execution plan from graph topology
    Plan {
        /// Output format: json or text
        #[arg(short, long, default_value = "text")]
        format: String,
    },

    /// Execute the task plan (spawns sub-agents)
    Execute {
        /// Maximum concurrent sub-agents per layer
        #[arg(long)]
        max_concurrent: Option<usize>,
        /// Model for sub-agents
        #[arg(long)]
        model: Option<String>,
        /// Approval mode: auto, mixed, manual
        #[arg(long)]
        approval_mode: Option<String>,
        /// Show plan and exit without executing
        #[arg(long)]
        dry_run: bool,
    },

    /// Show execution statistics from telemetry log
    Stats,

    /// Approve pending execution (when status is waiting_approval)
    Approve,

    /// Stop execution gracefully (marks cancel_requested)
    Stop,

    /// Assemble context for target nodes (code, deps, callers, tests)
    Context {
        /// Target node IDs (comma-separated)
        #[arg(short, long, required = true, value_delimiter = ',')]
        targets: Vec<String>,
        /// Maximum token budget (default: 8000)
        #[arg(long, default_value = "8000")]
        max_tokens: usize,
        /// Maximum traversal depth (default: 2)
        #[arg(short, long, default_value = "2")]
        depth: u32,
        /// Include filter patterns (repeatable). Use "*.rs" for file globs, "type:function" for node types.
        #[arg(short, long)]
        include: Vec<String>,
        /// Output format: markdown, json, yaml (default: markdown)
        #[arg(short, long, default_value = "markdown")]
        format: String,
        /// Project root for source code loading (default: auto-detect)
        #[arg(long)]
        project_root: Option<PathBuf>,
    },

    // ═══════════════════════════════════════════════════════════════════════════════
    // Migration Commands (requires "sqlite" feature)
    // ═══════════════════════════════════════════════════════════════════════════════

    /// Migrate graph data from YAML (graph.yml) to SQLite (graph.db)
    Migrate {
        /// Source YAML file (default: .gid/graph.yml)
        #[arg(long)]
        source: Option<PathBuf>,
        /// Target SQLite database (default: .gid/graph.db)
        #[arg(long)]
        target: Option<PathBuf>,
        /// Overwrite existing database
        #[arg(long)]
        force: bool,
        /// Skip validation
        #[arg(long)]
        no_validate: bool,
        /// Show detailed output
        #[arg(short, long)]
        verbose: bool,
    },

    // ═══════════════════════════════════════════════════════════════════════════════
    // Ritual Commands (requires "ritual" feature)
    // ═══════════════════════════════════════════════════════════════════════════════

    /// Ritual pipeline orchestration
    #[command(subcommand)]
    Ritual(RitualCommands),

    /// Watch source directory and auto-sync code graph on file changes
    Watch {
        /// Directory to watch (default: current directory)
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Debounce interval in milliseconds (default: 1000)
        #[arg(long, default_value = "1000")]
        debounce: u64,
        /// Skip LSP refinement (LSP is enabled by default)
        #[arg(long)]
        no_lsp: bool,
        /// Skip semantify/bridge edge generation (faster)
        #[arg(long)]
        no_semantify: bool,
    },

    // ═══════════════════════════════════════════════════════════════════════════════
    // Infer Commands (requires "infomap" feature via "full")
    // ═══════════════════════════════════════════════════════════════════════════════

    /// Infer architecture: cluster code into components, label with LLM
    Infer {
        /// Inference level: component (clustering only), feature (+ LLM), all (= feature)
        #[arg(long, default_value = "all")]
        level: String,

        /// Run only a specific phase: clustering, labeling, or integration
        #[arg(long)]
        phase: Option<String>,

        /// LLM model for labeling (default: claude-sonnet-4-20250514)
        #[arg(long, default_value = "claude-sonnet-4-20250514")]
        model: String,

        /// Skip LLM — clustering only with auto-naming
        #[arg(long)]
        no_llm: bool,

        /// Preview results without writing to graph
        #[arg(long)]
        dry_run: bool,

        /// Output format: summary, yaml, json
        #[arg(long, default_value = "summary")]
        format: String,

        /// Max LLM token budget (default: 50000)
        #[arg(long, default_value = "50000")]
        max_tokens: usize,

        /// Source directory for auto-extract (if no code nodes in graph)
        #[arg(long)]
        source: Option<PathBuf>,

        /// Enable hierarchical clustering
        #[arg(long)]
        hierarchical: bool,

        /// Number of Infomap optimization trials
        #[arg(long)]
        num_trials: Option<u32>,

        /// Minimum community size (smaller clusters dissolved)
        #[arg(long)]
        min_community_size: Option<usize>,

        /// Maximum files per component (oversized clusters are split)
        #[arg(long)]
        max_cluster_size: Option<usize>,

        /// Override per-relation edge weight (repeatable). Format: `relation=weight`.
        ///
        /// Examples:
        ///   --edge-weight calls=1.5 --edge-weight imports=0.5
        ///
        /// Unknown relations (not in defaults) are added; existing ones are
        /// overridden. Set to 0 to ignore a relation entirely. (ISS-049)
        #[arg(long = "edge-weight", value_name = "RELATION=WEIGHT")]
        edge_weights: Vec<String>,
    },

    /// Manage the project registry (which projects exist on this machine).
    ///
    /// Registry file: `$XDG_CONFIG_HOME/gid/projects.yml` (or `~/.config/gid/projects.yml`).
    /// Resolves ISS-020: eliminates project path guessing in cross-project sessions.
    Project {
        #[command(subcommand)]
        action: ProjectCommands,
    },

    /// Manage `.gid/` artifacts (issues, features, designs, reviews, …).
    ///
    /// Six kind-agnostic verbs that operate on any artifact kind defined by
    /// the project's `Layout` (built-in default + optional `.gid/layout.yml`
    /// override). Adding a new artifact kind requires NO changes here —
    /// that is the binding D2 invariant of ISS-053.
    ///
    /// Refs: short form `<project>:<short_or_path>` (e.g. `engram:ISS-022`,
    /// `gid-rs:.gid/issues/ISS-053/issue.md`), or unqualified short/path
    /// when `--project` is supplied (or inferable from the current working
    /// directory).
    Artifact {
        #[command(subcommand)]
        action: ArtifactCommands,
    },
}

#[derive(Subcommand)]
enum ProjectCommands {
    /// List all registered projects.
    List {
        /// Include archived projects in the output.
        #[arg(long)]
        all: bool,
    },
    /// Resolve a project name or alias to its canonical path. Exits 1 if not found.
    Resolve {
        /// Project name or alias (case-insensitive).
        ident: String,
    },
    /// Register a new project. Validates that <path>/.gid/ exists.
    Add {
        /// Canonical name (must be unique).
        name: String,
        /// Absolute path to the project root.
        path: PathBuf,
        /// Comma-separated aliases (e.g. "engram-ai,ea").
        #[arg(long)]
        aliases: Option<String>,
        /// Default git branch (informational).
        #[arg(long)]
        default_branch: Option<String>,
        /// Comma-separated tags.
        #[arg(long)]
        tags: Option<String>,
        /// Optional free-form note.
        #[arg(long)]
        notes: Option<String>,
    },
    /// Remove a project by its canonical name. Aliases are not accepted here (safety).
    Remove {
        name: String,
    },
    /// Print the path to the registry file (creates it if missing is your job, not ours).
    Where,
}

#[derive(Subcommand)]
enum ArtifactCommands {
    /// List artifacts under a project's `.gid/`.
    ///
    /// JSON shape: `[{project, path, kind, title}, ...]`.
    List {
        /// Filter by kind (e.g. `issue`, `feature`, `design`, `review`, `note`).
        #[arg(long)]
        kind: Option<String>,
        /// Project name or alias (resolved via `~/.config/gid/projects.yml`).
        /// Defaults to the project containing the current working directory.
        #[arg(long, short = 'p')]
        project: Option<String>,
    },

    /// Show a single artifact: id, kind, metadata, body.
    ///
    /// JSON shape: `{id: {project, path}, kind, metadata: {...}, body: "..."}`.
    Show {
        /// Artifact ref. Either `<project>:<short_or_path>` (e.g.
        /// `engram:ISS-022`) or an unqualified id (when `--project` is set
        /// or the cwd is inside a registered project).
        artifact_ref: String,
        /// Project (overrides any project component in `<artifact_ref>`).
        #[arg(long, short = 'p')]
        project: Option<String>,
    },

    /// Create a new artifact of the given kind.
    ///
    /// `next_id` / `next_path` are computed via [`Layout`]. Caller-supplied
    /// slots (e.g. `slug=resolution-pipeline`) are accepted as positional
    /// `key=value` args for kinds that need them.
    New {
        /// Artifact kind (must be defined in the project's Layout).
        #[arg(long)]
        kind: String,
        /// Project name or alias.
        #[arg(long, short = 'p')]
        project: Option<String>,
        /// Parent artifact ref (required for parent-scoped kinds like `review`).
        #[arg(long)]
        parent: Option<String>,
        /// Optional `title:` frontmatter field for the new artifact.
        #[arg(long, short = 't')]
        title: Option<String>,
        /// Layout slot overrides as `key=value`. Repeatable. Used for slug
        /// kinds (e.g. `slug=resolution-pipeline`) or any custom placeholder.
        slots: Vec<String>,
    },

    /// Update an artifact's frontmatter fields. Atomic, byte-exact for
    /// untouched fields (D4).
    Update {
        /// Artifact ref.
        artifact_ref: String,
        /// Project (overrides any project component in `<artifact_ref>`).
        #[arg(long, short = 'p')]
        project: Option<String>,
        /// Field assignment as `key=value`. Repeatable. A value containing
        /// commas is parsed as a YAML list (e.g. `--field blocks=ISS-2,ISS-3`).
        #[arg(long = "field", value_name = "KEY=VALUE")]
        fields: Vec<String>,
    },

    /// Add a typed relation from one artifact to another by appending to
    /// the source's frontmatter (e.g. `relate A blocks B` adds `B` to A's
    /// `blocks:` list). No separate relations DB.
    Relate {
        /// Source artifact ref (the one whose frontmatter is edited).
        from: String,
        /// Relation kind (frontmatter field name: `blocks`, `related`,
        /// `depends_on`, `applies-to`, …).
        kind: String,
        /// Target artifact ref.
        to: String,
        /// Project for `<from>` (overrides any project component in `<from>`).
        #[arg(long, short = 'p')]
        project: Option<String>,
    },

    /// Find every artifact that references the given target. Mirrors
    /// `ArtifactStore::relations_to`. JSON shape: `[{from, to, kind, source}, ...]`.
    Refs {
        /// Target artifact ref.
        artifact_ref: String,
        /// Project (overrides any project component in `<artifact_ref>`).
        #[arg(long, short = 'p')]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum QueryCommands {
    /// Impact analysis: what nodes are affected if this node changes?
    Impact {
        /// Node ID to analyze
        node: String,
        /// Filter by edge relation(s), comma-separated (e.g., depends_on,implements)
        #[arg(short, long)]
        relation: Option<String>,
        /// Filter by layer: code, project, all (default: all)
        #[arg(long, value_enum, default_value = "all")]
        layer: LayerFilter,
        /// Filter results by node_type (e.g., code, task, feature)
        #[arg(long, name = "type")]
        type_filter: Option<String>,
        /// Minimum edge confidence (0.0-1.0). Edges with `confidence`
        /// below this are hidden. Default 0.8 filters out tree-sitter
        /// name-match fallback noise. Pass `0.0` to include everything.
        /// Edges with no confidence (None) are always treated as trusted.
        /// See ISS-035.
        #[arg(long, default_value_t = gid_core::DEFAULT_MIN_CONFIDENCE)]
        min_confidence: f64,
    },

    /// Show dependencies of a node
    Deps {
        /// Node ID
        node: String,
        /// Include transitive dependencies
        #[arg(short, long)]
        transitive: bool,
        /// Filter by edge relation(s), comma-separated (e.g., depends_on,implements)
        #[arg(long)]
        relation: Option<String>,
        /// Filter by layer: code, project, all (default: all)
        #[arg(long, value_enum, default_value = "all")]
        layer: LayerFilter,
        /// Filter results by node_type (e.g., code, task, feature)
        #[arg(long, name = "type")]
        type_filter: Option<String>,
        /// Minimum edge confidence (0.0-1.0). See `query impact --help`.
        #[arg(long, default_value_t = gid_core::DEFAULT_MIN_CONFIDENCE)]
        min_confidence: f64,
    },

    /// Find path between two nodes
    Path {
        /// Source node
        from: String,
        /// Target node
        to: String,
    },

    /// Find common dependencies (shared ancestors) of two nodes
    CommonCause {
        /// First node
        a: String,
        /// Second node
        b: String,
    },

    /// Topological sort of nodes
    Topo,
}

#[derive(Subcommand)]
enum HistoryCommands {
    /// List all history snapshots
    List,

    /// Save a snapshot with an optional message
    Save {
        /// Commit-like message
        #[arg(short, long)]
        message: Option<String>,
    },

    /// Diff current graph against a historical version (or diff two versions)
    Diff {
        /// Version filename to compare against (or first version when comparing two)
        version: String,
        /// Optional second version filename; when provided, diffs version vs version2
        version2: Option<String>,
    },

    /// Restore a historical version
    Restore {
        /// Version filename to restore
        version: String,
        /// Force restore without confirmation
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum RefactorCommands {
    /// Rename a node (updates all edges)
    Rename {
        /// Current node ID
        old: String,
        /// New node ID
        new: String,
        /// Apply changes (default is preview only)
        #[arg(long)]
        apply: bool,
    },

    /// Merge two nodes into one
    Merge {
        /// First node ID
        a: String,
        /// Second node ID
        b: String,
        /// New merged node ID
        new_id: String,
        /// Apply changes
        #[arg(long)]
        apply: bool,
    },

    /// Split a node into multiple nodes
    Split {
        /// Node ID to split
        node: String,
        /// New node IDs (comma-separated)
        #[arg(short, long, value_delimiter = ',')]
        into: Vec<String>,
        /// Apply changes
        #[arg(long)]
        apply: bool,
    },

    /// Extract nodes into a new parent/module
    Extract {
        /// Node IDs to extract (comma-separated)
        #[arg(short, long, value_delimiter = ',')]
        nodes: Vec<String>,
        /// New parent node ID
        #[arg(short, long)]
        parent: String,
        /// New parent title
        #[arg(short, long)]
        title: String,
        /// Apply changes
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum RitualCommands {
    /// Initialize a new ritual from a template
    Init {
        /// Template name (default: full-dev-cycle)
        #[arg(short, long, default_value = "full-dev-cycle")]
        template: String,
    },

    /// Run the ritual (start or resume)
    Run {
        /// Auto-approve all gates (useful for CI/testing)
        #[arg(long)]
        auto_approve: bool,
        /// Initialize from template before running (combines init + run)
        #[arg(long, short)]
        template: Option<String>,
        /// Override the default model for skill phases
        #[arg(long, short)]
        model: Option<String>,
    },

    /// Show current ritual status
    Status,

    /// Approve the current pending phase
    Approve,

    /// Skip the current phase
    Skip,

    /// Cancel the ritual
    Cancel,

    /// List available templates
    Templates,
}

/// Layer filter for graph commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LayerFilter {
    /// Code nodes only (source == "extract")
    Code,
    /// Project nodes only (source == "project" or None)
    Project,
    /// All nodes
    All,
}

/// Apply a layer filter to a graph, returning a new graph with only the matching
/// nodes and edges whose both endpoints are in the visible set.
fn apply_layer_filter(graph: &Graph, layer: LayerFilter) -> Graph {
    match layer {
        LayerFilter::All => graph.clone(),
        _ => {
            let visible_ids: HashSet<String> = match layer {
                LayerFilter::Code => graph.code_nodes().into_iter().map(|n| n.id.clone()).collect(),
                LayerFilter::Project => graph.project_nodes().into_iter().map(|n| n.id.clone()).collect(),
                LayerFilter::All => unreachable!(),
            };
            let nodes: Vec<Node> = graph.nodes.iter()
                .filter(|n| visible_ids.contains(&n.id))
                .cloned()
                .collect();
            let edges: Vec<Edge> = graph.edges.iter()
                .filter(|e| visible_ids.contains(&e.from) && visible_ids.contains(&e.to))
                .cloned()
                .collect();
            Graph {
                project: graph.project.clone(),
                nodes,
                edges,
            }
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // For commands that use the graph, resolve context with backend detection.
    // Commands that don't need the graph (Init, code-graph commands) skip this.
    let backend_arg = cli.backend.clone();

    match cli.command {
        Commands::Init { name, desc } => cmd_init(name, desc, cli.json),
        Commands::Read { layer } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_read_ctx(&ctx, layer, cli.json)
        }
        Commands::Validate { check_drift } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_validate_ctx(&ctx, check_drift, cli.json)
        }
        Commands::BackfillDocPath { apply, verbose } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_backfill_doc_path_ctx(&ctx, apply, verbose, cli.json)
        }
        Commands::Repair {
            orphan_edges,
            orphan_nodes,
            duplicate_nodes,
            duplicate_edges,
            self_edges,
            all,
            dry_run,
            yes,
            no_backup,
        } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            let opts = if all {
                gid_core::RepairOptions::all()
            } else {
                gid_core::RepairOptions {
                    orphan_edges,
                    orphan_nodes,
                    duplicate_nodes,
                    duplicate_edges,
                    self_edges,
                }
            };
            cmd_repair_ctx(&ctx, opts, dry_run, yes, no_backup, cli.json)
        }
        Commands::About => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_about_ctx(&ctx, cli.json)
        }
        Commands::Tasks { status, ready, layer, compact } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_tasks_ctx(&ctx, status, ready, layer, compact, cli.json)
        }
        Commands::TaskUpdate { id, status } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_task_update_ctx(&ctx, &id, &status, cli.json)
        }
        Commands::Complete { id } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_complete_ctx(&ctx, &id, cli.json)
        }
        Commands::AddNode { id, title, desc, status, tags, node_type } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_add_node_ctx(&ctx, AddNodeOpts { id, title, desc, status, tags, node_type }, cli.json)
        }
        Commands::AddFeature { name, tasks, deps } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            let mut graph = ctx.load()?;

            // Parse task specs - for CLI, all tasks start as todo with no tags
            let mut task_specs: Vec<TaskSpec> = tasks.iter().map(|t| TaskSpec {
                title: t.clone(),
                status: None,
                tags: vec![],
                deps: vec![],
            }).collect();

            // Add deps from --dep flags (format: "from_title:to_title")
            for dep_str in &deps {
                if let Some((from_title, to_title)) = dep_str.split_once(':') {
                    if let Some(spec) = task_specs.iter_mut().find(|s| s.title == from_title) {
                        spec.deps.push(to_title.to_string());
                    }
                }
            }

            let feat_id = graph.add_feature(&name, &task_specs);
            ctx.save(&graph)?;

            println!("✅ Created feature '{}' with {} tasks", feat_id, tasks.len());
            let feature_slug = gid_core::slugify::slugify(&name);
            for spec in &task_specs {
                let slug = gid_core::slugify::slugify(&spec.title);
                println!("  📋 task-{}-{}: {}", feature_slug, slug, spec.title);
            }
            Ok(())
        }
        Commands::AddTask { title, for_feature, depends, tags, priority } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            let mut graph = ctx.load()?;
            let tag_vec: Vec<String> = tags.map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default();

            let task_id = graph.add_task(&title, for_feature.as_deref(), &depends, &tag_vec, priority);
            ctx.save(&graph)?;

            println!("✅ Created task '{}'", task_id);
            if let Some(ref feat) = for_feature {
                println!("  🔗 implements → {}", feat);
            }
            for dep in &depends {
                println!("  🔗 depends_on → {}", dep);
            }
            Ok(())
        }
        Commands::RemoveNode { id } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_remove_node_ctx(&ctx, &id, cli.json)
        }
        Commands::AddEdge { from, to, relation } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_add_edge_ctx(&ctx, &from, &to, &relation, cli.json)
        }
        Commands::RemoveEdge { from, to, relation } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_remove_edge_ctx(&ctx, &from, &to, relation.as_deref(), cli.json)
        }
        Commands::Query(qc) => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            match qc {
                QueryCommands::Impact { node, relation, layer, type_filter, min_confidence } => cmd_query_impact_ctx(&ctx, &node, relation.as_deref(), layer, type_filter.as_deref(), min_confidence, cli.json),
                QueryCommands::Deps { node, transitive, relation, layer, type_filter, min_confidence } => {
                    cmd_query_deps_ctx(&ctx, QueryDepsOpts {
                        node: &node,
                        transitive,
                        relation: relation.as_deref(),
                        layer,
                        type_filter: type_filter.as_deref(),
                        min_confidence,
                        json: cli.json,
                    })
                }
                QueryCommands::Path { from, to } => cmd_query_path_ctx(&ctx, &from, &to, cli.json),
                QueryCommands::CommonCause { a, b } => cmd_query_common_ctx(&ctx, &a, &b, cli.json),
                QueryCommands::Topo => cmd_query_topo_ctx(&ctx, cli.json),
            }
        }
        Commands::EditGraph { operations } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_edit_graph_ctx(&ctx, &operations, cli.json)
        }
        Commands::Extract { dir, format, output, no_lsp, force, no_semantify } => cmd_extract(ExtractOpts {
            dir: &dir,
            format: &format,
            output: output.as_deref(),
            json_flag: cli.json,
            lsp: !no_lsp,
            force,
            no_semantify,
            graph_override: cli.graph.as_ref(),
            backend_arg,
        }),
        Commands::Analyze { file, callers, callees, impact } => cmd_analyze(&file, callers, callees, impact, cli.json),
        Commands::CodeSearch { keywords, dir, format_llm } => cmd_code_search(&dir, &keywords, format_llm, cli.json),
        Commands::CodeFailures { changed, p2p, f2p, dir } => cmd_code_failures(&dir, &changed, p2p.as_deref(), f2p.as_deref(), cli.json),
        Commands::CodeSymptoms { problem, tests, dir } => cmd_code_symptoms(&dir, &problem, &tests, cli.json),
        Commands::CodeTrace { symptoms, depth, max_chains, dir } => cmd_code_trace(&dir, &symptoms, depth, max_chains, cli.json),
        Commands::CodeComplexity { nodes, dir } => cmd_code_complexity(&dir, &nodes, cli.json),
        Commands::CodeImpact { files, dir, relation, min_confidence } => cmd_code_impact(&dir, &files, relation.as_deref(), min_confidence, cli.json),
        Commands::CodeSnippets { keywords, max_lines, dir } => cmd_code_snippets(&dir, &keywords, max_lines, cli.json),
        Commands::Schema { dir } => cmd_schema(&dir, cli.json),
        Commands::FileSummary { file, dir } => cmd_file_summary(&dir, &file, cli.json),
        
        // New commands
        Commands::History(hc) => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            match hc {
                HistoryCommands::List => cmd_history_list(&ctx, cli.json),
                HistoryCommands::Save { message } => cmd_history_save(&ctx, message.as_deref(), cli.json),
                HistoryCommands::Diff { version, version2 } => cmd_history_diff(&ctx, &version, version2.as_deref(), cli.json),
                HistoryCommands::Restore { version, force } => cmd_history_restore(&ctx, &version, force, cli.json),
            }
        }
        Commands::Visual { format, output, layer } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_visual(&ctx, &format, output.as_deref(), layer, cli.json)
        }
        Commands::Advise { errors_only } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_advise(&ctx, errors_only, cli.json)
        }
        Commands::Design { requirements, parse, merge, scope, dry_run } => {
            let ctx = if parse {
                Some(resolve_graph_ctx(cli.graph, backend_arg)?)
            } else {
                None
            };
            cmd_design(ctx.as_ref(), requirements, parse, merge, scope, dry_run, cli.json)
        }
        Commands::Semantify { heuristic, parse } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_semantify(&ctx, heuristic, parse, cli.json)
        }
        Commands::Refactor(rc) => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            match rc {
                RefactorCommands::Rename { old, new, apply } => {
                    cmd_refactor_rename(&ctx, &old, &new, apply, cli.json)
                }
                RefactorCommands::Merge { a, b, new_id, apply } => {
                    cmd_refactor_merge(&ctx, &a, &b, &new_id, apply, cli.json)
                }
                RefactorCommands::Split { node, into, apply } => {
                    cmd_refactor_split(&ctx, &node, &into, apply, cli.json)
                }
                RefactorCommands::Extract { nodes, parent, title, apply } => {
                    cmd_refactor_extract(&ctx, &nodes, &parent, &title, apply, cli.json)
                }
            }
        }

        // Task Harness commands
        Commands::Plan { format } => {
            let ctx = resolve_graph_ctx(cli.graph.clone(), backend_arg.clone())?;
            cmd_plan(&ctx, &format, cli.json)
        }
        Commands::Execute { max_concurrent, model, approval_mode, dry_run } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_execute(&ctx, max_concurrent, model, approval_mode, dry_run, cli.json)
        }
        Commands::Stats => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_stats(&ctx, cli.json)
        }
        Commands::Approve => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_approve(&ctx, cli.json)
        }
        Commands::Stop => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_stop(&ctx, cli.json)
        }

        // Context command
        Commands::Context { targets, max_tokens, depth, include, format, project_root } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_context_ctx(&ctx, ContextOpts {
                targets,
                max_tokens,
                depth,
                include,
                format,
                project_root,
                json_flag: cli.json,
            })
        }

        // Migration command
        Commands::Migrate { source, target, force, no_validate, verbose } => {
            cmd_migrate(resolve_graph_path(cli.graph)?, source, target, force, no_validate, verbose, cli.json)
        }

        // Ritual commands
        Commands::Ritual(rc) => {
            let cwd = std::env::current_dir()?;
            match rc {
                RitualCommands::Init { template } => cmd_ritual_init(&cwd, &template, cli.json),
                RitualCommands::Run { auto_approve, template, model } => {
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(cmd_ritual_run(&cwd, auto_approve, template, model, cli.json))
                }
                RitualCommands::Status => cmd_ritual_status(&cwd, cli.json),
                RitualCommands::Approve => {
                    let rt = tokio::runtime::Runtime::new()?;
                    rt.block_on(cmd_ritual_approve(&cwd, cli.json))
                }
                RitualCommands::Skip => cmd_ritual_skip(&cwd, cli.json),
                RitualCommands::Cancel => cmd_ritual_cancel(&cwd, cli.json),
                RitualCommands::Templates => cmd_ritual_templates(&cwd, cli.json),
            }
        }
        Commands::Watch { dir, debounce, no_lsp, no_semantify } => {
            cmd_watch(&dir, debounce, no_lsp, no_semantify, cli.graph.as_ref())
        }
        Commands::Infer { level, phase, model, no_llm, dry_run, format, max_tokens, source, hierarchical, num_trials, min_community_size, max_cluster_size, edge_weights } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cmd_infer(&ctx, InferOpts {
                level_str: &level,
                phase: phase.as_deref(),
                model: &model,
                no_llm,
                dry_run,
                format_str: &format,
                max_tokens,
                source,
                hierarchical,
                num_trials,
                min_community_size,
                max_cluster_size,
                edge_weight_overrides: edge_weights,
                json: cli.json,
            }))
        }
        Commands::Project { action } => cmd_project(action, cli.json),
        Commands::Artifact { action } => cmd_artifact(action, cli.json),
    }
}

/// Resolve graph path: use provided path or auto-find in cwd.
fn resolve_graph_path(provided: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = provided {
        return Ok(p);
    }

    let cwd = std::env::current_dir()?;
    // Walk up from cwd to find .gid/graph.yml (like git finding .git/)
    find_graph_file_walk_up(&cwd).context(
        "No graph file found. Use --graph <path> or run 'gid init' to create one.\n\
         (Searched current directory and all parent directories for .gid/graph.yml)"
    )
}

// =============================================================================
// Backend-Aware Graph Context
// =============================================================================

/// Resolved graph location with backend information.
///
/// Carries the `.gid/` directory path plus the selected backend,
/// enabling commands to use either YAML or SQLite transparently.
struct GraphContext {
    /// Path to the `.gid/` directory.
    gid_dir: PathBuf,
    /// Path to graph.yml (for backward compatibility with history, etc.)
    graph_yml: PathBuf,
    /// Selected storage backend.
    backend: StorageBackend,
}

impl GraphContext {
    /// Load the full graph from the resolved backend.
    fn load(&self) -> Result<Graph> {
        load_graph_auto(&self.gid_dir, Some(self.backend))
            .map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Save the full graph to the resolved backend.
    fn save(&self, graph: &Graph) -> Result<()> {
        save_graph_auto(graph, &self.gid_dir, Some(self.backend))
            .map_err(|e| anyhow::anyhow!("{}", e))
    }
}

/// Resolve graph context: find `.gid/` directory, parse backend flag, auto-detect.
fn resolve_graph_ctx(graph_arg: Option<PathBuf>, backend_arg: Option<String>) -> Result<GraphContext> {
    // Find the graph.yml path (which tells us where .gid/ is)
    let graph_yml = resolve_graph_path(graph_arg)?;
    let gid_dir = graph_yml.parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();

    // Parse explicit backend flag
    let explicit_backend = match backend_arg {
        Some(ref s) => Some(s.parse::<StorageBackend>()
            .map_err(|e| anyhow::anyhow!("{}", e))?),
        None => None,
    };

    // Resolve backend (explicit or auto-detect)
    let backend = gid_core::storage::resolve_backend(explicit_backend, &gid_dir);

    Ok(GraphContext {
        gid_dir,
        graph_yml,
        backend,
    })
}

// ═══════════════════════════════════════════════════════════════════════════════
// Backend-Aware Command Wrappers
// ═══════════════════════════════════════════════════════════════════════════════
//
// These _ctx functions load/save the graph via GraphContext (auto-detect
// YAML vs SQLite backend). They delegate to the existing command logic
// by using ctx.load() / ctx.save() instead of load_graph() / save_graph().

fn cmd_read_ctx(ctx: &GraphContext, layer: LayerFilter, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let filtered = apply_layer_filter(&graph, layer);
    if json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
    } else {
        let yaml = serde_yaml::to_string(&filtered)?;
        print!("{}", yaml);
    }
    Ok(())
}

fn cmd_validate_ctx(ctx: &GraphContext, check_drift: bool, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let validator = Validator::new(&graph);
    let result = validator.validate();

    // Optional drift detection (ISS-059). Runs after graph-internal validation
    // so the order matches user expectation (cycles/orphans first, then "the
    // graph drifted from .gid/"). Drift findings have their own severity and
    // contribute their own bit to the non-zero exit code.
    let drift_report: Option<gid_core::validate::drift::DriftReport> = if check_drift {
        let project_root = ctx
            .gid_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| ctx.gid_dir.clone());
        let project_config = gid_core::config::load_project_config(&project_root);
        // Resolve the artifact store rooted at the same project so drift's
        // disk walk and the graph's `doc_path` values are talking about the
        // same `.gid/` tree.
        let project_name = project_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string();
        match gid_core::artifact::store::ArtifactStore::open_at(
            project_name,
            project_root.clone(),
        ) {
            Ok(store) => Some(gid_core::validate::drift::check_drift(
                &graph,
                &store,
                &project_root,
                &project_config.drift,
            )),
            Err(e) => {
                // Hard-failing here would defeat drift's purpose (it must run
                // on half-broken projects). Surface the issue and continue
                // with graph-only validation.
                eprintln!("warning: drift detection skipped — could not open artifact store: {e}");
                None
            }
        }
    } else {
        None
    };

    if json {
        let mut payload = serde_json::json!({
            "valid": result.is_valid(),
            "issues": result.issue_count(),
            "orphan_nodes": result.orphan_nodes,
            "missing_refs": result.missing_refs.iter().map(|r| {
                serde_json::json!({"from": r.edge_from, "to": r.edge_to, "missing": r.missing_node})
            }).collect::<Vec<_>>(),
            "cycles": result.cycles,
            "duplicate_nodes": result.duplicate_nodes,
        });
        if let Some(ref report) = drift_report {
            payload["drift"] = serde_json::to_value(report)
                .unwrap_or_else(|_| serde_json::json!({"error": "serialize drift report failed"}));
        }
        println!("{}", payload);
    } else {
        println!("{}", result);
        if let Some(ref report) = drift_report {
            let rendered = gid_core::validate::drift::render_text(report);
            if !rendered.is_empty() {
                println!("\n--- drift detection (ISS-059) ---\n{}", rendered);
            } else if check_drift {
                println!("\n--- drift detection (ISS-059) ---");
                println!("✓ no drift");
            }
        }
    }

    let drift_has_errors = drift_report
        .as_ref()
        .map(|r| {
            r.findings
                .iter()
                .any(|f| matches!(f.severity, gid_core::validate::drift::Severity::Error))
        })
        .unwrap_or(false);

    if !result.is_valid() || drift_has_errors {
        std::process::exit(1);
    }
    Ok(())
}

/// Implements `gid backfill-doc-path` (ISS-058 §3.4).
///
/// Default = dry-run: prints per-node outcomes + totals, no writes. With
/// `--apply`, persists the inferred `doc_path` values back via the storage
/// layer (round-trips through whichever backend `ctx` resolved to — the
/// SQLite path is the canonical one for ISS-058 since v2 only exists there).
fn cmd_backfill_doc_path_ctx(
    ctx: &GraphContext,
    apply: bool,
    verbose: bool,
    json: bool,
) -> Result<()> {
    use gid_core::backfill_doc_path::{
        applicable_updates, default_file_exists, plan_backfill, BackfillOutcome,
    };

    // Resolve project root (= parent of .gid/) so file-existence checks
    // and any displayed paths line up with what the user sees in their repo.
    let project_root = ctx
        .gid_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| ctx.gid_dir.clone());

    let mut graph = ctx.load()?;
    let plan = plan_backfill(&graph.nodes, &project_root, default_file_exists);

    if json {
        let entries: Vec<_> = plan
            .entries
            .iter()
            .map(|e| {
                let (label, path, existing) = match &e.outcome {
                    BackfillOutcome::Fillable { inferred_path } => {
                        ("fillable", Some(inferred_path.as_str()), None)
                    }
                    BackfillOutcome::SkippedMissing { inferred_path } => {
                        ("skipped-missing", Some(inferred_path.as_str()), None)
                    }
                    BackfillOutcome::SkippedNoRule => ("skipped-no-rule", None, None),
                    BackfillOutcome::AlreadySet { existing } => {
                        ("already-set", None, Some(existing.as_str()))
                    }
                };
                serde_json::json!({
                    "id": e.node_id,
                    "node_type": e.node_type,
                    "outcome": label,
                    "inferred_path": path,
                    "existing_doc_path": existing,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "dry_run": !apply,
                "applied": false,    // updated below if apply succeeds
                "totals": {
                    "fillable":        plan.fillable,
                    "skipped_missing": plan.skipped_missing,
                    "skipped_no_rule": plan.skipped_no_rule,
                    "already_set":     plan.already_set,
                    "total":           plan.total(),
                },
                "entries": entries,
            })
        );
    } else {
        println!("ISS-058 doc_path back-fill — {} mode",
                 if apply { "apply" } else { "dry-run" });
        println!("Project root: {}", project_root.display());
        println!();

        for e in &plan.entries {
            match &e.outcome {
                BackfillOutcome::Fillable { inferred_path } => {
                    println!(
                        "  fillable        {:<40} → {}",
                        e.node_id, inferred_path
                    );
                }
                BackfillOutcome::SkippedMissing { inferred_path } => {
                    println!(
                        "  skipped-missing {:<40}   {} (file not found)",
                        e.node_id, inferred_path
                    );
                }
                BackfillOutcome::SkippedNoRule if verbose => {
                    println!(
                        "  skipped-no-rule {:<40}   ({})",
                        e.node_id,
                        e.node_type.as_deref().unwrap_or("<no type>")
                    );
                }
                BackfillOutcome::AlreadySet { existing } if verbose => {
                    println!("  already-set     {:<40}   {}", e.node_id, existing);
                }
                _ => {} // omitted in non-verbose mode
            }
        }

        println!();
        println!("Totals:");
        println!("  fillable:        {}", plan.fillable);
        println!("  skipped-missing: {}", plan.skipped_missing);
        println!("  skipped-no-rule: {}", plan.skipped_no_rule);
        println!("  already-set:     {}", plan.already_set);
        println!("  total nodes:     {}", plan.total());
    }

    if !apply {
        if !json {
            println!();
            println!("Dry-run only — re-run with `--apply` to persist {} update(s).",
                     plan.fillable);
        }
        return Ok(());
    }

    // Apply mode: mutate the in-memory graph, then save through the existing
    // backend (no direct SQL — go through the same save_graph_auto path that
    // every other write uses, so we inherit migration + validation handling).
    let updates = applicable_updates(&plan);
    let n_updates = updates.len();
    if n_updates == 0 {
        if !json {
            println!();
            println!("Nothing to apply.");
        }
        return Ok(());
    }

    use std::collections::HashMap;
    let map: HashMap<String, String> = updates.into_iter().collect();
    for node in graph.nodes.iter_mut() {
        if let Some(path) = map.get(&node.id) {
            node.doc_path = Some(path.clone());
        }
    }

    ctx.save(&graph)?;

    if json {
        // Second JSON line for apply summary — keeps the structured stream parseable.
        println!(
            "{}",
            serde_json::json!({
                "applied": true,
                "updates_written": n_updates,
            })
        );
    } else {
        println!();
        println!("Applied {} update(s).", n_updates);
    }
    Ok(())
}

fn cmd_repair_ctx(
    ctx: &GraphContext,
    opts: gid_core::RepairOptions,
    dry_run: bool,
    yes: bool,
    no_backup: bool,
    json: bool,
) -> Result<()> {
    use std::io::{self, BufRead, Write};

    if !opts.any() {
        anyhow::bail!(
            "No repair categories selected. Use --all or one of --orphan-edges, \
             --orphan-nodes, --duplicate-nodes, --duplicate-edges, --self-edges."
        );
    }

    let mut graph = ctx.load()?;
    let plan = gid_core::plan_repair(&graph, &opts);

    // JSON mode: print plan (and report if applied), no prompts.
    if json {
        if plan.is_empty() {
            println!("{}", serde_json::json!({
                "dry_run": dry_run,
                "applied": false,
                "plan_empty": true,
                "total_changes": 0,
            }));
            return Ok(());
        }
        if dry_run {
            println!("{}", serde_json::json!({
                "dry_run": true,
                "applied": false,
                "plan": {
                    "orphan_edges": plan.orphan_edges,
                    "orphan_nodes": plan.orphan_nodes,
                    "duplicate_node_ids": plan.duplicate_node_ids,
                    "duplicate_edges": plan.duplicate_edges,
                    "self_edges": plan.self_edges,
                    "skipped_unsafe_orphan_nodes": plan.skipped_unsafe_orphan_nodes,
                    "total_changes": plan.total_changes(),
                },
            }));
            return Ok(());
        }
        // JSON + apply: requires --yes (no prompting in JSON mode)
        if !yes {
            anyhow::bail!("JSON apply mode requires --yes (cannot prompt in JSON mode).");
        }
        let backup = if !no_backup {
            Some(backup_graph_file(ctx)?)
        } else {
            None
        };
        let report = gid_core::apply_repair(&mut graph, &plan);
        ctx.save(&graph)?;
        println!("{}", serde_json::json!({
            "dry_run": false,
            "applied": true,
            "backup": backup.as_ref().map(|p| p.display().to_string()),
            "report": {
                "orphan_edges_removed": report.orphan_edges_removed,
                "orphan_nodes_removed": report.orphan_nodes_removed,
                "duplicate_nodes_merged": report.duplicate_nodes_merged,
                "duplicate_edges_removed": report.duplicate_edges_removed,
                "self_edges_removed": report.self_edges_removed,
                "total": report.total(),
            },
        }));
        return Ok(());
    }

    // Human-readable mode.
    println!("{}", plan);

    if plan.is_empty() {
        return Ok(());
    }

    if dry_run {
        println!("\n(dry-run — no changes applied. Re-run without --dry-run to apply.)");
        return Ok(());
    }

    if !yes {
        print!("\nApply these changes? [y/N]: ");
        io::stdout().flush().ok();
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        let answer = line.trim().to_lowercase();
        if answer != "y" && answer != "yes" {
            println!("Aborted.");
            return Ok(());
        }
    }

    let backup = if !no_backup {
        let path = backup_graph_file(ctx)?;
        println!("✓ Backup written to {}", path.display());
        Some(path)
    } else {
        None
    };

    let report = gid_core::apply_repair(&mut graph, &plan);
    ctx.save(&graph)?;
    println!("\n{}", report);
    if backup.is_some() {
        println!("(restore from backup if anything looks wrong)");
    }
    Ok(())
}

/// Copy the active graph file to `<file>.backup-<timestamp>`.
///
/// For SQLite: copies `graph.db` (and `-wal` / `-shm` if present, just to be safe —
/// though after a clean save these are usually merged).
/// For YAML: copies `graph.yml`.
fn backup_graph_file(ctx: &GraphContext) -> Result<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let primary = match ctx.backend {
        StorageBackend::Sqlite => ctx.gid_dir.join("graph.db"),
        StorageBackend::Yaml => ctx.gid_dir.join("graph.yml"),
    };
    if !primary.exists() {
        anyhow::bail!(
            "Cannot back up: graph file does not exist at {}",
            primary.display()
        );
    }
    let backup_path = primary.with_extension(format!(
        "{}.backup-{}",
        primary
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("graph"),
        timestamp
    ));
    std::fs::copy(&primary, &backup_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to copy {} to {}: {}",
            primary.display(),
            backup_path.display(),
            e
        )
    })?;
    Ok(backup_path)
}

fn cmd_about_ctx(ctx: &GraphContext, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    
    // Get project metadata
    let project_name = graph.project.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Unnamed Project".to_string());
    let project_desc = graph.project.as_ref().and_then(|p| p.description.clone());
    
    // Count nodes by type
    let total_nodes = graph.nodes.len();
    let mut type_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for node in &graph.nodes {
        let node_type = node.node_type.as_deref().unwrap_or("unknown");
        *type_counts.entry(node_type.to_string()).or_default() += 1;
    }
    
    // Count nodes by status (for tasks)
    let todo_count = graph.nodes.iter().filter(|n| n.status == NodeStatus::Todo).count();
    let in_progress_count = graph.nodes.iter().filter(|n| n.status == NodeStatus::InProgress).count();
    let done_count = graph.nodes.iter().filter(|n| n.status == NodeStatus::Done).count();
    let blocked_count = graph.nodes.iter().filter(|n| n.status == NodeStatus::Blocked).count();
    
    // Count code entities by kind
    let file_count = graph.nodes.iter().filter(|n| n.node_kind.as_deref() == Some("File")).count();
    let function_count = graph.nodes.iter().filter(|n| matches!(n.node_kind.as_deref(), Some("Function") | Some("Constant"))).count();
    let class_count = graph.nodes.iter().filter(|n| matches!(n.node_kind.as_deref(), Some("Class") | Some("Interface") | Some("Enum") | Some("TypeAlias") | Some("Trait"))).count();
    
    // Count edges by relation
    let total_edges = graph.edges.len();
    let mut relation_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for edge in &graph.edges {
        *relation_counts.entry(edge.relation.clone()).or_default() += 1;
    }
    
    // Collect languages from node.lang field
    let mut lang_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for node in &graph.nodes {
        if let Some(ref lang) = node.lang {
            *lang_counts.entry(lang.clone()).or_default() += 1;
        }
    }
    let mut languages: Vec<(String, usize)> = lang_counts.into_iter().collect();
    languages.sort_by(|a, b| b.1.cmp(&a.1));
    
    // Collect features (nodes with type=feature)
    let features: Vec<&Node> = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("feature"))
        .collect();
    
    if json {
        let mut type_counts_vec: Vec<_> = type_counts.into_iter().collect();
        type_counts_vec.sort_by(|a, b| b.1.cmp(&a.1));
        
        let mut relation_counts_vec: Vec<_> = relation_counts.into_iter().collect();
        relation_counts_vec.sort_by(|a, b| b.1.cmp(&a.1));
        
        println!("{}", serde_json::json!({
            "project": {
                "name": project_name,
                "description": project_desc,
            },
            "nodes": {
                "total": total_nodes,
                "by_type": type_counts_vec.into_iter().map(|(k, v)| serde_json::json!({k: v})).collect::<Vec<_>>(),
                "by_status": {
                    "todo": todo_count,
                    "in_progress": in_progress_count,
                    "done": done_count,
                    "blocked": blocked_count,
                },
                "code": {
                    "files": file_count,
                    "functions": function_count,
                    "classes": class_count,
                },
            },
            "edges": {
                "total": total_edges,
                "by_relation": relation_counts_vec.into_iter().map(|(k, v)| serde_json::json!({k: v})).collect::<Vec<_>>(),
            },
            "languages": languages.iter().map(|(lang, count)| serde_json::json!({lang: count})).collect::<Vec<_>>(),
            "features": features.iter().map(|f| serde_json::json!({
                "id": f.id,
                "title": f.title,
            })).collect::<Vec<_>>(),
        }));
    } else {
        println!("📊 Project: {}", project_name);
        if let Some(desc) = project_desc {
            println!("   Description: {}", desc);
        }
        println!();
        
        // Node counts
        println!("📦 Nodes: {} total", total_nodes);
        
        // Count tasks (nodes with type=task)
        let task_count = type_counts.get("task").copied().unwrap_or(0);
        if task_count > 0 {
            println!("   Tasks: {} ({} todo, {} in_progress, {} done)", 
                task_count, todo_count, in_progress_count, done_count);
        }
        
        // Features
        if let Some(&feat_count) = type_counts.get("feature") {
            if feat_count > 0 {
                println!("   Features: {}", feat_count);
            }
        }
        
        // Components
        if let Some(&comp_count) = type_counts.get("component") {
            if comp_count > 0 {
                println!("   Components: {}", comp_count);
            }
        }
        
        // Code entities
        let total_code = file_count + function_count + class_count;
        if total_code > 0 {
            println!("   Code: {} ({} files, {} functions, {} classes)", 
                total_code, file_count, function_count, class_count);
        }
        
        println!();
        
        // Edge counts
        println!("🔗 Edges: {} total", total_edges);
        let mut sorted_relations: Vec<_> = relation_counts.iter().collect();
        sorted_relations.sort_by(|a, b| b.1.cmp(a.1));
        for (relation, count) in sorted_relations.iter().take(5) {
            println!("   {}: {}", relation, count);
        }
        
        println!();
        
        // Languages
        if !languages.is_empty() {
            let lang_list: Vec<String> = languages.iter()
                .take(5)
                .map(|(lang, count)| format!("{} ({})", lang, count))
                .collect();
            println!("🗣️  Languages: {}", lang_list.join(", "));
        }
        
        // Features list
        if !features.is_empty() {
            println!();
            println!("✨ Features:");
            for feat in &features {
                println!("   • {} — {}", feat.id, feat.title);
            }
        }
    }
    
    Ok(())
}

fn cmd_tasks_ctx(ctx: &GraphContext, status_filter: Option<String>, ready_only: bool, layer: LayerFilter, compact: bool, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let filtered = apply_layer_filter(&graph, layer);
    let tasks: Vec<&Node> = if ready_only {
        filtered.ready_tasks()
    } else if let Some(status_str) = &status_filter {
        let status: NodeStatus = status_str.parse()?;
        filtered.tasks_by_status(&status)
    } else {
        filtered.nodes.iter().collect()
    };
    if json {
        let tasks_json: Vec<_> = tasks.iter().map(|t| {
            serde_json::json!({
                "id": t.id,
                "title": t.title,
                "status": t.status.to_string(),
                "tags": t.tags,
                "description": t.description,
            })
        }).collect();
        let summary = filtered.summary();
        println!("{}", serde_json::json!({
            "tasks": tasks_json,
            "summary": {
                "total": summary.total_nodes,
                "code_nodes": summary.code_nodes,
                "todo": summary.todo,
                "in_progress": summary.in_progress,
                "done": summary.done,
                "blocked": summary.blocked,
                "ready": summary.ready,
            }
        }));
    } else if compact {
        // Compact: one line per task, minimal info
        for task in &tasks {
            println!("{} {:30} {}", status_icon(&task.status), task.id, task.title);
        }
        let summary = filtered.summary();
        println!("— {} tasks ({} todo, {} in_progress, {} done)", summary.total_nodes, summary.todo, summary.in_progress, summary.done);
    } else {
        if tasks.is_empty() {
            println!("No tasks found.");
        } else {
            for task in &tasks {
                let tags = if task.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", task.tags.join(", "))
                };
                println!("{} {} — {}{}", status_icon(&task.status), task.id, task.title, tags);
            }
        }
        let summary = filtered.summary();
        println!("\n{}", summary);
    }
    Ok(())
}

fn cmd_task_update_ctx(ctx: &GraphContext, id: &str, status_str: &str, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    let status: NodeStatus = status_str.parse()?;
    if !graph.update_status(id, status.clone()) {
        bail!("Node not found: {}", id);
    }
    ctx.save(&graph)?;
    if json {
        println!("{}", serde_json::json!({"success": true, "id": id, "status": status.to_string()}));
    } else {
        println!("✓ Updated {} to {}", id, status);
    }
    Ok(())
}

fn cmd_complete_ctx(ctx: &GraphContext, id: &str, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    if graph.get_node(id).is_none() {
        bail!("Node not found: {}", id);
    }
    let ready_before: HashSet<String> = graph.ready_tasks().iter().map(|n| n.id.clone()).collect();
    graph.update_status(id, NodeStatus::Done);
    ctx.save(&graph)?;
    let ready_after: HashSet<String> = graph.ready_tasks().iter().map(|n| n.id.clone()).collect();
    let newly_unblocked: Vec<&String> = ready_after.difference(&ready_before).collect();
    if json {
        println!("{}", serde_json::json!({"success": true, "id": id, "newly_unblocked": newly_unblocked}));
    } else {
        println!("✓ Completed: {}", id);
        if !newly_unblocked.is_empty() {
            println!("\n🔓 Newly unblocked tasks:");
            for task_id in newly_unblocked {
                if let Some(task) = graph.get_node(task_id) {
                    println!("   {} — {}", task.id, task.title);
                }
            }
        }
    }
    Ok(())
}

/// Inputs for `cmd_add_node_ctx` — packs the optional node attributes from
/// the `add-node` CLI subcommand.
struct AddNodeOpts {
    id: String,
    title: String,
    desc: Option<String>,
    status: Option<String>,
    tags: Option<String>,
    node_type: Option<String>,
}

fn cmd_add_node_ctx(
    ctx: &GraphContext,
    opts: AddNodeOpts,
    json: bool,
) -> Result<()> {
    let mut graph = ctx.load()?;
    if graph.get_node(&opts.id).is_some() {
        bail!("Node already exists: {}", opts.id);
    }
    let mut node = Node::new(&opts.id, &opts.title);
    if let Some(d) = opts.desc { node.description = Some(d); }
    if let Some(s) = opts.status { node.status = s.parse()?; }
    if let Some(t) = opts.tags {
        node.tags = t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    if let Some(nt) = opts.node_type { node.node_type = Some(nt); }
    graph.add_node(node);
    ctx.save(&graph)?;
    if json {
        println!("{}", serde_json::json!({"success": true, "id": opts.id}));
    } else {
        println!("✓ Added node: {} — {}", opts.id, opts.title);
    }
    Ok(())
}

fn cmd_remove_node_ctx(ctx: &GraphContext, id: &str, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    if graph.get_node(id).is_none() {
        bail!("Node not found: {}", id);
    }
    graph.remove_node(id);
    ctx.save(&graph)?;
    if json {
        println!("{}", serde_json::json!({"success": true, "id": id}));
    } else {
        println!("✓ Removed node: {}", id);
    }
    Ok(())
}

fn cmd_add_edge_ctx(ctx: &GraphContext, from: &str, to: &str, relation: &str, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    if graph.get_node(from).is_none() {
        bail!("Source node not found: {}", from);
    }
    if graph.get_node(to).is_none() {
        bail!("Target node not found: {}", to);
    }
    graph.add_edge(Edge::new(from, to, relation));
    ctx.save(&graph)?;
    if json {
        println!("{}", serde_json::json!({"success": true, "from": from, "to": to, "relation": relation}));
    } else {
        println!("✓ Added edge: {} —[{}]→ {}", from, relation, to);
    }
    Ok(())
}

fn cmd_remove_edge_ctx(ctx: &GraphContext, from: &str, to: &str, relation: Option<&str>, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    let before = graph.edges.len();
    if let Some(rel) = relation {
        graph.edges.retain(|e| !(e.from == from && e.to == to && e.relation == rel));
    } else {
        graph.edges.retain(|e| !(e.from == from && e.to == to));
    }
    let removed = before - graph.edges.len();
    if removed == 0 {
        bail!("No matching edge found");
    }
    ctx.save(&graph)?;
    if json {
        println!("{}", serde_json::json!({"success": true, "removed": removed}));
    } else {
        println!("✓ Removed {} edge(s): {} → {}", removed, from, to);
    }
    Ok(())
}

fn cmd_query_impact_ctx(ctx: &GraphContext, node: &str, relation: Option<&str>, layer: LayerFilter, type_filter: Option<&str>, min_confidence: f64, json: bool) -> Result<()> {
    // For SQLite backend, load graph into memory and use same logic as YAML
    match ctx.backend {
        StorageBackend::Yaml => cmd_query_impact(ctx, node, relation, layer, type_filter, min_confidence, json),
        _ => {
            let graph = ctx.load()?;
            let filtered = apply_layer_filter(&graph, layer);
            let engine = QueryEngine::new(&filtered);
            let rels: Option<Vec<&str>> = relation.map(|r| r.split(',').map(|s| s.trim()).collect());
            let result = engine.impact_with_filters(
                node,
                rels.as_deref(),
                Some(min_confidence),
            );
            let impacted: Vec<&Node> = if let Some(tf) = type_filter {
                result.nodes.into_iter().filter(|n| n.node_type.as_deref() == Some(tf)).collect()
            } else {
                result.nodes
            };
            if json {
                let nodes: Vec<_> = impacted.iter().map(|n| serde_json::json!({"id": n.id, "title": n.title})).collect();
                println!("{}", serde_json::json!({
                    "node": node,
                    "impacted": nodes,
                    "min_confidence": min_confidence,
                    "hidden_low_confidence": result.hidden_low_confidence,
                }));
            } else {
                if impacted.is_empty() {
                    println!("No nodes would be affected by changes to '{}'", node);
                } else {
                    println!("Changes to '{}' would affect {} node(s):", node, impacted.len());
                    for n in &impacted { println!("  {} — {}", n.id, n.title); }
                }
                if result.hidden_low_confidence > 0 {
                    println!(
                        "  ({} low-confidence edges hidden — pass `--min-confidence 0.0` to include.)",
                        result.hidden_low_confidence,
                    );
                }
            }
            Ok(())
        }
    }
}

/// Inputs for the `query deps` subcommand. Borrowed strings + plain values
/// to avoid forcing the caller to clone CLI args.
#[derive(Clone, Copy)]
struct QueryDepsOpts<'a> {
    node: &'a str,
    transitive: bool,
    relation: Option<&'a str>,
    layer: LayerFilter,
    type_filter: Option<&'a str>,
    min_confidence: f64,
    json: bool,
}

fn cmd_query_deps_ctx(ctx: &GraphContext, opts: QueryDepsOpts<'_>) -> Result<()> {
    match ctx.backend {
        StorageBackend::Yaml => cmd_query_deps(ctx, opts),
        _ => {
            let graph = ctx.load()?;
            let filtered = apply_layer_filter(&graph, opts.layer);
            let engine = QueryEngine::new(&filtered);
            let rels: Option<Vec<&str>> = opts.relation.map(|r| r.split(',').map(|s| s.trim()).collect());
            let result = engine.deps_with_filters(
                opts.node,
                opts.transitive,
                rels.as_deref(),
                Some(opts.min_confidence),
            );
            let deps: Vec<&Node> = if let Some(tf) = opts.type_filter {
                result.nodes.into_iter().filter(|n| n.node_type.as_deref() == Some(tf)).collect()
            } else {
                result.nodes
            };
            if opts.json {
                let nodes: Vec<_> = deps.iter().map(|n| serde_json::json!({"id": n.id, "title": n.title, "status": n.status.to_string()})).collect();
                println!("{}", serde_json::json!({
                    "node": opts.node,
                    "transitive": opts.transitive,
                    "dependencies": nodes,
                    "min_confidence": opts.min_confidence,
                    "hidden_low_confidence": result.hidden_low_confidence,
                }));
            } else {
                let label = if opts.transitive { "Transitive" } else { "Direct" };
                if deps.is_empty() {
                    println!("'{}' has no {} dependencies", opts.node, label.to_lowercase());
                } else {
                    println!("{} dependencies of '{}' ({}):", label, opts.node, deps.len());
                    for n in &deps { println!("  {} {} — {}", status_icon(&n.status), n.id, n.title); }
                }
                if result.hidden_low_confidence > 0 {
                    println!(
                        "  ({} low-confidence edges hidden — pass `--min-confidence 0.0` to include.)",
                        result.hidden_low_confidence,
                    );
                }
            }
            Ok(())
        }
    }
}

fn cmd_query_path_ctx(ctx: &GraphContext, from: &str, to: &str, json: bool) -> Result<()> {
    match ctx.backend {
        StorageBackend::Yaml => cmd_query_path(ctx, from, to, json),
        _ => {
            let graph = ctx.load()?;
            let engine = QueryEngine::new(&graph);
            let result = engine.path(from, to);
            if json {
                println!("{}", serde_json::json!({"from": from, "to": to, "path": result}));
            } else {
                match result {
                    Some(p) => {
                        println!("Path from '{}' to '{}' ({} hops):", from, to, p.len() - 1);
                        println!("  {}", p.join(" → "));
                    }
                    None => println!("No path found between '{}' and '{}'", from, to),
                }
            }
            Ok(())
        }
    }
}

fn cmd_query_common_ctx(ctx: &GraphContext, a: &str, b: &str, json: bool) -> Result<()> {
    match ctx.backend {
        StorageBackend::Yaml => cmd_query_common(ctx, a, b, json),
        _ => {
            let graph = ctx.load()?;
            let engine = QueryEngine::new(&graph);
            let common = engine.common_cause(a, b);
            if json {
                let ids: Vec<&str> = common.iter().map(|n| n.id.as_str()).collect();
                println!("{}", serde_json::json!({"a": a, "b": b, "common_ancestors": ids}));
            } else {
                if common.is_empty() {
                    println!("No common ancestors between {} and {}", a, b);
                } else {
                    println!("Common ancestors of {} and {}:", a, b);
                    for n in &common { println!("  {} — {}", n.id, n.title); }
                }
            }
            Ok(())
        }
    }
}

fn cmd_query_topo_ctx(ctx: &GraphContext, json: bool) -> Result<()> {
    match ctx.backend {
        StorageBackend::Yaml => cmd_query_topo(ctx, json),
        _ => {
            let graph = ctx.load()?;
            let engine = QueryEngine::new(&graph);
            match engine.topological_sort() {
                Ok(order) => {
                    if json {
                        println!("{}", serde_json::json!({"order": order}));
                    } else {
                        println!("Topological order ({} nodes):", order.len());
                        for (i, id) in order.iter().enumerate() {
                            if let Some(node) = graph.get_node(id) {
                                println!("  {}. {} — {}", i + 1, node.id, node.title);
                            }
                        }
                    }
                }
                Err(e) => {
                    if json { println!("{}", serde_json::json!({"error": e.to_string()})); }
                    else { println!("Error: {}", e); }
                }
            }
            Ok(())
        }
    }
}

fn cmd_edit_graph_ctx(ctx: &GraphContext, operations_json: &str, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    let ops: Vec<serde_json::Value> = serde_json::from_str(operations_json)
        .context("Invalid JSON array for operations")?;
    let mut added_nodes = 0;
    let mut added_edges = 0;
    let mut removed = 0;
    let mut updated = 0;
    for op in &ops {
        let op_type = op["op"].as_str().unwrap_or("");
        match op_type {
            "add_node" => {
                let id = op["id"].as_str().unwrap_or("");
                let title = op["title"].as_str().unwrap_or(id);
                if !id.is_empty() {
                    let mut node = Node::new(id, title);
                    if let Some(s) = op["status"].as_str() { node.status = s.parse().unwrap_or_default(); }
                    if let Some(d) = op["description"].as_str() { node.description = Some(d.to_string()); }
                    if let Some(t) = op["type"].as_str() { node.node_type = Some(t.to_string()); }
                    graph.add_node(node);
                    added_nodes += 1;
                }
            }
            "add_edge" => {
                let from = op["from"].as_str().unwrap_or("");
                let to = op["to"].as_str().unwrap_or("");
                let rel = op["relation"].as_str().unwrap_or("depends_on");
                if !from.is_empty() && !to.is_empty() {
                    graph.add_edge(Edge::new(from, to, rel));
                    added_edges += 1;
                }
            }
            "remove_node" => {
                let id = op["id"].as_str().unwrap_or("");
                if !id.is_empty() { graph.remove_node(id); removed += 1; }
            }
            "update_status" => {
                let id = op["id"].as_str().unwrap_or("");
                let status = op["status"].as_str().unwrap_or("");
                if !id.is_empty() && !status.is_empty() {
                    if let Ok(s) = status.parse() {
                        graph.update_status(id, s);
                        updated += 1;
                    }
                }
            }
            _ => {
                eprintln!("Warning: unknown operation '{}'", op_type);
            }
        }
    }
    ctx.save(&graph)?;
    if json {
        println!("{}", serde_json::json!({
            "success": true,
            "added_nodes": added_nodes,
            "added_edges": added_edges,
            "removed": removed,
            "updated": updated
        }));
    } else {
        println!("✓ Applied {} operations: {} nodes added, {} edges added, {} removed, {} updated",
            ops.len(), added_nodes, added_edges, removed, updated);
    }
    Ok(())
}



/// Inputs for `cmd_context_ctx` — packs the parameters from the `context`
/// CLI subcommand (target nodes, traversal limits, output formatting).
struct ContextOpts {
    targets: Vec<String>,
    max_tokens: usize,
    depth: u32,
    include: Vec<String>,
    format: String,
    project_root: Option<PathBuf>,
    json_flag: bool,
}

/// Handle `gid context` — assemble context for target nodes. **[GOAL-4.9, 4.12]**
fn cmd_context_ctx(
    ctx: &GraphContext,
    opts: ContextOpts,
) -> Result<()> {
    let ContextOpts {
        targets,
        max_tokens,
        depth,
        include,
        format,
        project_root,
        json_flag,
    } = opts;
    use gid_core::harness::{
        ContextQuery, ContextFilters, OutputFormat, assemble_context, format_context,
    };

    let graph = ctx.load()?;

    // Parse output format — --json flag overrides --format.
    let output_format = if json_flag {
        OutputFormat::Json
    } else {
        format.parse::<OutputFormat>()
            .map_err(|e| anyhow::anyhow!("{}", e))?
    };

    // Resolve project root: explicit flag > walk-up from .gid dir > cwd.
    let resolved_root = project_root
        .or_else(|| {
            // .gid is usually inside the project root.
            ctx.gid_dir.parent().map(|p| p.to_path_buf())
        })
        .or_else(|| std::env::current_dir().ok());

    let query = ContextQuery {
        targets,
        token_budget: max_tokens,
        depth,
        filters: ContextFilters {
            include_patterns: include,
            ..Default::default()
        },
        format: output_format,
        project_root: resolved_root,
    };

    let assembled = assemble_context(&graph, &query)?;

    // Output the result.
    let output = format_context(&assembled, output_format);
    println!("{}", output);

    // Log stats to stderr (GOAL-4.13) — in addition to the tracing::info inside assemble_context.
    eprintln!(
        "context: {} visited, {} included, {} filtered, {}/{} tokens, {}ms",
        assembled.stats.nodes_visited,
        assembled.stats.nodes_included,
        assembled.stats.nodes_excluded_by_filter,
        assembled.stats.budget_used,
        assembled.stats.budget_total,
        assembled.stats.elapsed_ms,
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Original Commands (updated for --json)
// ═══════════════════════════════════════════════════════════════════════════════

fn cmd_init(name: Option<String>, desc: Option<String>, json: bool) -> Result<()> {
    let path = PathBuf::from(".gid/graph.yml");
    if path.exists() {
        bail!("Graph file already exists: {}", path.display());
    }

    let project_name = name.unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "project".to_string())
    });

    let graph = Graph {
        project: Some(gid_core::ProjectMeta {
            name: project_name.clone(),
            description: desc,
        }),
        nodes: Vec::new(),
        edges: Vec::new(),
    };

    save_graph(&graph, &path)?;
    
    if json {
        println!("{}", serde_json::json!({
            "success": true,
            "path": path.display().to_string(),
            "project": project_name
        }));
    } else {
        println!("✓ Created {}", path.display());
        println!("  Project: {}", project_name);
    }
    Ok(())
}



















/// Resolve a node reference, handling disambiguation.
/// Returns Ok(node_id) for unambiguous match, Err for no match or ambiguous.
fn resolve_or_disambiguate(graph: &Graph, query: &str, json: bool) -> Result<String> {
    let matches = graph.resolve_node(query);
    match matches.len() {
        0 => bail!("No node found matching '{}'", query),
        1 => Ok(matches[0].id.clone()),
        n => {
            if json {
                let match_list: Vec<_> = matches.iter().map(|m| {
                    serde_json::json!({"id": &m.id, "title": &m.title, "type": m.node_type.as_deref()})
                }).collect();
                println!("{}", serde_json::json!({"ambiguous": true, "query": query, "matches": match_list}));
                bail!("Ambiguous query '{}' — {} matches", query, n);
            } else {
                eprintln!("Ambiguous query \"{}\" — {} matches:", query, n);
                for (i, m) in matches.iter().enumerate() {
                    eprintln!("  {}. {} ({})", i + 1, m.id, m.node_type.as_deref().unwrap_or("?"));
                }
                eprintln!("Use a more specific query or pass the exact ID.");
                bail!("Ambiguous query");
            }
        }
    }
}

/// Resolve a node reference with layer-filtered graph, falling back to full graph.
/// Reports if the node was found in a filtered-out layer.
fn resolve_with_layer_fallback(filtered: &Graph, graph: &Graph, query: &str, layer: LayerFilter, json: bool) -> Result<String> {
    match resolve_or_disambiguate(filtered, query, json) {
        Ok(id) => Ok(id),
        Err(_) => {
            // Try full graph
            if let Ok(id) = resolve_or_disambiguate(graph, query, json) {
                let layer_name = graph.get_node(&id)
                    .and_then(|n| n.source.as_deref())
                    .unwrap_or("unknown");
                bail!("Node '{}' exists but is not in the '{}' layer (found in '{}' layer)", id, match layer {
                    LayerFilter::Code => "code",
                    LayerFilter::Project => "project",
                    LayerFilter::All => "all",
                }, layer_name);
            } else {
                bail!("No node found matching '{}'", query);
            }
        }
    }
}

fn cmd_query_impact(ctx: &GraphContext, node: &str, relation: Option<&str>, layer: LayerFilter, type_filter: Option<&str>, min_confidence: f64, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let filtered = apply_layer_filter(&graph, layer);

    let resolved_id = resolve_with_layer_fallback(&filtered, &graph, node, layer, json)?;
    let node = &resolved_id;

    let engine = QueryEngine::new(&filtered);
    let rels: Option<Vec<&str>> = relation.map(|r| r.split(',').map(|s| s.trim()).collect());
    let result = engine.impact_with_filters(node, rels.as_deref(), Some(min_confidence));

    // Apply type filter
    let impacted: Vec<&Node> = if let Some(tf) = type_filter {
        result.nodes.into_iter().filter(|n| n.node_type.as_deref() == Some(tf)).collect()
    } else {
        result.nodes
    };

    if json {
        let nodes: Vec<_> = impacted.iter().map(|n| serde_json::json!({"id": n.id, "title": n.title})).collect();
        println!("{}", serde_json::json!({
            "node": node,
            "relation_filter": relation,
            "layer": format!("{:?}", layer),
            "type_filter": type_filter,
            "impacted": nodes,
            "min_confidence": min_confidence,
            "hidden_low_confidence": result.hidden_low_confidence,
        }));
    } else {
        if impacted.is_empty() {
            println!("No nodes would be affected by changes to '{}'", node);
        } else {
            let filter_note = relation.map(|r| format!(" (relations: {})", r)).unwrap_or_default();
            println!("Changes to '{}' would affect {} node(s){}:", node, impacted.len(), filter_note);
            for n in &impacted {
                println!("  {} — {}", n.id, n.title);
            }
        }
        if result.hidden_low_confidence > 0 {
            println!(
                "  ({} low-confidence edges hidden — pass `--min-confidence 0.0` to include.)",
                result.hidden_low_confidence,
            );
        }
    }
    Ok(())
}

fn cmd_query_deps(ctx: &GraphContext, opts: QueryDepsOpts<'_>) -> Result<()> {
    let graph = ctx.load()?;
    let filtered = apply_layer_filter(&graph, opts.layer);

    let resolved_id = resolve_with_layer_fallback(&filtered, &graph, opts.node, opts.layer, opts.json)?;
    let node = &resolved_id;

    let engine = QueryEngine::new(&filtered);
    let rels: Option<Vec<&str>> = opts.relation.map(|r| r.split(',').map(|s| s.trim()).collect());
    let result = engine.deps_with_filters(node, opts.transitive, rels.as_deref(), Some(opts.min_confidence));

    // Apply type filter
    let deps: Vec<&Node> = if let Some(tf) = opts.type_filter {
        result.nodes.into_iter().filter(|n| n.node_type.as_deref() == Some(tf)).collect()
    } else {
        result.nodes
    };

    if opts.json {
        let nodes: Vec<_> = deps.iter().map(|n| serde_json::json!({
            "id": n.id, "title": n.title, "status": n.status.to_string()
        })).collect();
        println!("{}", serde_json::json!({
            "node": node,
            "transitive": opts.transitive,
            "relation_filter": opts.relation,
            "layer": format!("{:?}", opts.layer),
            "type_filter": opts.type_filter,
            "dependencies": nodes,
            "min_confidence": opts.min_confidence,
            "hidden_low_confidence": result.hidden_low_confidence,
        }));
    } else {
        let label = if opts.transitive { "Transitive" } else { "Direct" };
        let filter_note = opts.relation.map(|r| format!(" (relations: {})", r)).unwrap_or_default();
        if deps.is_empty() {
            println!("'{}' has no {} dependencies{}", node, label.to_lowercase(), filter_note);
        } else {
            println!("{} dependencies of '{}' ({}){}:", label, node, deps.len(), filter_note);
            for n in &deps {
                println!("  {} {} — {}", status_icon(&n.status), n.id, n.title);
            }
        }
        if result.hidden_low_confidence > 0 {
            println!(
                "  ({} low-confidence edges hidden — pass `--min-confidence 0.0` to include.)",
                result.hidden_low_confidence,
            );
        }
    }
    Ok(())
}

fn cmd_query_path(ctx: &GraphContext, from: &str, to: &str, json: bool) -> Result<()> {
    let graph = ctx.load()?;

    let resolved_from = resolve_or_disambiguate(&graph, from, json)?;
    let from = &resolved_from;
    let resolved_to = resolve_or_disambiguate(&graph, to, json)?;
    let to = &resolved_to;

    let engine = QueryEngine::new(&graph);
    let result = engine.path(from, to);
    
    if json {
        println!("{}", serde_json::json!({"from": from, "to": to, "path": result}));
    } else {
        match result {
            Some(p) => {
                println!("Path from '{}' to '{}' ({} hops):", from, to, p.len() - 1);
                println!("  {}", p.join(" → "));
            }
            None => {
                println!("No path found between '{}' and '{}'", from, to);
            }
        }
    }
    Ok(())
}

fn cmd_query_common(ctx: &GraphContext, a: &str, b: &str, json: bool) -> Result<()> {
    let graph = ctx.load()?;

    let resolved_a = resolve_or_disambiguate(&graph, a, json)?;
    let a = &resolved_a;
    let resolved_b = resolve_or_disambiguate(&graph, b, json)?;
    let b = &resolved_b;

    let engine = QueryEngine::new(&graph);
    let common = engine.common_cause(a, b);

    if json {
        let nodes: Vec<_> = common.iter().map(|n| serde_json::json!({"id": n.id, "title": n.title})).collect();
        println!("{}", serde_json::json!({"a": a, "b": b, "common": nodes}));
    } else {
        if common.is_empty() {
            println!("'{}' and '{}' have no common dependencies", a, b);
        } else {
            println!("Common dependencies of '{}' and '{}' ({}):", a, b, common.len());
            for n in common {
                println!("  {} — {}", n.id, n.title);
            }
        }
    }
    Ok(())
}

fn cmd_query_topo(ctx: &GraphContext, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let engine = QueryEngine::new(&graph);

    match engine.topological_sort() {
        Ok(order) => {
            if json {
                println!("{}", serde_json::json!({"order": order}));
            } else {
                println!("Topological order ({} nodes):", order.len());
                for (i, id) in order.iter().enumerate() {
                    if let Some(node) = graph.get_node(id) {
                        println!("  {}. {} — {}", i + 1, id, node.title);
                    } else {
                        println!("  {}. {}", i + 1, id);
                    }
                }
            }
        }
        Err(e) => {
            if json {
                println!("{}", serde_json::json!({"error": e.to_string()}));
            } else {
                println!("Cannot produce topological order: {}", e);
            }
            std::process::exit(1);
        }
    }
    Ok(())
}



/// Inputs for `cmd_extract` — packs the parameters from the `extract` CLI
/// subcommand (target directory, formatting, LSP/force/semantify toggles,
/// and the optional graph/backend overrides).
struct ExtractOpts<'a> {
    dir: &'a PathBuf,
    format: &'a str,
    output: Option<&'a std::path::Path>,
    json_flag: bool,
    lsp: bool,
    force: bool,
    no_semantify: bool,
    graph_override: Option<&'a PathBuf>,
    backend_arg: Option<String>,
}

fn cmd_extract(opts: ExtractOpts<'_>) -> Result<()> {
    let ExtractOpts {
        dir,
        format,
        output,
        json_flag,
        lsp,
        force,
        no_semantify,
        graph_override,
        backend_arg,
    } = opts;
    let dir = if dir.is_absolute() {
        dir.clone()
    } else {
        std::env::current_dir()?.join(dir)
    };
    
    if !dir.exists() {
        bail!("Directory not found: {}", dir.display());
    }

    // Resolve .gid/ directory: --graph flag > walk up from extract dir > walk up from cwd > cwd/.gid/
    let gid_dir = if let Some(graph_path) = graph_override {
        // Explicit --graph: use its parent as .gid/
        graph_path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".gid"))
    } else {
        // Walk up from extract dir to find existing .gid/
        find_graph_file_walk_up(&dir)
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            // Then try walking up from cwd
            .or_else(|| {
                std::env::current_dir().ok()
                    .and_then(|cwd| find_graph_file_walk_up(&cwd))
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            })
            // Fall back to cwd/.gid/
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(".gid")
            })
    };
    let meta_path = gid_dir.join("extract-meta.json");

    // Resolve storage backend (explicit flag > auto-detect from .gid/ contents)
    let explicit_backend = match backend_arg {
        Some(ref s) => Some(s.parse::<StorageBackend>()
            .map_err(|e| anyhow::anyhow!("{}", e))?),
        None => None,
    };
    let backend = gid_core::storage::resolve_backend(explicit_backend, &gid_dir);

    if !json_flag {
        if force {
            eprintln!("Extracting code graph from {} (full rebuild)...", dir.display());
        } else {
            eprintln!("Extracting code graph from {} (incremental)...", dir.display());
        }
    }

    let (mut code_graph, report) = CodeGraph::extract_incremental(&dir, &gid_dir, &meta_path, force)?;

    if !json_flag {
        eprintln!("{}", report);
    }

    // LSP refinement pass
    if lsp {
        if !json_flag {
            eprintln!("Refining call edges with LSP...");
        }
        match code_graph.refine_with_lsp(&dir) {
            Ok(stats) => {
                if !json_flag {
                    eprintln!(
                        "LSP refinement: {} refined, {} removed, {} failed, {} skipped, {} refinement_skipped (languages: {})",
                        stats.refined,
                        stats.removed,
                        stats.failed,
                        stats.skipped,
                        stats.refinement_skipped,
                        if stats.languages_used.is_empty() {
                            "none".to_string()
                        } else {
                            stats.languages_used.join(", ")
                        }
                    );
                    if stats.references_queried > 0 || stats.implementations_queried > 0 {
                        eprintln!(
                            "LSP enrichment: {} references queried → {} new call edges, {} implementations queried → {} new impl edges",
                            stats.references_queried,
                            stats.references_edges_added,
                            stats.implementations_queried,
                            stats.implementation_edges_added,
                        );
                    }
                    if !stats.missing_servers.is_empty() {
                        eprintln!();
                        eprintln!("⚠️  Missing LSP servers ({} language{}):", 
                            stats.missing_servers.len(),
                            if stats.missing_servers.len() > 1 { "s" } else { "" }
                        );
                        for m in &stats.missing_servers {
                            eprintln!("   • {} — {} files, {} call edges unrefined", m.language_id, m.file_count, m.edge_count);
                            eprintln!("     Install: {}", m.install_command);
                        }
                        eprintln!();
                    }
                }
            }
            Err(e) => {
                if !json_flag {
                    eprintln!("LSP refinement failed: {}, using tree-sitter edges only", e);
                }
            }
        }
    }
    
    // Convert CodeGraph to graph nodes/edges
    let (code_nodes, code_edges) = codegraph_to_graph_nodes(&code_graph, &dir);

    // Load existing graph (preserves project tasks)
    let mut graph = load_graph_auto(&gid_dir, Some(backend)).unwrap_or_default();

    // Merge code layer (replaces old extract nodes, preserves project nodes)
    let code_node_count = code_nodes.len();
    let code_edge_count = code_edges.len();
    merge_code_layer(&mut graph, code_nodes, code_edges);

    // Save graph via resolved backend
    save_graph_auto(&graph, &gid_dir, Some(backend))
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if !json_flag {
        let project_count = graph.project_nodes().len();
        let backend_label = match backend {
            StorageBackend::Sqlite => "graph.db",
            StorageBackend::Yaml => "graph.yml",
        };
        eprintln!("✓ Wrote {} code nodes + {} code edges to {} ({} project nodes preserved)",
            code_node_count, code_edge_count, backend_label, project_count);
    }

    // Auto-semantify: assign architectural layers to code nodes
    if !no_semantify {
        let assigned = apply_heuristic_layers(&mut graph);

        // Generate bridge edges between project and code nodes
        gid_core::unify::generate_bridge_edges(&mut graph);

        let bridge_count = graph.bridge_edges().len();

        // Re-save graph with semantified results
        save_graph_auto(&graph, &gid_dir, Some(backend))
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        if !json_flag {
            eprintln!("✓ Auto-semantify: assigned layers to {} nodes, generated {} bridge edges",
                assigned, bridge_count);
        }
    }

    // Build output string for display / --output
    let output_str = match format {
        "yaml" | "yml" => serde_yaml::to_string(&graph)?,
        "json" => serde_json::to_string_pretty(&graph)?,
        "summary" => {
            if json_flag {
                serde_json::to_string_pretty(&graph)?
            } else {
                // Count from merged graph
                let file_count = graph.nodes.iter()
                    .filter(|n| n.node_kind.as_deref() == Some("File") && n.source.as_deref() == Some("extract"))
                    .count();
                let class_count = graph.nodes.iter()
                    .filter(|n| matches!(n.node_kind.as_deref(), Some("Class") | Some("Interface") | Some("Enum") | Some("TypeAlias") | Some("Trait")) && n.source.as_deref() == Some("extract"))
                    .count();
                let func_count = graph.nodes.iter()
                    .filter(|n| matches!(n.node_kind.as_deref(), Some("Function") | Some("Constant")) && n.source.as_deref() == Some("extract"))
                    .count();
                let task_count = graph.project_nodes().len();

                // Count edges by relation
                let import_count = graph.edges.iter()
                    .filter(|e| e.relation == "imports")
                    .count();
                let call_count = graph.edges.iter()
                    .filter(|e| e.relation == "calls")
                    .count();

                let mut s = format!(
                    "Code Graph Summary\n{}\n\n",
                    "=".repeat(50)
                );
                s.push_str(&format!("📊 {} files, {} classes/structs, {} functions\n",
                    file_count, class_count, func_count));
                if task_count > 0 {
                    s.push_str(&format!("📋 {} task nodes (preserved from existing graph)\n", task_count));
                }
                s.push_str(&format!("🔗 {} edges ({} imports, {} calls)\n\n",
                    graph.edges.len(), import_count, call_count));

                // Count entities per file using file_path field
                let mut file_entities: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                for node in &graph.nodes {
                    if let Some(ref fp) = node.file_path {
                        if node.node_kind.as_deref() != Some("File") && node.source.as_deref() == Some("extract") {
                            *file_entities.entry(fp.clone()).or_default() += 1;
                        }
                    }
                }
                let mut files: Vec<_> = file_entities.into_iter().collect();
                files.sort_by(|a, b| b.1.cmp(&a.1));

                s.push_str("Top files by entity count:\n");
                for (file, count) in files.iter().take(10) {
                    s.push_str(&format!("  📄 {} ({} entities)\n", file, count));
                }

                if files.len() > 10 {
                    s.push_str(&format!("  ... and {} more files\n", files.len() - 10));
                }

                s
            }
        }
        _ => {
            anyhow::bail!("Unknown format: {} (expected: yaml, json, or summary)", format);
        }
    };

    if let Some(out_path) = output {
        std::fs::write(out_path, &output_str)?;
        if !json_flag {
            println!("✓ Wrote unified graph to {}", out_path.display());
        }
    } else {
        print!("{}", output_str);
    }

    Ok(())
}

fn cmd_analyze(file: &PathBuf, show_callers: bool, show_callees: bool, show_impact: bool, json_flag: bool) -> Result<()> {
    let project_root = find_project_root(file)?;
    
    if !json_flag {
        eprintln!("Analyzing {} (project root: {})...", file.display(), project_root.display());
    }
    let graph = load_code_graph(&project_root);
    
    let abs_file = if file.is_absolute() {
        file.clone()
    } else {
        std::env::current_dir()?.join(file)
    };
    
    let rel_path = abs_file.strip_prefix(&project_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| file.to_string_lossy().to_string());
    
    let file_nodes: Vec<&CodeNode> = graph.nodes.iter()
        .filter(|n| n.file_path == rel_path && n.kind != NodeKind::File)
        .collect();
    
    if json_flag {
        let mut result = serde_json::json!({
            "file": rel_path,
            "entities": file_nodes.len(),
            "nodes": file_nodes.iter().map(|n| serde_json::json!({
                "id": n.id,
                "name": n.name,
                "kind": format!("{:?}", n.kind),
                "line": n.line,
            })).collect::<Vec<_>>()
        });
        
        if show_callers {
            let callers_map: std::collections::HashMap<_, _> = file_nodes.iter().map(|n| {
                let callers = graph.get_callers(&n.id);
                (n.id.clone(), callers.iter().map(|c| serde_json::json!({
                    "name": c.name,
                    "file": c.file_path
                })).collect::<Vec<_>>())
            }).collect();
            result["callers"] = serde_json::json!(callers_map);
        }
        
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    
    if file_nodes.is_empty() {
        println!("No code entities found in {}", rel_path);
        return Ok(());
    }
    
    println!("📄 {} — {} entities\n", rel_path, file_nodes.len());
    
    for node in &file_nodes {
        let icon = match node.kind {
            NodeKind::Class => "🔷",
            NodeKind::Function => "🔹",
            _ => "📦",
        };
        let line_info = node.line.map(|l| format!(":L{}", l)).unwrap_or_default();
        println!("{} {}{}", icon, node.name, line_info);
        
        if show_callers {
            let callers = graph.get_callers(&node.id);
            if !callers.is_empty() {
                println!("  ↑ Callers ({}):", callers.len());
                for caller in callers.iter().take(5) {
                    println!("    {} ({})", caller.name, caller.file_path);
                }
                if callers.len() > 5 {
                    println!("    ... and {} more", callers.len() - 5);
                }
            }
        }
        
        if show_callees {
            let callees = graph.get_callees(&node.id);
            if !callees.is_empty() {
                println!("  ↓ Callees ({}):", callees.len());
                for callee in callees.iter().take(5) {
                    println!("    {} ({})", callee.name, callee.file_path);
                }
                if callees.len() > 5 {
                    println!("    ... and {} more", callees.len() - 5);
                }
            }
        }
        
        println!();
    }
    
    if show_impact {
        println!("\n{}\n", "=".repeat(50));
        // Convert CodeGraph to Graph for impact analysis
        let (code_nodes, code_edges) = codegraph_to_graph_nodes(&graph, &project_root);
        let mut unified = gid_core::graph::Graph::new();
        unified.nodes = code_nodes;
        unified.edges = code_edges;
        // ISS-035: surface only high-confidence edges by default; the
        // hidden count is reported in `analysis.summary`.
        let analysis = analyze_impact_with_filters(
            std::slice::from_ref(&rel_path),
            &unified,
            None,
            Some(gid_core::DEFAULT_MIN_CONFIDENCE),
        );
        print!("{}", format_impact_for_llm(&analysis));
    }
    
    Ok(())
}

fn find_project_root(file: &PathBuf) -> Result<PathBuf> {
    let abs_file = if file.is_absolute() {
        file.clone()
    } else {
        std::env::current_dir()?.join(file)
    };
    
    let start = abs_file.parent().unwrap_or(&abs_file);
    
    let mut current = start;
    loop {
        if current.join(".git").exists() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    
    let markers = ["Cargo.toml", "package.json", "pyproject.toml", "setup.py"];
    current = start;
    let mut found = None;
    
    loop {
        for marker in &markers {
            if current.join(marker).exists() {
                found = Some(current.to_path_buf());
            }
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    
    found.or_else(|| std::env::current_dir().ok()).context("Could not find project root")
}

// ═══════════════════════════════════════════════════════════════════════════════
// New Command Implementations
// ═══════════════════════════════════════════════════════════════════════════════

fn cmd_history_list(ctx: &GraphContext, json: bool) -> Result<()> {
    let mgr = HistoryManager::new(&ctx.gid_dir);
    let entries = mgr.list_snapshots()?;
    
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        if entries.is_empty() {
            println!("No history entries found.");
            println!("Run `gid history save` to create a snapshot.");
        } else {
            println!("\n📜 Graph History");
            println!("{}", "═".repeat(60));
            for (i, entry) in entries.iter().enumerate() {
                let latest = if i == 0 { " (latest)" } else { "" };
                println!("\n  {}{}", entry.filename, latest);
                println!("    {} | {} nodes, {} edges", 
                    entry.timestamp, entry.node_count, entry.edge_count);
                if let Some(ref msg) = entry.message {
                    println!("    Message: {}", msg);
                }
            }
            println!("\nUse `gid history diff <version>` to compare.");
            println!("Use `gid history restore <version>` to restore.");
        }
    }
    Ok(())
}

fn cmd_history_save(ctx: &GraphContext, message: Option<&str>, json: bool) -> Result<()> {
    let graph = load_graph(&ctx.graph_yml)?;
    let mgr = HistoryManager::new(&ctx.gid_dir);
    let filename = mgr.save_snapshot(&graph, message)?;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "filename": filename}));
    } else {
        println!("✓ Saved snapshot: {}", filename);
    }
    Ok(())
}

fn cmd_history_diff(ctx: &GraphContext, version: &str, version2: Option<&str>, json: bool) -> Result<()> {
    let mgr = HistoryManager::new(&ctx.gid_dir);
    
    let diff = if let Some(v2) = version2 {
        // Diff two historical versions against each other
        let d = mgr.diff_versions(version, v2)?;
        if !json {
            println!("\n📊 Comparing {} → {}\n", version, v2);
        }
        d
    } else {
        // Diff historical version against current graph
        let current = load_graph(&ctx.graph_yml)?;
        let d = mgr.diff_against(version, &current)?;
        if !json {
            println!("\n📊 Comparing {} → current\n", version);
        }
        d
    };
    
    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
    } else {
        println!("{}", diff);
    }
    Ok(())
}

fn cmd_history_restore(ctx: &GraphContext, version: &str, force: bool, json: bool) -> Result<()> {
    if !force && !json {
        println!("Warning: This will overwrite the current graph.");
        println!("Use --force to confirm.");
        return Ok(());
    }
    
    let mgr = HistoryManager::new(&ctx.gid_dir);
    mgr.restore(version, &ctx.gid_dir, Some(ctx.backend))?;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "restored": version}));
    } else {
        println!("✓ Restored graph from {}", version);
    }
    Ok(())
}

fn cmd_visual(ctx: &GraphContext, format: &str, output: Option<&std::path::Path>, layer: LayerFilter, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let filtered = apply_layer_filter(&graph, layer);
    let fmt: VisualFormat = format.parse()?;
    let result = render(&filtered, fmt);
    
    if let Some(out_path) = output {
        std::fs::write(out_path, &result)?;
        if json {
            println!("{}", serde_json::json!({"success": true, "output": out_path.display().to_string()}));
        } else {
            println!("✓ Wrote visualization to {}", out_path.display());
        }
    } else {
        if json && fmt == VisualFormat::Ascii {
            println!("{}", serde_json::json!({"format": format, "output": result}));
        } else {
            print!("{}", result);
        }
    }
    Ok(())
}

fn cmd_advise(ctx: &GraphContext, errors_only: bool, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let mut result = advise_analyze(&graph);
    
    if errors_only {
        result.items.retain(|a| a.severity == gid_core::Severity::Error);
    }
    
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", result);
    }
    
    if !result.passed {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_design(ctx: Option<&GraphContext>, requirements: Option<String>, parse: bool, merge: bool, scope: Option<String>, dry_run: bool, json: bool) -> Result<()> {
    if parse {
        // Read LLM response from stdin and parse it
        let mut response = String::new();
        io::stdin().read_to_string(&mut response)?;
        
        let incoming = parse_llm_response(&response)?;
        
        if merge {
            // Merge mode: requires a graph context
            let ctx = ctx.ok_or_else(|| anyhow::anyhow!("Graph context required for merge mode"))?;
            let mut graph = ctx.load()?;
            
            if let Some(ref feature_id) = scope {
                // Scoped merge: replace tasks under a specific feature
                if dry_run {
                    // Count old tasks (implements edges to feature_id)
                    let old_task_count = graph.edges.iter()
                        .filter(|e| e.to == *feature_id && e.relation == "implements")
                        .count();
                    let new_node_count = incoming.nodes.len();
                    let new_edge_count = incoming.edges.len();
                    
                    if json {
                        println!("{}", serde_json::json!({
                            "dry_run": true,
                            "feature_id": feature_id,
                            "old_tasks": old_task_count,
                            "new_nodes": new_node_count,
                            "new_edges": new_edge_count,
                        }));
                    } else {
                        println!("Dry run — merge preview for scope '{}'", feature_id);
                        println!("  Old tasks to remove: {}", old_task_count);
                        println!("  New nodes to add:    {}", new_node_count);
                        println!("  New edges to add:    {}", new_edge_count);
                        println!("\nRun without --dry-run to apply.");
                    }
                    return Ok(());
                }
                
                let (removed, added) = graph.merge_feature_nodes(feature_id, incoming);
                ctx.save(&graph)?;
                
                if json {
                    println!("{}", serde_json::json!({
                        "success": true,
                        "path": ctx.graph_yml.display().to_string(),
                        "feature_id": feature_id,
                        "removed": removed,
                        "added": added,
                    }));
                } else {
                    println!("✓ Merged into '{}': removed {} old tasks, added {} new nodes", feature_id, removed, added);
                    println!("  Saved to {}", ctx.graph_yml.display());
                }
            } else {
                // Unscoped merge: add nodes/edges if not already present
                let mut nodes_added = 0usize;
                let mut edges_added = 0usize;
                
                for node in incoming.nodes {
                    if graph.get_node(&node.id).is_none() {
                        graph.add_node(node);
                        nodes_added += 1;
                    }
                }
                for edge in incoming.edges {
                    if graph.add_edge_dedup(edge) {
                        edges_added += 1;
                    }
                }
                
                ctx.save(&graph)?;
                
                if json {
                    println!("{}", serde_json::json!({
                        "success": true,
                        "path": ctx.graph_yml.display().to_string(),
                        "nodes_added": nodes_added,
                        "edges_added": edges_added,
                    }));
                } else {
                    println!("✓ Merged: added {} nodes, {} edges", nodes_added, edges_added);
                    println!("  Saved to {}", ctx.graph_yml.display());
                }
            }
        } else {
            // Overwrite mode (original behavior)
            if let Some(ctx) = ctx {
                // Save to graph via context
                ctx.save(&incoming)?;
                if json {
                    println!("{}", serde_json::json!({"success": true, "path": ctx.graph_yml.display().to_string()}));
                } else {
                    println!("✓ Saved graph to {}", ctx.graph_yml.display());
                }
            } else {
                // Output as YAML
                if json {
                    println!("{}", serde_json::to_string_pretty(&incoming)?);
                } else {
                    println!("{}", serde_yaml::to_string(&incoming)?);
                }
            }
        }
    } else {
        // Generate prompt
        let reqs = match requirements {
            Some(r) => r,
            None => {
                let mut s = String::new();
                eprintln!("Enter requirements (Ctrl+D to finish):");
                io::stdin().read_to_string(&mut s)?;
                s
            }
        };
        
        let prompt = generate_graph_prompt(&reqs);
        
        if json {
            println!("{}", serde_json::json!({"prompt": prompt}));
        } else {
            println!("{}", prompt);
        }
    }
    Ok(())
}

fn cmd_semantify(ctx: &GraphContext, heuristic: bool, parse: bool, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    
    if heuristic {
        // Apply heuristic layer assignments
        let assigned = apply_heuristic_layers(&mut graph);
        ctx.save(&graph)?;
        
        if json {
            println!("{}", serde_json::json!({"success": true, "assigned": assigned}));
        } else {
            println!("✓ Assigned layers to {} nodes using heuristics", assigned);
        }
    } else if parse {
        // Parse LLM response from stdin
        let mut response = String::new();
        io::stdin().read_to_string(&mut response)?;
        
        let result = gid_core::parse_semantify_response(&response)?;
        let applied = gid_core::apply_proposals(&mut graph, &result.proposals);
        ctx.save(&graph)?;
        
        if json {
            println!("{}", serde_json::json!({"success": true, "applied": applied}));
        } else {
            println!("✓ Applied {} semantic upgrades", applied);
        }
    } else {
        // Generate prompt
        let prompt = generate_semantify_prompt(&graph);
        
        if json {
            println!("{}", serde_json::json!({"prompt": prompt}));
        } else {
            println!("{}", prompt);
        }
    }
    Ok(())
}

fn cmd_refactor_rename(ctx: &GraphContext, old: &str, new: &str, apply: bool, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    
    if let Some(preview) = preview_rename(&graph, old, new) {
        if apply {
            if apply_rename(&mut graph, old, new) {
                ctx.save(&graph)?;
                if json {
                    println!("{}", serde_json::json!({"success": true, "renamed": {"from": old, "to": new}}));
                } else {
                    println!("✓ Renamed {} to {}", old, new);
                }
            } else {
                bail!("Failed to apply rename");
            }
        } else {
            if json {
                println!("{}", serde_json::to_string_pretty(&preview)?);
            } else {
                println!("{}", preview);
                println!("\nUse --apply to execute these changes.");
            }
        }
    } else {
        bail!("Node not found: {}", old);
    }
    Ok(())
}

fn cmd_refactor_merge(ctx: &GraphContext, a: &str, b: &str, new_id: &str, apply: bool, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    
    if let Some(preview) = preview_merge(&graph, a, b, new_id) {
        if apply {
            if apply_merge(&mut graph, a, b, new_id) {
                ctx.save(&graph)?;
                if json {
                    println!("{}", serde_json::json!({"success": true, "merged": {"a": a, "b": b, "new_id": new_id}}));
                } else {
                    println!("✓ Merged {} and {} into {}", a, b, new_id);
                }
            } else {
                bail!("Failed to apply merge");
            }
        } else {
            if json {
                println!("{}", serde_json::to_string_pretty(&preview)?);
            } else {
                println!("{}", preview);
                println!("\nUse --apply to execute these changes.");
            }
        }
    } else {
        bail!("One or both nodes not found: {}, {}", a, b);
    }
    Ok(())
}

fn cmd_refactor_split(ctx: &GraphContext, node: &str, into: &[String], apply: bool, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    
    let splits: Vec<SplitDefinition> = into.iter().map(|id| SplitDefinition {
        id: id.clone(),
        title: id.clone(),
        description: None,
        tags: vec![],
    }).collect();
    
    if let Some(preview) = preview_split(&graph, node, &splits) {
        if apply {
            let created = apply_split(&mut graph, node, &splits);
            if !created.is_empty() {
                ctx.save(&graph)?;
                if json {
                    println!("{}", serde_json::json!({"success": true, "created": created}));
                } else {
                    println!("✓ Split {} into: {}", node, created.join(", "));
                }
            } else {
                bail!("Failed to apply split");
            }
        } else {
            if json {
                println!("{}", serde_json::to_string_pretty(&preview)?);
            } else {
                println!("{}", preview);
                println!("\nUse --apply to execute these changes.");
            }
        }
    } else {
        bail!("Node not found: {}", node);
    }
    Ok(())
}

fn cmd_refactor_extract(ctx: &GraphContext, nodes: &[String], parent: &str, title: &str, apply: bool, json: bool) -> Result<()> {
    let mut graph = ctx.load()?;
    
    if let Some(preview) = preview_extract(&graph, nodes, parent, title) {
        if apply {
            if apply_extract(&mut graph, nodes, parent, title) {
                ctx.save(&graph)?;
                if json {
                    println!("{}", serde_json::json!({"success": true, "parent": parent, "extracted": nodes}));
                } else {
                    println!("✓ Extracted {} nodes into '{}'", nodes.len(), parent);
                }
            } else {
                bail!("Failed to apply extract");
            }
        } else {
            if json {
                println!("{}", serde_json::to_string_pretty(&preview)?);
            } else {
                println!("{}", preview);
                println!("\nUse --apply to execute these changes.");
            }
        }
    } else {
        bail!("One or more nodes not found");
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Task Harness Commands
// ═══════════════════════════════════════════════════════════════════════════════

fn cmd_plan(ctx: &GraphContext, format: &str, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let plan = create_plan(&graph)?;

    // If --json flag or format is json, output as JSON
    if json || format == "json" {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }

    // Human-readable text format
    println!();
    println!("Execution Plan");
    println!("══════════════════════════════════════════════════════════");
    println!();
    println!("Total tasks: {}", plan.total_tasks);
    println!("Estimated total turns: {}", plan.estimated_total_turns);

    if !plan.critical_path.is_empty() {
        println!(
            "Critical path: {} ({} tasks)",
            plan.critical_path.join(" → "),
            plan.critical_path.len()
        );
    }

    for layer in &plan.layers {
        println!();
        let parallel = if layer.tasks.len() > 1 { ", parallel" } else { "" };
        let task_word = if layer.tasks.len() == 1 { "task" } else { "tasks" };
        println!(
            "Layer {} ({} {}{}):",
            layer.index,
            layer.tasks.len(),
            task_word,
            parallel
        );
        for task in &layer.tasks {
            let turns = format!(" [{} turns]", task.estimated_turns);
            let desc = if task.description.is_empty() {
                task.title.clone()
            } else {
                // Take first line of description or title
                task.description.lines().next().unwrap_or(&task.title).to_string()
            };
            println!("  ○ {}{} — {}", task.id, turns, desc);
        }
        if let Some(ref checkpoint) = layer.checkpoint {
            println!("  ✓ Checkpoint: {}", checkpoint);
        }
    }

    println!();
    Ok(())
}

fn cmd_execute(
    ctx: &GraphContext,
    max_concurrent: Option<usize>,
    model: Option<String>,
    approval_mode: Option<String>,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let graph = ctx.load()?;
    let plan = create_plan(&graph)?;

    // Load config from .gid/execution.yml (or defaults)
    let gid_dir = &ctx.gid_dir;
    let execution_yml = gid_dir.join("execution.yml");
    let mut config = load_config(
        None,
        Some(&execution_yml),
        None,
    ).unwrap_or_default();

    // Override config from CLI args
    if let Some(mc) = max_concurrent {
        config.max_concurrent = mc;
    }
    if let Some(m) = model {
        config.model = m;
    }
    if let Some(am) = approval_mode {
        config.approval_mode = match am.to_lowercase().as_str() {
            "auto" => gid_core::harness::types::ApprovalMode::Auto,
            "manual" => gid_core::harness::types::ApprovalMode::Manual,
            _ => gid_core::harness::types::ApprovalMode::Mixed,
        };
    }

    if dry_run {
        if json {
            println!("{}", serde_json::json!({
                "dry_run": true,
                "plan": serde_json::to_value(&plan)?,
                "config": {
                    "max_concurrent": config.max_concurrent,
                    "model": config.model,
                    "approval_mode": format!("{:?}", config.approval_mode),
                }
            }));
        } else {
            println!("Dry run — showing plan without executing\n");
            println!("Configuration:");
            println!("  Max concurrent: {}", config.max_concurrent);
            println!("  Model: {}", config.model);
            println!("  Approval mode: {:?}", config.approval_mode);
            println!();

            // Reuse plan display
            cmd_plan(ctx, "text", false)?;
        }
        return Ok(());
    }

    // Run full execution via gid-harness
    if !json {
        println!("Starting execution...");
        println!("✓ Loaded graph: {} tasks in {} layers\n", plan.total_tasks, plan.layers.len());
    }

    // Set up executor and worktree manager
    let project_dir = gid_dir.parent().unwrap_or(std::path::Path::new("."));
    let worktree_mgr = gid_core::harness::GitWorktreeManager::new(project_dir.to_path_buf());
    let executor = gid_core::harness::CliExecutor::new();

    // Run async execution
    let rt = tokio::runtime::Runtime::new()?;
    let mut graph_mut = graph;
    let result = rt.block_on(gid_core::harness::execute_plan(
        &plan,
        &mut graph_mut,
        &config,
        &executor,
        &worktree_mgr,
        gid_dir,
    ));

    // Save updated graph state
    ctx.save(&graph_mut)?;

    match result {
        Ok(exec_result) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&exec_result)?);
            } else {
                println!("\nExecution complete!");
                println!("✓ {} tasks completed, {} failed",
                    exec_result.tasks_completed, exec_result.tasks_failed);
                println!("  Total turns: {}", exec_result.total_turns);
                println!("  Total tokens: {}K", exec_result.total_tokens / 1000);
                println!("  Duration: {}s", exec_result.duration_secs);
            }
        }
        Err(e) => {
            // Save graph even on error
            let _ = ctx.save(&graph_mut);
            if json {
                println!("{}", serde_json::json!({"error": e.to_string()}));
            } else {
                eprintln!("✗ Execution failed: {}", e);
            }
            std::process::exit(1);
        }
    }
    Ok(())
}

fn cmd_stats(ctx: &GraphContext, json: bool) -> Result<()> {
    let gid_dir = &ctx.gid_dir;

    // Per-layer graph breakdown
    if let Ok(graph) = ctx.load() {
        let project_node_count = graph.project_nodes().len();
        let project_edge_count = graph.project_edges().len();
        let code_node_count = graph.code_nodes().len();
        let code_edge_count = graph.code_edges().len();
        let bridge_count = graph.bridge_edges().len();
        let total_nodes = graph.nodes.len();
        let total_edges = graph.edges.len();

        if json {
            // Graph stats will be merged into the JSON output below
        } else {
            println!();
            println!("Graph Breakdown");
            println!("══════════════════════════════════════════════════════════");
            println!();
            println!("Project layer:  {} nodes, {} edges", project_node_count, project_edge_count);
            println!("Code layer:     {} nodes, {} edges", code_node_count, code_edge_count);
            println!("Bridge edges:   {}", bridge_count);
            println!("Total:          {} nodes, {} edges", total_nodes, total_edges);
            println!();
        }
    }

    let log_path = gid_dir.join("execution-log.jsonl");

    if !log_path.exists() {
        if json {
            println!("{}", serde_json::json!({"error": "No execution log found", "path": log_path.display().to_string()}));
        } else {
            println!("No execution log found at {}", log_path.display());
            println!("Run `gid execute` to generate execution telemetry.");
        }
        return Ok(());
    }

    let content = std::fs::read_to_string(&log_path)?;
    let events: Vec<ExecutionEvent> = content.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    if events.is_empty() {
        if json {
            println!("{}", serde_json::json!({"error": "Execution log is empty"}));
        } else {
            println!("Execution log is empty.");
        }
        return Ok(());
    }

    // Compute stats from events
    let mut tasks_completed: usize = 0;
    let mut tasks_failed: usize = 0;
    let mut total_turns: u32 = 0;
    let mut total_tokens: u64 = 0;
    let mut duration_secs: u64 = 0;

    for event in &events {
        match event {
            ExecutionEvent::TaskDone { turns, tokens, .. } => {
                tasks_completed += 1;
                total_turns += turns;
                total_tokens += tokens;
            }
            ExecutionEvent::TaskFailed { turns, .. } => {
                tasks_failed += 1;
                total_turns += turns;
            }
            ExecutionEvent::Complete { duration_s, .. } => {
                duration_secs = *duration_s;
            }
            _ => {}
        }
    }

    let avg_turns = if tasks_completed > 0 {
        total_turns as f32 / tasks_completed as f32
    } else {
        0.0
    };

    let stats = ExecutionStats {
        tasks_completed,
        tasks_failed,
        total_turns,
        avg_turns_per_task: avg_turns,
        total_tokens,
        duration_secs,
    };

    if json {
        let mut stats_json = serde_json::to_value(&stats)?;
        // Merge graph breakdown if available
        if let Ok(graph) = ctx.load() {
            stats_json["graph"] = serde_json::json!({
                "project_nodes": graph.project_nodes().len(),
                "project_edges": graph.project_edges().len(),
                "code_nodes": graph.code_nodes().len(),
                "code_edges": graph.code_edges().len(),
                "bridge_edges": graph.bridge_edges().len(),
                "total_nodes": graph.nodes.len(),
                "total_edges": graph.edges.len(),
            });
        }
        println!("{}", serde_json::to_string_pretty(&stats_json)?);
    } else {
        println!();
        println!("Execution Statistics");
        println!("══════════════════════════════════════════════════════════");
        println!();
        println!("Tasks completed: {}", stats.tasks_completed);
        println!("Tasks failed:    {}", stats.tasks_failed);
        println!("Total turns:     {}", stats.total_turns);
        println!("Avg turns/task:  {:.1}", stats.avg_turns_per_task);

        let tokens_display = if stats.total_tokens > 1_000_000 {
            format!("{:.1}M", stats.total_tokens as f64 / 1_000_000.0)
        } else if stats.total_tokens > 1_000 {
            format!("{:.1}K", stats.total_tokens as f64 / 1_000.0)
        } else {
            format!("{}", stats.total_tokens)
        };
        println!("Total tokens:    {}", tokens_display);

        if stats.duration_secs > 0 {
            let mins = stats.duration_secs / 60;
            let secs = stats.duration_secs % 60;
            if mins > 0 {
                println!("Duration:        {}m {}s", mins, secs);
            } else {
                println!("Duration:        {}s", secs);
            }
        }
        println!();
    }
    Ok(())
}

fn cmd_approve(ctx: &GraphContext, json: bool) -> Result<()> {
    let gid_dir = &ctx.gid_dir;
    let mut state = ExecutionState::load(gid_dir)?;

    match state.status {
        ExecutionStatus::WaitingApproval => {
            let approved = state.approve();
            state.save(gid_dir)?;

            if json {
                let approvals: Vec<_> = approved.iter().map(|a| {
                    serde_json::json!({
                        "layer_index": a.layer_index,
                        "message": a.message,
                        "requested_at": a.requested_at.to_rfc3339()
                    })
                }).collect();
                println!("{}", serde_json::json!({
                    "success": true,
                    "approved": approvals,
                    "status": state.status.to_string()
                }));
            } else {
                println!("✓ Approved {} pending request(s)", approved.len());
                for a in &approved {
                    println!("  Layer {}: {}", a.layer_index, a.message);
                }
                println!("\nStatus is now: {}", state.status);
                println!("Run `gid execute` to continue.");
            }
        }
        _ => {
            if json {
                println!("{}", serde_json::json!({
                    "success": false,
                    "error": "No pending approvals",
                    "status": state.status.to_string()
                }));
            } else {
                println!("No pending approvals.");
                println!("Current status: {}", state.status);
            }
        }
    }

    Ok(())
}

fn cmd_stop(ctx: &GraphContext, json: bool) -> Result<()> {
    let gid_dir = &ctx.gid_dir;
    let mut state = ExecutionState::load(gid_dir)?;

    match state.status {
        ExecutionStatus::Running | ExecutionStatus::WaitingApproval => {
            state.request_cancel();
            state.save(gid_dir)?;

            if json {
                println!("{}", serde_json::json!({
                    "success": true,
                    "cancel_requested": true,
                    "active_tasks": state.active_tasks
                }));
            } else {
                println!("✓ Cancellation requested");
                if !state.active_tasks.is_empty() {
                    println!("  Active tasks will complete their current work:");
                    for task in &state.active_tasks {
                        println!("    - {}", task);
                    }
                }
                println!("\nThe scheduler will stop gracefully at the next layer boundary.");
                println!("In-progress tasks will be reset to 'todo' (not 'failed').");
            }
        }
        ExecutionStatus::Idle => {
            if json {
                println!("{}", serde_json::json!({
                    "success": false,
                    "error": "No execution in progress",
                    "status": state.status.to_string()
                }));
            } else {
                println!("No execution in progress.");
            }
        }
        _ => {
            if json {
                println!("{}", serde_json::json!({
                    "success": false,
                    "error": format!("Cannot stop: execution is {}", state.status),
                    "status": state.status.to_string()
                }));
            } else {
                println!("Cannot stop: execution status is '{}'", state.status);
            }
        }
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn resolve_dir(dir: &PathBuf) -> Result<PathBuf> {
    let d = if dir.is_absolute() {
        dir.clone()
    } else {
        std::env::current_dir()?.join(dir)
    };
    if !d.exists() {
        bail!("Directory not found: {}", d.display());
    }
    Ok(d)
}

/// Load a CodeGraph from graph.yml code layer if available, otherwise fall back to extraction.
///
/// This is the migration bridge: commands that still use CodeGraph APIs can call this
/// instead of `CodeGraph::extract_from_dir()`. When graph.yml has code nodes, we reconstruct
/// a CodeGraph from them (O(n) in-memory conversion, no disk parsing). When graph.yml is
/// missing or has no code nodes, we fall back to the expensive `extract_from_dir()`.
fn load_code_graph(dir: &std::path::Path) -> CodeGraph {
    // Try loading from graph.yml first
    let graph_path = dir.join(".gid/graph.yml");
    if graph_path.exists() {
        if let Ok(graph) = gid_core::parser::load_graph(&graph_path) {
            if !graph.code_nodes().is_empty() {
                return graph_to_codegraph(&graph);
            }
        }
    }
    // Fallback: extract from source
    CodeGraph::extract_from_dir(dir)
}

fn cmd_code_search(dir: &PathBuf, keywords_str: &str, format_llm: Option<usize>, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let graph = load_code_graph(&dir);
    let keywords: Vec<&str> = keywords_str.split(',').map(|s| s.trim()).collect();

    if let Some(max_chars) = format_llm {
        let output = graph.format_for_llm(&keywords, max_chars);
        if json {
            println!("{}", serde_json::json!({"formatted": output}));
        } else {
            print!("{}", output);
        }
    } else {
        let nodes = graph.find_relevant_nodes(&keywords);
        if json {
            let items: Vec<_> = nodes.iter().map(|n| serde_json::json!({
                "id": n.id, "name": n.name, "kind": format!("{:?}", n.kind),
                "file": n.file_path, "line": n.line,
            })).collect();
            println!("{}", serde_json::to_string_pretty(&items)?);
        } else {
            if nodes.is_empty() {
                println!("No relevant nodes found for: {}", keywords_str);
            } else {
                println!("Found {} relevant nodes:\n", nodes.len());
                for n in nodes.iter().take(50) {
                    let icon = match n.kind {
                        NodeKind::File | NodeKind::Module => "📄",
                        NodeKind::Class | NodeKind::Interface | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Trait => "🔷",
                        NodeKind::Function | NodeKind::Constant => "🔹",
                    };
                    let line = n.line.map(|l| format!(":L{}", l)).unwrap_or_default();
                    println!("  {} {} ({}{})", icon, n.name, n.file_path, line);
                }
                if nodes.len() > 50 {
                    println!("  ... and {} more", nodes.len() - 50);
                }
            }
        }
    }
    Ok(())
}

fn cmd_code_failures(dir: &PathBuf, changed_str: &str, p2p: Option<&str>, f2p: Option<&str>, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let graph = load_code_graph(&dir);
    let changed: Vec<&str> = changed_str.split(',').map(|s| s.trim()).collect();
    let p2p_tests: Vec<String> = p2p.unwrap_or("").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let f2p_tests: Vec<String> = f2p.unwrap_or("").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

    let analysis = graph.trace_causal_chains(&changed, &p2p_tests, &f2p_tests);

    if json {
        println!("{}", serde_json::json!({"analysis": analysis}));
    } else {
        if analysis.is_empty() {
            println!("No test failures to analyze.");
        } else {
            print!("{}", analysis);
        }
    }
    Ok(())
}

fn cmd_code_symptoms(dir: &PathBuf, problem: &str, tests: &str, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let graph = load_code_graph(&dir);
    let nodes = graph.find_symptom_nodes(problem, tests);

    if json {
        let items: Vec<_> = nodes.iter().map(|n| serde_json::json!({
            "id": n.id, "name": n.name, "kind": format!("{:?}", n.kind),
            "file": n.file_path, "line": n.line, "is_test": n.is_test,
        })).collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        if nodes.is_empty() {
            println!("No symptom nodes found.");
        } else {
            println!("Found {} symptom nodes:\n", nodes.len());
            for n in &nodes {
                let icon = if n.is_test { "🧪" } else { match n.kind {
                    NodeKind::Class | NodeKind::Interface | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Trait => "🔷",
                    NodeKind::Function | NodeKind::Constant => "🔹",
                    NodeKind::File | NodeKind::Module => "📄",
                }};
                let line = n.line.map(|l| format!(":L{}", l)).unwrap_or_default();
                println!("  {} {} ({}{})", icon, n.name, n.file_path, line);
            }
        }
    }
    Ok(())
}

fn cmd_code_trace(dir: &PathBuf, symptoms_str: &str, depth: usize, max_chains: usize, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let graph = load_code_graph(&dir);
    let symptom_ids: Vec<&str> = symptoms_str.split(',').map(|s| s.trim()).collect();
    let chains = graph.trace_causal_chains_from_symptoms(&symptom_ids, depth, max_chains);

    if json {
        let items: Vec<_> = chains.iter().map(|c| serde_json::json!({
            "symptom": c.symptom_node_id,
            "chain": c.chain.iter().map(|n| serde_json::json!({
                "node_id": n.node_id, "name": n.node_name,
                "file": n.file_path, "line": n.line,
                "edge": n.edge_to_next,
            })).collect::<Vec<_>>(),
        })).collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        if chains.is_empty() {
            println!("No causal chains found.");
        } else {
            println!("Found {} causal chains:\n", chains.len());
            for (i, chain) in chains.iter().enumerate() {
                println!("Chain {} (from {}):", i + 1, chain.symptom_node_id);
                for (j, node) in chain.chain.iter().enumerate() {
                    let arrow = if let Some(ref edge) = node.edge_to_next {
                        format!(" --[{}]-->", edge)
                    } else {
                        String::new()
                    };
                    let line = node.line.map(|l| format!(":L{}", l)).unwrap_or_default();
                    println!("  {}. {} ({}{}){}", j + 1, node.node_name, node.file_path, line, arrow);
                }
                println!();
            }
        }
    }
    Ok(())
}

fn cmd_code_complexity(dir: &PathBuf, nodes_str: &str, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let graph = load_code_graph(&dir);
    let keywords: Vec<&str> = nodes_str.split(',').map(|s| s.trim()).collect();
    let node_ids: Vec<&str> = keywords.clone();

    let report = assess_complexity_from_graph(&graph, &keywords, 0);
    let risk = assess_risk_level(&graph, &node_ids);

    if json {
        println!("{}", serde_json::json!({
            "complexity": format!("{:?}", report.complexity),
            "relevant_nodes": report.relevant_nodes,
            "relevant_files": report.relevant_files,
            "risk_level": format!("{:?}", risk),
            "summary": report.summary,
        }));
    } else {
        println!("Complexity: {:?}", report.complexity);
        println!("Risk level: {:?}", risk);
        println!("Relevant: {} nodes across {} files", report.relevant_nodes, report.relevant_files);
        println!("\n{}", report.summary);
    }
    Ok(())
}

fn cmd_code_impact(dir: &PathBuf, files_str: &str, relation: Option<&str>, min_confidence: f64, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let code_graph = load_code_graph(&dir);
    let files: Vec<String> = files_str.split(',').map(|s| s.trim().to_string()).collect();

    // Convert CodeGraph to Graph for impact analysis
    let (code_nodes, code_edges) = codegraph_to_graph_nodes(&code_graph, &dir);
    let mut graph = gid_core::graph::Graph::new();
    graph.nodes = code_nodes;
    graph.edges = code_edges;

    let rels: Option<Vec<&str>> = relation.map(|r| r.split(',').map(|s| s.trim()).collect());
    let analysis = gid_core::analyze_impact_with_filters(
        &files,
        &graph,
        rels.as_deref(),
        Some(min_confidence),
    );
    let formatted = format_impact_for_llm(&analysis);

    if json {
        println!("{}", serde_json::json!({
            "files_changed": files,
            "relation_filter": relation,
            "risk_level": format!("{:?}", analysis.risk_level),
            "affected_source": analysis.affected_source.len(),
            "affected_tests": analysis.affected_tests.len(),
            "min_confidence": min_confidence,
            "hidden_low_confidence": analysis.hidden_low_confidence,
            "formatted": formatted,
        }));
    } else {
        print!("{}", formatted);
    }
    Ok(())
}

fn cmd_code_snippets(dir: &PathBuf, keywords_str: &str, max_lines: usize, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let graph = load_code_graph(&dir);
    let keywords: Vec<&str> = keywords_str.split(',').map(|s| s.trim()).collect();
    let relevant = graph.find_relevant_nodes(&keywords);
    let snippets = graph.extract_snippets(&relevant, &dir, max_lines);

    if json {
        println!("{}", serde_json::to_string_pretty(&snippets)?);
    } else {
        if snippets.is_empty() {
            println!("No snippets found for: {}", keywords_str);
        } else {
            for (node_id, snippet) in &snippets {
                let name = graph.node_by_id(node_id).map(|n| n.name.as_str()).unwrap_or(node_id);
                println!("━━━ {} ━━━", name);
                println!("{}", snippet);
                println!();
            }
        }
    }
    Ok(())
}

fn cmd_schema(dir: &PathBuf, json: bool) -> Result<()> {
    let dir = if dir.is_absolute() {
        dir.clone()
    } else {
        std::env::current_dir()?.join(dir)
    };
    if !dir.exists() {
        bail!("Directory not found: {}", dir.display());
    }
    let graph = load_code_graph(&dir);
    let schema = graph.get_schema();
    if json {
        println!("{}", serde_json::json!({"schema": schema}));
    } else {
        print!("{}", schema);
    }
    Ok(())
}

fn cmd_file_summary(dir: &PathBuf, file: &str, json: bool) -> Result<()> {
    let dir = if dir.is_absolute() {
        dir.clone()
    } else {
        std::env::current_dir()?.join(dir)
    };
    if !dir.exists() {
        bail!("Directory not found: {}", dir.display());
    }
    let graph = load_code_graph(&dir);
    let summary = graph.get_file_summary(file);
    if json {
        println!("{}", serde_json::json!({"file": file, "summary": summary}));
    } else {
        print!("{}", summary);
    }
    Ok(())
}

fn status_icon(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Todo => "○",
        NodeStatus::InProgress => "◐",
        NodeStatus::Done => "●",
        NodeStatus::Blocked => "✗",
        NodeStatus::Cancelled => "⊘",
        NodeStatus::Failed => "✘",
        NodeStatus::NeedsResolution => "⚠",
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Ritual Commands
// ═══════════════════════════════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════════════════════════════
// Migration command
// ═══════════════════════════════════════════════════════════════════════════════

fn cmd_migrate(
    graph_path: PathBuf,
    source: Option<PathBuf>,
    target: Option<PathBuf>,
    force: bool,
    no_validate: bool,
    verbose: bool,
    json: bool,
) -> Result<()> {
    use gid_core::storage::migration::{
        migrate, MigrationConfig, MigrationStatus, ValidationLevel,
    };

    // Derive paths from graph_path (.gid/graph.yml)
    let gid_dir = graph_path.parent().unwrap_or(Path::new(".gid"));
    let source_path = source.unwrap_or_else(|| graph_path.clone());
    let target_path = target.unwrap_or_else(|| gid_dir.join("graph.db"));
    let backup_dir = gid_dir.join("backups");

    let config = MigrationConfig {
        source_path: source_path.clone(),
        target_path: target_path.clone(),
        backup_dir: Some(backup_dir),
        validation_level: if no_validate {
            ValidationLevel::None
        } else {
            ValidationLevel::Strict
        },
        force,
        verbose,
    };

    if !json {
        println!("Migrating: {} → {}", source_path.display(), target_path.display());
        if force {
            println!("  (--force: will overwrite existing DB)");
        }
    }

    match migrate(&config) {
        Ok(report) => {
            if json {
                let obj = serde_json::json!({
                    "status": format!("{:?}", report.status),
                    "nodes_migrated": report.nodes_migrated,
                    "edges_migrated": report.edges_migrated,
                    "knowledge_migrated": report.knowledge_migrated,
                    "tags_migrated": report.tags_migrated,
                    "metadata_migrated": report.metadata_migrated,
                    "warnings": report.warnings.len(),
                    "duration_ms": report.duration.as_millis(),
                    "backup_path": report.backup_path.as_ref().map(|p| p.display().to_string()),
                    "source_fingerprint": report.source_fingerprint,
                });
                println!("{}", serde_json::to_string_pretty(&obj)?);
            } else {
                let status_str = match report.status {
                    MigrationStatus::Success => "✅ Success",
                    MigrationStatus::SuccessWithWarnings => "⚠️  Success (with warnings)",
                    MigrationStatus::Failed => "❌ Failed",
                };
                println!("\n{status_str}");
                println!("  Nodes:      {}", report.nodes_migrated);
                println!("  Edges:      {}", report.edges_migrated);
                println!("  Knowledge:  {}", report.knowledge_migrated);
                println!("  Tags:       {}", report.tags_migrated);
                println!("  Metadata:   {}", report.metadata_migrated);
                println!("  Duration:   {:?}", report.duration);
                if let Some(ref backup) = report.backup_path {
                    println!("  Backup:     {}", backup.display());
                }
                println!("  Fingerprint: {}", &report.source_fingerprint[..16]);

                if !report.warnings.is_empty() {
                    println!("\n  Warnings ({}):", report.warnings.len());
                    let max_show = if verbose { report.warnings.len() } else { 10 };
                    for (i, w) in report.warnings.iter().take(max_show).enumerate() {
                        println!("    {}. {w}", i + 1);
                    }
                    if report.warnings.len() > max_show {
                        println!("    ... and {} more (use --verbose to see all)", report.warnings.len() - max_show);
                    }
                }
            }
            Ok(())
        }
        Err(e) => {
            if json {
                let obj = serde_json::json!({
                    "error": e.to_string(),
                });
                println!("{}", serde_json::to_string_pretty(&obj)?);
                std::process::exit(1);
            } else {
                eprintln!("❌ Migration failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn cmd_ritual_init(project_root: &std::path::Path, template_name: &str, json: bool) -> Result<()> {
    use gid_core::ritual::TemplateRegistry;
    
    let ritual_path = project_root.join(".gid/ritual.yml");
    if ritual_path.exists() {
        bail!("Ritual already exists: {}", ritual_path.display());
    }
    
    // Load template
    let registry = TemplateRegistry::for_project(project_root);
    let template = registry.load(template_name)
        .with_context(|| format!("Template not found: {}", template_name))?;
    
    // Write ritual.yml
    std::fs::create_dir_all(project_root.join(".gid"))?;
    let yaml = serde_yaml::to_string(&template)?;
    std::fs::write(&ritual_path, &yaml)?;
    
    if json {
        println!("{}", serde_json::json!({
            "success": true,
            "path": ritual_path.display().to_string(),
            "template": template_name,
            "phases": template.phases.len()
        }));
    } else {
        println!("✓ Created {} from template '{}'", ritual_path.display(), template_name);
        println!("  {} phases defined", template.phases.len());
        println!("\n  Phases:");
        for (i, phase) in template.phases.iter().enumerate() {
            println!("    {}. {} ({})", i, phase.id, phase_kind_name(&phase.kind));
        }
        println!("\n  Run `gid ritual run` to start the ritual.");
    }
    Ok(())
}

async fn cmd_ritual_run(
    project_root: &std::path::Path,
    auto_approve: bool,
    template: Option<String>,
    model_override: Option<String>,
    json: bool,
) -> Result<()> {
    use gid_core::ritual::{RitualDefinition, RitualEngine, RitualStatus, PhaseStatus, RitualNotifier};
    use std::time::Instant;

    let ritual_path = project_root.join(".gid/ritual.yml");
    let state_path = project_root.join(".gid/ritual-state.json");

    // If --template is provided and no ritual exists, init first
    if let Some(ref tmpl) = template {
        if !ritual_path.exists() {
            if !json {
                println!("Initializing ritual from template: {}", tmpl);
            }
            cmd_ritual_init(project_root, tmpl, json)?;
        }
    }

    if !ritual_path.exists() {
        bail!("No ritual found. Run `gid ritual init` first or use `--template`.");
    }

    // Load ritual definition
    let mut template_dirs = vec![project_root.join(".gid/rituals/")];
    if let Some(home) = dirs::home_dir() {
        template_dirs.push(home.join(".gid/rituals/"));
    }

    let definition = RitualDefinition::load(&ritual_path, &template_dirs)?;

    // Create LLM client — prefer API (agentctl-auth) over CLI fallback
    let llm_client = if let Some(api_client) = gid_core::ritual::ApiLlmClient::try_from_pool() {
        eprintln!("Using agentctl-auth API client");
        api_client.into_arc()
    } else {
        eprintln!("No auth pool found, falling back to claude CLI");
        llm_client::CliLlmClient::new().into_arc()
    };

    // Check if we're resuming
    let resuming = state_path.exists();

    // Create or resume engine with LLM client
    let mut engine = if resuming {
        let engine = RitualEngine::resume_with_llm_client(definition.clone(), project_root, Some(llm_client))?;
        if !json {
            println!("▶ Resuming ritual: {} (from phase {})", 
                engine.definition().name,
                engine.state().current_phase + 1
            );
        }
        engine
    } else {
        let engine = RitualEngine::with_llm_client(definition.clone(), project_root, Some(llm_client))?;
        if !json {
            println!("▶ Running ritual: {}", engine.definition().name);
        }
        engine
    };

    // Set up notifier if configured via env vars
    if let Some(notifier) = RitualNotifier::from_env() {
        engine.set_notifier(notifier);
    }

    let total_phases = engine.definition().phases.len();
    if !json {
        println!();
    }

    // Run the ritual with progress output
    loop {
        let current_phase = engine.state().current_phase;
        
        // Show progress before running if not completed
        if current_phase < total_phases && !json {
            let phase = &engine.definition().phases[current_phase];
            let model = model_override.as_deref()
                .or(phase.model.as_deref())
                .unwrap_or(&engine.definition().config.default_model);
            let kind_name = phase_kind_name(&phase.kind);
            println!("  [{}/{}] {} ({}, {})", 
                current_phase + 1, 
                total_phases,
                phase.id,
                kind_name,
                model
            );
        }

        let phase_start = Instant::now();
        let status = engine.run().await?;
        let phase_duration = phase_start.elapsed();

        match &status {
            RitualStatus::WaitingApproval { phase_id, message, .. } => {
                if auto_approve {
                    if !json {
                        println!("  ⏩ Auto-approving phase: {}", phase_id);
                    }
                    engine.approve().await?;
                    continue;
                }

                // Interactive approval prompt
                if json {
                    println!("{}", serde_json::json!({
                        "status": "waiting_approval",
                        "phase_id": phase_id,
                        "message": message
                    }));
                    return Ok(());
                } else {
                    println!("  ⏸ Approval required for '{}'", phase_id);
                    
                    // Show artifacts if available
                    let phase_idx = engine.definition().phase_index(phase_id);
                    if let Some(idx) = phase_idx {
                        let artifacts = &engine.state().phase_states[idx].artifacts_produced;
                        if !artifacts.is_empty() {
                            println!("    Review artifacts:");
                            for artifact in artifacts {
                                println!("      - {}", artifact);
                            }
                        }
                    }
                    
                    // Prompt for approval
                    loop {
                        print!("    Approve? [y/n/s(kip)] ");
                        use std::io::Write;
                        std::io::stdout().flush()?;
                        
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        let choice = input.trim().to_lowercase();
                        
                        match choice.as_str() {
                            "y" | "yes" => {
                                engine.approve().await?;
                                break;
                            }
                            "n" | "no" => {
                                println!("  ✗ Rejected. Ritual paused.");
                                return Ok(());
                            }
                            "s" | "skip" => {
                                engine.skip_current()?;
                                println!("  ⊘ Skipped phase: {}", phase_id);
                                break;
                            }
                            _ => {
                                println!("    Invalid choice. Enter y, n, or s.");
                            }
                        }
                    }
                    continue;
                }
            }
            RitualStatus::Running => {
                // Phase completed, show success and continue
                if !json && current_phase < total_phases {
                    let phase_state = &engine.state().phase_states[current_phase];
                    let artifact_count = phase_state.artifacts_produced.len();
                    
                    match &phase_state.status {
                        PhaseStatus::Completed => {
                            println!("  ✓ {} completed ({:.1}s, {} artifact{})",
                                phase_state.phase_id,
                                phase_duration.as_secs_f64(),
                                artifact_count,
                                if artifact_count == 1 { "" } else { "s" }
                            );
                        }
                        PhaseStatus::Skipped { reason } => {
                            println!("  ⊘ {} skipped: {}", phase_state.phase_id, reason);
                        }
                        _ => {}
                    }
                }
                continue;
            }
            RitualStatus::Completed => {
                if json {
                    println!("{}", serde_json::json!({
                        "status": "completed"
                    }));
                } else {
                    println!();
                    println!("✓ Ritual completed successfully!");
                }
                return Ok(());
            }
            RitualStatus::Failed { phase_id, error } => {
                if json {
                    println!("{}", serde_json::json!({
                        "status": "failed",
                        "phase_id": phase_id,
                        "error": error
                    }));
                } else {
                    eprintln!("  ✗ {} failed: {}", phase_id, error);
                }
                std::process::exit(1);
            }
            RitualStatus::Cancelled => {
                if json {
                    println!("{}", serde_json::json!({"status": "cancelled"}));
                } else {
                    println!("⊘ Ritual was cancelled.");
                }
                return Ok(());
            }
            RitualStatus::Paused => {
                if json {
                    println!("{}", serde_json::json!({"status": "paused"}));
                } else {
                    println!("⏸ Ritual paused. Run `gid ritual run` to resume.");
                }
                return Ok(());
            }
        }
    }
}

fn cmd_ritual_status(project_root: &std::path::Path, json: bool) -> Result<()> {
    use gid_core::ritual::{RitualDefinition, RitualEngine, RitualStatus, PhaseStatus};
    
    let ritual_path = project_root.join(".gid/ritual.yml");
    let state_path = project_root.join(".gid/ritual-state.json");
    
    if !ritual_path.exists() {
        if json {
            println!("{}", serde_json::json!({"exists": false}));
        } else {
            println!("No ritual configured. Run `gid ritual init` to create one.");
        }
        return Ok(());
    }
    
    let template_dirs = vec![project_root.join(".gid/rituals/")];
    let definition = RitualDefinition::load(&ritual_path, &template_dirs)?;
    
    if !state_path.exists() {
        if json {
            println!("{}", serde_json::json!({
                "exists": true,
                "running": false,
                "name": definition.name,
                "phases": definition.phases.len()
            }));
        } else {
            println!("Ritual: {} ({} phases)", definition.name, definition.phases.len());
            println!("Status: Not started");
            println!("\nRun `gid ritual run` to start.");
        }
        return Ok(());
    }
    
    let engine = RitualEngine::resume(definition, project_root)?;
    let state = engine.state();
    
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    } else {
        println!("Ritual: {}", state.ritual_name);
        println!("Started: {}", state.started_at);
        
        match &state.status {
            RitualStatus::Running => println!("Status: Running"),
            RitualStatus::WaitingApproval { phase_id, .. } => {
                println!("Status: Waiting approval for '{}'", phase_id);
            }
            RitualStatus::Paused => println!("Status: Paused"),
            RitualStatus::Completed => println!("Status: Completed ✓"),
            RitualStatus::Failed { phase_id, error } => {
                println!("Status: Failed at '{}': {}", phase_id, error);
            }
            RitualStatus::Cancelled => println!("Status: Cancelled"),
        }
        
        println!("\nPhases:");
        for (i, phase_state) in state.phase_states.iter().enumerate() {
            let icon = match &phase_state.status {
                PhaseStatus::Pending => "○",
                PhaseStatus::Running => "◐",
                PhaseStatus::Completed => "●",
                PhaseStatus::Skipped { .. } => "⊘",
                PhaseStatus::WaitingApproval => "⏸",
                PhaseStatus::Failed => "✗",
            };
            let current = if i == state.current_phase { " ← current" } else { "" };
            println!("  {} {} {}{}", icon, i, phase_state.phase_id, current);
        }
    }
    
    Ok(())
}

async fn cmd_ritual_approve(project_root: &std::path::Path, json: bool) -> Result<()> {
    use gid_core::ritual::{RitualDefinition, RitualEngine, RitualStatus};
    
    let ritual_path = project_root.join(".gid/ritual.yml");
    let state_path = project_root.join(".gid/ritual-state.json");
    
    if !state_path.exists() {
        bail!("No ritual in progress. Run `gid ritual run` first.");
    }
    
    let template_dirs = vec![project_root.join(".gid/rituals/")];
    let definition = RitualDefinition::load(&ritual_path, &template_dirs)?;
    let mut engine = RitualEngine::resume(definition, project_root)?;
    
    let status = engine.approve().await?;
    
    match &status {
        RitualStatus::WaitingApproval { phase_id, message, .. } => {
            if json {
                println!("{}", serde_json::json!({
                    "approved": true,
                    "next_approval": phase_id,
                    "message": message
                }));
            } else {
                println!("✓ Approved. Now waiting for next approval:\n");
                println!("{}", message);
            }
        }
        RitualStatus::Completed => {
            if json {
                println!("{}", serde_json::json!({"approved": true, "completed": true}));
            } else {
                println!("✓ Approved. Ritual completed!");
            }
        }
        RitualStatus::Failed { phase_id, error } => {
            if json {
                println!("{}", serde_json::json!({
                    "approved": true,
                    "failed": true,
                    "phase_id": phase_id,
                    "error": error
                }));
            } else {
                eprintln!("✓ Approved, but ritual failed at '{}': {}", phase_id, error);
            }
            std::process::exit(1);
        }
        _ => {
            if json {
                println!("{}", serde_json::json!({"approved": true, "status": format!("{:?}", status)}));
            } else {
                println!("✓ Approved.");
            }
        }
    }
    
    Ok(())
}

fn cmd_ritual_skip(project_root: &std::path::Path, json: bool) -> Result<()> {
    use gid_core::ritual::{RitualDefinition, RitualEngine};
    
    let ritual_path = project_root.join(".gid/ritual.yml");
    let state_path = project_root.join(".gid/ritual-state.json");
    
    if !state_path.exists() {
        bail!("No ritual in progress. Run `gid ritual run` first.");
    }
    
    let template_dirs = vec![project_root.join(".gid/rituals/")];
    let definition = RitualDefinition::load(&ritual_path, &template_dirs)?;
    let mut engine = RitualEngine::resume(definition, project_root)?;
    
    let phase_id = engine.state().phase_states.get(engine.state().current_phase)
        .map(|p| p.phase_id.clone())
        .unwrap_or_else(|| "unknown".to_string());
    
    engine.skip_current()?;
    
    if json {
        println!("{}", serde_json::json!({
            "skipped": true,
            "phase_id": phase_id
        }));
    } else {
        println!("⊘ Skipped phase: {}", phase_id);
        println!("  Run `gid ritual run` to continue.");
    }
    
    Ok(())
}

fn cmd_ritual_cancel(project_root: &std::path::Path, json: bool) -> Result<()> {
    use gid_core::ritual::{RitualDefinition, RitualEngine};
    
    let ritual_path = project_root.join(".gid/ritual.yml");
    let state_path = project_root.join(".gid/ritual-state.json");
    
    if !state_path.exists() {
        bail!("No ritual in progress.");
    }
    
    let template_dirs = vec![project_root.join(".gid/rituals/")];
    let definition = RitualDefinition::load(&ritual_path, &template_dirs)?;
    let mut engine = RitualEngine::resume(definition, project_root)?;
    
    engine.cancel()?;
    
    if json {
        println!("{}", serde_json::json!({"cancelled": true}));
    } else {
        println!("⊘ Ritual cancelled.");
        println!("  State preserved in .gid/ritual-state.json");
        println!("  Run `gid ritual run` to resume, or delete state file to start fresh.");
    }
    
    Ok(())
}

fn cmd_ritual_templates(project_root: &std::path::Path, json: bool) -> Result<()> {
    use gid_core::ritual::TemplateRegistry;
    
    let registry = TemplateRegistry::for_project(project_root);
    let templates = registry.list()?;
    
    if json {
        println!("{}", serde_json::to_string_pretty(&templates)?);
    } else {
        println!("Available Ritual Templates\n");
        for template in &templates {
            let source = if template.source.to_string_lossy() == "<builtin>" {
                "(built-in)".to_string()
            } else {
                format!("({})", template.source.display())
            };
            println!("  {} {} — {} phases", template.name, source, template.phase_count);
            if let Some(ref desc) = template.description {
                println!("    {}", desc);
            }
            println!();
        }
        println!("Use `gid ritual init --template <name>` to create a ritual.");
    }
    
    Ok(())
}

fn phase_kind_name(kind: &gid_core::ritual::PhaseKind) -> &'static str {
    use gid_core::ritual::PhaseKind;
    match kind {
        PhaseKind::Skill { .. } => "skill",
        PhaseKind::GidCommand { .. } => "gid_command",
        PhaseKind::Harness { .. } => "harness",
        PhaseKind::Shell { .. } => "shell",
    }
}

// =============================================================================
// Watch Command
// =============================================================================

fn cmd_watch(dir: &PathBuf, debounce_ms: u64, no_lsp: bool, no_semantify: bool, graph_override: Option<&PathBuf>) -> Result<()> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use gid_core::watch::{WatchConfig, sync_on_change, should_trigger_sync};
    use gid_core::ignore::load_ignore_list;

    let dir = if dir.is_absolute() {
        dir.clone()
    } else {
        std::env::current_dir()?.join(dir)
    };

    if !dir.exists() {
        bail!("Directory not found: {}", dir.display());
    }

    // Resolve .gid/ directory
    let gid_dir = if let Some(graph_path) = graph_override {
        graph_path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".gid"))
    } else {
        find_graph_file_walk_up(&dir)
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .or_else(|| {
                std::env::current_dir().ok()
                    .and_then(|cwd| find_graph_file_walk_up(&cwd))
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            })
            .unwrap_or_else(|| dir.join(".gid"))
    };

    if !gid_dir.exists() {
        bail!(".gid/ directory not found. Run 'gid init' first, or specify --graph <path>.");
    }

    let ignore_list = load_ignore_list(&dir);

    let config = WatchConfig {
        watch_dir: dir.clone(),
        gid_dir: gid_dir.clone(),
        debounce_ms,
        lsp: !no_lsp,
        no_semantify,
        backend: None,
    };

    eprintln!("👁 Watching {} for changes (debounce: {}ms)", dir.display(), debounce_ms);
    eprintln!("   Graph: {}/graph.yml", gid_dir.display());
    eprintln!("   Press Ctrl+C to stop.\n");

    // Initial sync to ensure graph is up-to-date
    match sync_on_change(&config) {
        Ok(result) if result.graph_modified => {
            eprintln!("♻ Initial sync: {} files, {} nodes, {} edges ({}ms)",
                result.files_changed, result.code_nodes, result.code_edges, result.duration_ms);
        }
        Ok(_) => eprintln!("✓ Graph is up-to-date."),
        Err(e) => eprintln!("⚠ Initial sync failed: {}", e),
    }

    // Set up file watcher
    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default())
        .context("failed to create file watcher")?;

    watcher.watch(&dir, RecursiveMode::Recursive)
        .context("failed to start watching directory")?;

    // Set up Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }).context("failed to set Ctrl+C handler")?;

    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(event)) => {
                // Check if any changed path is relevant
                let relevant = event.paths.iter().any(|p| {
                    should_trigger_sync(p, &dir, &gid_dir, &ignore_list)
                });

                if !relevant {
                    continue;
                }

                // Debounce: drain any queued events within the debounce window
                std::thread::sleep(Duration::from_millis(debounce_ms));
                while rx.try_recv().is_ok() {}

                // Panic-safe extraction
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sync_on_change(&config)
                }));

                match result {
                    Ok(Ok(r)) if r.graph_modified => {
                        eprintln!("♻ Synced: {} files changed, {} nodes, {} edges ({}ms)",
                            r.files_changed, r.code_nodes, r.code_edges, r.duration_ms);
                    }
                    Ok(Ok(_)) => {} // no change
                    Ok(Err(e)) => eprintln!("⚠ Extraction error: {}", e),
                    Err(_) => eprintln!("⚠ Extraction panicked, continuing watch"),
                }
            }
            Ok(Err(e)) => eprintln!("⚠ Watch error: {}", e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    eprintln!("\n👋 Watch stopped.");
    Ok(())
}

// =============================================================================
// Infer Command
// =============================================================================

/// Inputs for `cmd_infer` — packs the parameters from the `infer` CLI
/// subcommand (clustering controls, LLM toggles, output formatting, and
/// algorithmic tuning knobs).
struct InferOpts<'a> {
    level_str: &'a str,
    phase: Option<&'a str>,
    model: &'a str,
    no_llm: bool,
    dry_run: bool,
    format_str: &'a str,
    max_tokens: usize,
    source: Option<PathBuf>,
    hierarchical: bool,
    num_trials: Option<u32>,
    min_community_size: Option<usize>,
    max_cluster_size: Option<usize>,
    edge_weight_overrides: Vec<String>,
    json: bool,
}

async fn cmd_infer(
    ctx: &GraphContext,
    opts: InferOpts<'_>,
) -> Result<()> {
    let InferOpts {
        level_str,
        phase,
        model,
        no_llm,
        dry_run,
        format_str,
        max_tokens,
        source,
        hierarchical,
        num_trials,
        min_community_size,
        max_cluster_size,
        edge_weight_overrides,
        json,
    } = opts;
    use gid_core::infer;

    let mut graph = ctx.load()?;

    // Parse level
    let level = match level_str {
        "component" => infer::InferLevel::Component,
        "feature" => infer::InferLevel::Feature,
        "all" => infer::InferLevel::All,
        other => bail!("Unknown level '{}'. Use: component, feature, all", other),
    };

    // Parse output format
    let out_format = match format_str {
        "summary" => infer::OutputFormat::Summary,
        "yaml" => infer::OutputFormat::Yaml,
        "json" => infer::OutputFormat::Json,
        other => bail!("Unknown format '{}'. Use: summary, yaml, json", other),
    };

    // Build clustering config
    let mut cluster_config = infer::ClusterConfig {
        hierarchical,
        ..Default::default()
    };
    if let Some(n) = num_trials {
        cluster_config.num_trials = n;
    }
    if let Some(n) = min_community_size {
        cluster_config.min_community_size = n;
    }
    if let Some(n) = max_cluster_size {
        cluster_config.max_cluster_size = Some(n);
    }

    // Parse --edge-weight RELATION=WEIGHT overrides (ISS-049).
    // Each entry overrides one relation's weight. Setting weight=0 effectively
    // ignores that relation (build_network skips zero-weight edges).
    for spec in &edge_weight_overrides {
        let (relation, weight_str) = spec.split_once('=').ok_or_else(|| {
            anyhow!("--edge-weight expects RELATION=WEIGHT, got {:?}", spec)
        })?;
        let relation = relation.trim();
        if relation.is_empty() {
            bail!("--edge-weight: relation name cannot be empty (got {:?})", spec);
        }
        let weight: f64 = weight_str.trim().parse().map_err(|e| {
            anyhow!("--edge-weight: invalid weight {:?} for {:?}: {}", weight_str, relation, e)
        })?;
        if !weight.is_finite() || weight < 0.0 {
            bail!("--edge-weight: weight must be finite and non-negative (got {} for {:?})", weight, relation);
        }
        cluster_config.edge_weights.insert(relation.to_string(), weight);
    }

    // Build labeling config
    let labeling_config = if no_llm {
        None
    } else {
        let mut lc = infer::LabelingConfig::default();
        lc.token_budget = max_tokens;
        Some(lc)
    };

    // Build top-level config
    let config = infer::InferConfig {
        clustering: cluster_config,
        labeling: labeling_config,
        level,
        format: out_format,
        dry_run,
        source_dir: source,
    };

    // Create LLM client if needed
    let llm_client: Option<gid_core::infer::CliLlm> = if no_llm || level == infer::InferLevel::Component {
        None
    } else {
        Some(gid_core::infer::CliLlm::new(model))
    };

    // Handle --phase for step-by-step execution
    if let Some(phase_str) = phase {
        match phase_str {
            "clustering" => {
                eprintln!("Running clustering phase only...");
                let cluster_result = infer::cluster(&graph, &config.clustering)?;
                eprintln!("✅ Clustering complete: {} communities, codelength={:.3}",
                    cluster_result.metrics.num_communities, cluster_result.metrics.codelength);

                // In phase mode for clustering, output the component nodes
                let result = infer::InferResult::from_phases(&cluster_result, &infer::LabelingResult::empty());
                println!("{}", infer::format_output(&result, config.format));
                return Ok(());
            }
            "labeling" => {
                eprintln!("Running full pipeline (labeling requires clustering first)...");
                // Fall through to full run — labeling can't run without clustering
            }
            "integration" => {
                eprintln!("Running full pipeline then merging...");
                // Fall through to full run — integration needs results
            }
            other => bail!("Unknown phase '{}'. Use: clustering, labeling, integration", other),
        }
    }

    // Run full pipeline
    eprintln!("🔍 Running infer pipeline (level={:?})...", config.level);
    let result = infer::run(&graph, &config, llm_client.as_ref().map(|c| c as &dyn infer::SimpleLlm)).await?;

    if result.node_count() == 0 {
        eprintln!("⚠ No architecture inferred. Ensure the graph has code nodes (run `gid extract` first).");
        return Ok(());
    }

    // Output formatted result
    let output = infer::format_output(&result, if dry_run { infer::OutputFormat::Yaml } else { config.format });

    if json {
        // --json global flag overrides format
        println!("{}", infer::format_output(&result, infer::OutputFormat::Json));
    } else {
        println!("{}", output);
    }

    // Merge into graph (unless dry-run)
    if !dry_run {
        let stats = infer::merge_into_graph(&mut graph, &result, true);
        ctx.save(&graph)?;
        eprintln!("\n📊 Merged: +{} components, +{} features, +{} edges ({} old removed, {} skipped)",
            stats.components_added, stats.features_added, stats.edges_added,
            stats.old_nodes_removed + stats.old_edges_removed, stats.nodes_skipped);
    }

    Ok(())
}

// =============================================================================
// Project Registry Commands (ISS-028)
// =============================================================================

fn cmd_project(action: ProjectCommands, json: bool) -> Result<()> {
    use gid_core::project_registry::{Registry, ProjectEntry, default_registry_path};

    let reg_path = default_registry_path()
        .context("could not determine registry path (no $HOME and no $XDG_CONFIG_HOME)")?;

    match action {
        ProjectCommands::Where => {
            if json {
                println!("{}", serde_json::json!({ "path": reg_path }));
            } else {
                println!("{}", reg_path.display());
            }
            Ok(())
        }

        ProjectCommands::List { all } => {
            let reg = Registry::load_from(&reg_path)
                .with_context(|| format!("loading registry at {}", reg_path.display()))?;
            let entries: Vec<_> = reg.list(all).collect();
            if json {
                let items: Vec<_> = entries.iter().map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "path": p.path,
                        "aliases": p.aliases,
                        "default_branch": p.default_branch,
                        "tags": p.tags,
                        "archived": p.archived,
                        "notes": p.notes,
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            if entries.is_empty() {
                eprintln!("No projects registered.");
                eprintln!("  Registry: {}", reg_path.display());
                eprintln!("  Add one with:  gid project add <name> <path>");
                return Ok(());
            }
            println!("Registered projects ({}) at {}:",
                     entries.len(), reg_path.display());
            for p in entries {
                let arch = if p.archived { " [archived]" } else { "" };
                print!("  {}{}", p.name, arch);
                if !p.aliases.is_empty() {
                    print!(" (aka: {})", p.aliases.join(", "));
                }
                println!();
                println!("    path: {}", p.path.display());
                if let Some(b) = &p.default_branch {
                    println!("    branch: {}", b);
                }
                if !p.tags.is_empty() {
                    println!("    tags: {}", p.tags.join(", "));
                }
                if let Some(n) = &p.notes {
                    println!("    notes: {}", n);
                }
            }
            Ok(())
        }

        ProjectCommands::Resolve { ident } => {
            let reg = Registry::load_from(&reg_path)
                .with_context(|| format!("loading registry at {}", reg_path.display()))?;
            match reg.resolve(&ident) {
                Ok(entry) => {
                    if json {
                        println!("{}", serde_json::json!({
                            "name": entry.name,
                            "path": entry.path,
                            "aliases": entry.aliases,
                        }));
                    } else {
                        // Print the canonical path to stdout — this is the contract
                        // for consumers that shell out to `gid project resolve <x>`.
                        println!("{}", entry.path.display());
                    }
                    Ok(())
                }
                Err(e) => {
                    // Non-zero exit with a helpful message on stderr.
                    eprintln!("Error: {}", e);
                    if matches!(e, gid_core::project_registry::RegistryError::NotFound(_)) {
                        eprintln!("  Registry: {}", reg_path.display());
                        eprintln!("  Register it with:  gid project add <name> <path>");
                    }
                    std::process::exit(1);
                }
            }
        }

        ProjectCommands::Add { name, path, aliases, default_branch, tags, notes } => {
            let mut reg = Registry::load_from(&reg_path)
                .with_context(|| format!("loading registry at {}", reg_path.display()))?;

            let aliases_vec: Vec<String> = aliases
                .map(|s| s.split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect())
                .unwrap_or_default();
            let tags_vec: Vec<String> = tags
                .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
                .unwrap_or_default();

            // Canonicalize path (absolute, resolve symlinks) to avoid registering
            // the same project twice with different path forms.
            let abs_path = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir()?.join(&path)
            };
            let canon = abs_path.canonicalize()
                .with_context(|| format!("could not canonicalize path: {}", abs_path.display()))?;

            reg.add(ProjectEntry {
                name: name.clone(),
                path: canon.clone(),
                aliases: aliases_vec,
                default_branch,
                tags: tags_vec,
                archived: false,
                notes,
            }).with_context(|| format!("failed to add project '{}'", name))?;

            reg.save_to(&reg_path)
                .with_context(|| format!("saving registry to {}", reg_path.display()))?;

            if json {
                println!("{}", serde_json::json!({
                    "ok": true,
                    "name": name,
                    "path": canon,
                    "registry": reg_path,
                }));
            } else {
                eprintln!("✓ Added '{}' → {}", name, canon.display());
                eprintln!("  Registry: {}", reg_path.display());
            }
            Ok(())
        }

        ProjectCommands::Remove { name } => {
            let mut reg = Registry::load_from(&reg_path)
                .with_context(|| format!("loading registry at {}", reg_path.display()))?;
            let removed = reg.remove(&name)
                .with_context(|| format!("removing '{}' from registry", name))?;
            reg.save_to(&reg_path)
                .with_context(|| format!("saving registry to {}", reg_path.display()))?;
            if json {
                println!("{}", serde_json::json!({
                    "ok": true,
                    "removed": {
                        "name": removed.name,
                        "path": removed.path,
                    },
                }));
            } else {
                eprintln!("✓ Removed '{}' (was: {})", removed.name, removed.path.display());
            }
            Ok(())
        }
    }
}

// =============================================================================
// Artifact Commands (ISS-053 Phase E)
// =============================================================================
//
// Six kind-agnostic verbs over `gid_core::ArtifactStore`. Behavior differences
// between artifact kinds live in `Layout`, NOT here — this module must remain
// invariant under "add a new kind".
//
// Reference parsing
// -----------------
// Refs accepted by every verb take one of these shapes:
//   - `<project>:<short_or_path>` — qualified, project resolved via registry.
//   - `<short_or_path>` — unqualified; project comes from `--project` or cwd.
//
// Short forms:
//   - `<project>:.gid/issues/ISS-001/issue.md` — explicit canonical path.
//   - `<project>:ISS-001` — short id; resolved by scanning the project for a
//      pattern slot (`id`, `slug`, `seq`, `name`) that equals the short.

fn cmd_artifact(action: ArtifactCommands, json: bool) -> Result<()> {
    match action {
        ArtifactCommands::List { kind, project } => cmd_artifact_list(kind, project, json),
        ArtifactCommands::Show { artifact_ref, project } => {
            cmd_artifact_show(&artifact_ref, project, json)
        }
        ArtifactCommands::New { kind, project, parent, title, slots } => {
            cmd_artifact_new(&kind, project, parent, title, slots, json)
        }
        ArtifactCommands::Update { artifact_ref, project, fields } => {
            cmd_artifact_update(&artifact_ref, project, fields, json)
        }
        ArtifactCommands::Relate { from, kind, to, project } => {
            cmd_artifact_relate(&from, &kind, &to, project, json)
        }
        ArtifactCommands::Refs { artifact_ref, project } => {
            cmd_artifact_refs(&artifact_ref, project, json)
        }
    }
}

// ---------------------------------------------------------------------------
// Project / ref resolution helpers
// ---------------------------------------------------------------------------

/// Open an `ArtifactStore` for the project resolved from (in order):
///   1. explicit `--project` arg
///   2. project component of `ref_hint` (e.g. `engram:ISS-022` → `engram`)
///   3. project containing the current working directory (registry walk)
fn open_store_for(
    project_arg: Option<&str>,
    ref_hint: Option<&str>,
) -> Result<gid_core::ArtifactStore> {
    use gid_core::ArtifactStore;

    if let Some(name) = project_arg {
        return ArtifactStore::open(name)
            .with_context(|| format!("opening artifact store for project '{}'", name));
    }

    // Try project prefix from ref.
    if let Some(r) = ref_hint {
        if let Some((proj, _)) = r.split_once(':') {
            if !proj.is_empty() {
                return ArtifactStore::open(proj)
                    .with_context(|| format!("opening artifact store for project '{}'", proj));
            }
        }
    }

    // Walk up from cwd looking for a project root that's registered.
    let cwd = std::env::current_dir()?;
    let project_root = find_artifact_project_root(&cwd).ok_or_else(|| {
        anyhow!(
            "no `--project` given and current directory `{}` is not inside a project root \
             (no `.gid/` ancestor found)",
            cwd.display()
        )
    })?;

    // Look up the project name from the registry by matching the path.
    let registry = gid_core::project_registry::Registry::load_default()
        .context("loading project registry (~/.config/gid/projects.yml)")?;
    let canonical_root = std::fs::canonicalize(&project_root).unwrap_or(project_root.clone());
    for entry in registry.list(true) {
        let entry_canonical =
            std::fs::canonicalize(&entry.path).unwrap_or_else(|_| entry.path.clone());
        if entry_canonical == canonical_root {
            return ArtifactStore::open_at(entry.name.clone(), project_root.clone())
                .with_context(|| {
                    format!("opening artifact store at `{}`", project_root.display())
                });
        }
    }

    // Not registered: open with project name = root's last path component.
    let name = project_root
        .file_name()
        .and_then(|s: &std::ffi::OsStr| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    ArtifactStore::open_at(name, project_root.clone())
        .with_context(|| format!("opening artifact store at `{}`", project_root.display()))
}

/// Find the nearest ancestor of `start` that contains a `.gid/` directory.
/// (Local helper for artifact commands; distinct from
/// `gid_core::find_project_root` which has a different signature.)
fn find_artifact_project_root(start: &Path) -> Option<PathBuf> {
    let mut cur: &Path = start;
    loop {
        if cur.join(".gid").is_dir() {
            return Some(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

/// Strip the `<project>:` prefix from a ref, if present. Returns the part
/// after the `:` (or the ref unchanged if no prefix).
fn ref_path_part(r: &str) -> &str {
    match r.split_once(':') {
        Some((proj, rest)) if !proj.is_empty() => rest,
        _ => r,
    }
}

/// Resolve a ref string to a canonical [`gid_core::ArtifactId`] within `store`.
///
/// Strategy:
///   1. If the path-part contains `/`, treat as a relative path inside the
///      project. Accept either `.gid/...` or anything else (we don't enforce).
///   2. Otherwise, treat as a *short id* and scan `store.list()` for an
///      artifact whose Layout-extracted slots contain the short as `id`,
///      `slug`, `seq`, or `name` (in that priority).
fn resolve_ref(
    store: &gid_core::ArtifactStore,
    r: &str,
) -> Result<gid_core::ArtifactId> {
    use gid_core::ArtifactId;

    let path_part = ref_path_part(r);
    if path_part.is_empty() {
        bail!("empty artifact ref: `{}`", r);
    }
    if path_part.contains('/') {
        return ArtifactId::new(path_part)
            .with_context(|| format!("parsing ref `{}` as relative path", r));
    }

    // Short-id resolution: scan and match by slot.
    let layout = store.layout();
    let artifacts = store
        .list(None)
        .with_context(|| format!("listing artifacts to resolve short ref `{}`", r))?;
    let mut candidates: Vec<&gid_core::Artifact> = Vec::new();
    for art in &artifacts {
        let rel = art.id.as_str().strip_prefix(".gid/").unwrap_or(art.id.as_str());
        let m = layout.match_path(rel);
        let hits = ["id", "slug", "name", "seq"]
            .iter()
            .any(|k| m.slots.get(*k).map(|v: &String| v.as_str()) == Some(path_part));
        if hits {
            candidates.push(art);
        }
    }
    match candidates.len() {
        0 => bail!(
            "no artifact in project `{}` matches short ref `{}` \
             (looked for slots: id, slug, name, seq)",
            store.project(),
            path_part
        ),
        1 => Ok(candidates[0].id.clone()),
        _ => bail!(
            "ambiguous short ref `{}` in project `{}`: matched {} artifacts ({})",
            path_part,
            store.project(),
            candidates.len(),
            candidates
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// ---------------------------------------------------------------------------
// Verb 1: list
// ---------------------------------------------------------------------------

fn cmd_artifact_list(
    kind: Option<String>,
    project: Option<String>,
    json: bool,
) -> Result<()> {
    let store = open_store_for(project.as_deref(), None)?;
    let artifacts = store
        .list(kind.as_deref())
        .with_context(|| format!("listing artifacts in `{}`", store.project()))?;

    if json {
        let arr: Vec<serde_json::Value> = artifacts
            .iter()
            .map(|a| {
                let title = a
                    .metadata
                    .get("title")
                    .and_then(|v| v.as_scalar().map(|s| s.to_string()))
                    .unwrap_or_default();
                serde_json::json!({
                    "project": store.project(),
                    "path": a.id.as_str(),
                    "kind": a.kind,
                    "title": title,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(arr));
    } else {
        if artifacts.is_empty() {
            eprintln!(
                "(no artifacts{} in project '{}')",
                kind.as_deref().map(|k| format!(" of kind '{}'", k)).unwrap_or_default(),
                store.project()
            );
            return Ok(());
        }
        for a in &artifacts {
            let title = a
                .metadata
                .get("title")
                .and_then(|v| v.as_scalar().map(|s| s.to_string()))
                .unwrap_or_default();
            println!(
                "{:<10}  {:<60}  {}",
                a.kind,
                a.id.as_str(),
                title
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Verb 2: show
// ---------------------------------------------------------------------------

fn cmd_artifact_show(
    artifact_ref: &str,
    project: Option<String>,
    json: bool,
) -> Result<()> {
    let store = open_store_for(project.as_deref(), Some(artifact_ref))?;
    let id = resolve_ref(&store, artifact_ref)?;
    let art = store
        .get(&id)
        .with_context(|| format!("loading artifact `{}`", id.as_str()))?
        .ok_or_else(|| anyhow!(
            "artifact not found: project=`{}` path=`{}`",
            store.project(),
            id.as_str()
        ))?;

    if json {
        println!("{}", artifact_to_json(&store, &art));
    } else {
        println!("# {} ({})", art.id.as_str(), art.kind);
        println!("project: {}", store.project());
        if !art.metadata.is_empty() {
            println!("---");
            for (k, v) in art.metadata.fields() {
                match v {
                    gid_core::FieldValue::Scalar(s) => println!("{}: {}", k, s),
                    gid_core::FieldValue::List(items) => {
                        println!("{}: [{}]", k, items.join(", "))
                    }
                }
            }
            println!("---");
        }
        print!("{}", art.body);
    }
    Ok(())
}

fn artifact_to_json(
    store: &gid_core::ArtifactStore,
    art: &gid_core::Artifact,
) -> serde_json::Value {
    let mut metadata = serde_json::Map::new();
    for (k, v) in art.metadata.fields() {
        let val = match v {
            gid_core::FieldValue::Scalar(s) => serde_json::Value::String(s.clone()),
            gid_core::FieldValue::List(items) => serde_json::Value::Array(
                items
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        };
        metadata.insert(k.to_string(), val);
    }
    serde_json::json!({
        "id": {
            "project": store.project(),
            "path": art.id.as_str(),
        },
        "kind": art.kind,
        "metadata": metadata,
        "body": art.body,
    })
}

// ---------------------------------------------------------------------------
// Verb 3: new
// ---------------------------------------------------------------------------

fn cmd_artifact_new(
    kind: &str,
    project: Option<String>,
    parent: Option<String>,
    title: Option<String>,
    slot_args: Vec<String>,
    json: bool,
) -> Result<()> {
    use gid_core::{FieldValue, MetaSourceHint, Metadata};

    let store = open_store_for(project.as_deref(), None)?;

    // Parse slot key=value args.
    let mut slots = gid_core::SlotMap::new();
    for raw in &slot_args {
        let (k, v) = raw.split_once('=').ok_or_else(|| {
            anyhow!(
                "expected positional slot arg in `key=value` form, got `{}`",
                raw
            )
        })?;
        slots.insert(k.trim().to_string(), v.trim().to_string());
    }

    // Resolve parent (if supplied).
    let parent_id = if let Some(p) = parent.as_deref() {
        Some(resolve_ref(&store, p)?)
    } else {
        None
    };

    let path = store
        .next_path(kind, parent_id.as_ref(), &slots)
        .with_context(|| format!("allocating new path for kind `{}`", kind))?;

    // Build minimal frontmatter (title only, if given).
    let mut metadata = Metadata::new(MetaSourceHint::Frontmatter);
    if let Some(t) = title.as_deref() {
        metadata.set_field("title", FieldValue::Scalar(t.to_string()));
    }

    let body = "\n";
    let art = store
        .create(&path, metadata, body)
        .with_context(|| format!("creating artifact at `{}`", path.display()))?;

    if json {
        println!("{}", artifact_to_json(&store, &art));
    } else {
        eprintln!(
            "✓ Created {} ({}) at {}",
            art.id.as_str(),
            art.kind,
            store.project_root().join(art.id.as_path()).display()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Verb 4: update
// ---------------------------------------------------------------------------

fn cmd_artifact_update(
    artifact_ref: &str,
    project: Option<String>,
    fields: Vec<String>,
    json: bool,
) -> Result<()> {
    use gid_core::FieldValue;

    let store = open_store_for(project.as_deref(), Some(artifact_ref))?;
    let id = resolve_ref(&store, artifact_ref)?;
    let mut art = store
        .get(&id)
        .with_context(|| format!("loading artifact `{}`", id.as_str()))?
        .ok_or_else(|| anyhow!(
            "artifact not found: project=`{}` path=`{}`",
            store.project(),
            id.as_str()
        ))?;

    if fields.is_empty() {
        bail!("`gid artifact update` requires at least one --field key=value");
    }
    for raw in &fields {
        let (k, v) = raw.split_once('=').ok_or_else(|| {
            anyhow!("--field expects `key=value`, got `{}`", raw)
        })?;
        let key = k.trim().to_string();
        let value = v.trim();
        let fv = if value.contains(',') {
            FieldValue::List(value.split(',').map(|s| s.trim().to_string()).collect())
        } else {
            FieldValue::Scalar(value.to_string())
        };
        art.metadata.set_field(&key, fv);
    }

    store
        .update(&art)
        .with_context(|| format!("writing updated artifact `{}`", art.id.as_str()))?;

    if json {
        println!("{}", artifact_to_json(&store, &art));
    } else {
        eprintln!(
            "✓ Updated {} ({} fields)",
            art.id.as_str(),
            fields.len()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Verb 5: relate
// ---------------------------------------------------------------------------

fn cmd_artifact_relate(
    from: &str,
    relation_kind: &str,
    to: &str,
    project: Option<String>,
    json: bool,
) -> Result<()> {
    use gid_core::{merge_list, FieldValue};

    // Source store: from project arg, prefix on `from`, or cwd.
    let from_store = open_store_for(project.as_deref(), Some(from))?;
    let from_id = resolve_ref(&from_store, from)?;

    // For `to`, we don't need to open its store — we just need the ref string
    // to embed in the source frontmatter. Honor the project prefix verbatim
    // (qualified ref like `engram:ISS-022`) or use the source's project as
    // the implicit one (unqualified ref → relative path within from_store).
    let to_token: String = if to.contains(':') {
        to.to_string()
    } else {
        // Resolve within the source project to a canonical path so the
        // recorded edge is unambiguous.
        let resolved = resolve_ref(&from_store, to)?;
        resolved.as_str().to_string()
    };

    // Load source artifact and append `to_token` to the relation field.
    let mut art = from_store
        .get(&from_id)
        .with_context(|| format!("loading artifact `{}`", from_id.as_str()))?
        .ok_or_else(|| anyhow!(
            "source artifact not found: project=`{}` path=`{}`",
            from_store.project(),
            from_id.as_str()
        ))?;

    let existing: Vec<String> = art
        .metadata
        .get(relation_kind)
        .map(|v| v.as_list())
        .unwrap_or_default();
    let merged = merge_list(&existing, std::slice::from_ref(&to_token));
    let new_value = if merged.len() == 1 {
        FieldValue::Scalar(merged.into_iter().next().unwrap())
    } else {
        FieldValue::List(merged)
    };
    art.metadata.set_field(relation_kind, new_value);

    from_store
        .update(&art)
        .with_context(|| format!("writing updated artifact `{}`", art.id.as_str()))?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "from": {
                    "project": from_store.project(),
                    "path": art.id.as_str(),
                },
                "kind": relation_kind,
                "to": to_token,
            })
        );
    } else {
        eprintln!(
            "✓ Related {}:{} -[{}]-> {}",
            from_store.project(),
            art.id.as_str(),
            relation_kind,
            to_token
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Verb 6: refs
// ---------------------------------------------------------------------------

fn cmd_artifact_refs(
    artifact_ref: &str,
    project: Option<String>,
    json: bool,
) -> Result<()> {
    let store = open_store_for(project.as_deref(), Some(artifact_ref))?;
    let id = resolve_ref(&store, artifact_ref)?;
    let rels = store
        .relations_to(&id)
        .with_context(|| format!("scanning incoming relations to `{}`", id.as_str()))?;

    if json {
        let arr: Vec<serde_json::Value> = rels
            .iter()
            .map(|r| {
                let source = match &r.source {
                    gid_core::RelationSource::Frontmatter { field } => {
                        serde_json::json!({"type": "frontmatter", "field": field})
                    }
                    gid_core::RelationSource::MarkdownLink => {
                        serde_json::json!({"type": "markdown_link"})
                    }
                    gid_core::RelationSource::BacktickRef => {
                        serde_json::json!({"type": "backtick_ref"})
                    }
                    gid_core::RelationSource::DirectoryNesting => {
                        serde_json::json!({"type": "directory_nesting"})
                    }
                };
                serde_json::json!({
                    "from": r.from.as_str(),
                    "to": r.to.as_str(),
                    "kind": r.kind,
                    "source": source,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(arr));
    } else {
        if rels.is_empty() {
            eprintln!("(no references to {})", id.as_str());
            return Ok(());
        }
        for r in &rels {
            let src = match &r.source {
                gid_core::RelationSource::Frontmatter { field } => {
                    format!("frontmatter:{}", field)
                }
                gid_core::RelationSource::MarkdownLink => "markdown_link".into(),
                gid_core::RelationSource::BacktickRef => "backtick_ref".into(),
                gid_core::RelationSource::DirectoryNesting => "directory_nesting".into(),
            };
            println!("{}  -[{}]->  {}  ({})", r.from.as_str(), r.kind, r.to.as_str(), src);
        }
    }
    Ok(())
}