//! GID CLI - Graph Indexed Development tool
//!
//! A unified graph-based project and task management CLI.

mod llm_client;
// ApiLlmClient now lives in gid-core (ritual::api_llm_client)

use std::path::PathBuf;
use std::path::Path;
use std::collections::HashSet;
use std::io::{self, Read};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use gid_core::{
    Graph, Node, Edge, NodeStatus, TaskSpec,
    load_graph, save_graph,
    parser::find_graph_file_walk_up,
    query::QueryEngine,
    validator::Validator,
    CodeGraph, CodeNode, NodeKind,
    analyze_impact, analyze_impact_filtered, format_impact_for_llm,
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

    /// Validate the graph (cycles, orphans, missing refs)
    Validate,

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
        Commands::Validate => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            cmd_validate_ctx(&ctx, cli.json)
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
            cmd_add_node_ctx(&ctx, &id, &title, desc, status, tags, node_type, cli.json)
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
                QueryCommands::Impact { node, relation, layer, type_filter } => cmd_query_impact_ctx(&ctx, &node, relation.as_deref(), layer, type_filter.as_deref(), cli.json),
                QueryCommands::Deps { node, transitive, relation, layer, type_filter } => {
                    cmd_query_deps_ctx(&ctx, &node, transitive, relation.as_deref(), layer, type_filter.as_deref(), cli.json)
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
        Commands::Extract { dir, format, output, no_lsp, force, no_semantify } => cmd_extract(&dir, &format, output.as_deref(), cli.json, !no_lsp, force, no_semantify, cli.graph.as_ref(), backend_arg),
        Commands::Analyze { file, callers, callees, impact } => cmd_analyze(&file, callers, callees, impact, cli.json),
        Commands::CodeSearch { keywords, dir, format_llm } => cmd_code_search(&dir, &keywords, format_llm, cli.json),
        Commands::CodeFailures { changed, p2p, f2p, dir } => cmd_code_failures(&dir, &changed, p2p.as_deref(), f2p.as_deref(), cli.json),
        Commands::CodeSymptoms { problem, tests, dir } => cmd_code_symptoms(&dir, &problem, &tests, cli.json),
        Commands::CodeTrace { symptoms, depth, max_chains, dir } => cmd_code_trace(&dir, &symptoms, depth, max_chains, cli.json),
        Commands::CodeComplexity { nodes, dir } => cmd_code_complexity(&dir, &nodes, cli.json),
        Commands::CodeImpact { files, dir, relation } => cmd_code_impact(&dir, &files, relation.as_deref(), cli.json),
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
            cmd_context_ctx(&ctx, targets, max_tokens, depth, include, &format, project_root, cli.json)
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
        Commands::Infer { level, phase, model, no_llm, dry_run, format, max_tokens, source, hierarchical, num_trials, min_community_size, max_cluster_size } => {
            let ctx = resolve_graph_ctx(cli.graph, backend_arg)?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cmd_infer(&ctx, &level, phase.as_deref(), &model, no_llm, dry_run, &format, max_tokens, source, hierarchical, num_trials, min_community_size, max_cluster_size, cli.json))
        }
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

fn cmd_validate_ctx(ctx: &GraphContext, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let validator = Validator::new(&graph);
    let result = validator.validate();
    if json {
        println!("{}", serde_json::json!({
            "valid": result.is_valid(),
            "issues": result.issue_count(),
            "orphan_nodes": result.orphan_nodes,
            "missing_refs": result.missing_refs.iter().map(|r| {
                serde_json::json!({"from": r.edge_from, "to": r.edge_to, "missing": r.missing_node})
            }).collect::<Vec<_>>(),
            "cycles": result.cycles,
            "duplicate_nodes": result.duplicate_nodes,
        }));
    } else {
        println!("{}", result);
    }
    if !result.is_valid() {
        std::process::exit(1);
    }
    Ok(())
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

fn cmd_add_node_ctx(
    ctx: &GraphContext,
    id: &str, title: &str, desc: Option<String>,
    status: Option<String>, tags: Option<String>, node_type: Option<String>,
    json: bool,
) -> Result<()> {
    let mut graph = ctx.load()?;
    if graph.get_node(id).is_some() {
        bail!("Node already exists: {}", id);
    }
    let mut node = Node::new(id, title);
    if let Some(d) = desc { node.description = Some(d); }
    if let Some(s) = status { node.status = s.parse()?; }
    if let Some(t) = tags {
        node.tags = t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    if let Some(nt) = node_type { node.node_type = Some(nt); }
    graph.add_node(node);
    ctx.save(&graph)?;
    if json {
        println!("{}", serde_json::json!({"success": true, "id": id}));
    } else {
        println!("✓ Added node: {} — {}", id, title);
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

fn cmd_query_impact_ctx(ctx: &GraphContext, node: &str, relation: Option<&str>, layer: LayerFilter, type_filter: Option<&str>, json: bool) -> Result<()> {
    // For SQLite backend, load graph into memory and use same logic as YAML
    match ctx.backend {
        StorageBackend::Yaml => cmd_query_impact(ctx, node, relation, layer, type_filter, json),
        _ => {
            let graph = ctx.load()?;
            let filtered = apply_layer_filter(&graph, layer);
            let engine = QueryEngine::new(&filtered);
            let rels: Option<Vec<&str>> = relation.map(|r| r.split(',').map(|s| s.trim()).collect());
            let impacted = match &rels {
                Some(r) => engine.impact_filtered(node, Some(r)),
                None => engine.impact(node),
            };
            let impacted: Vec<&Node> = if let Some(tf) = type_filter {
                impacted.into_iter().filter(|n| n.node_type.as_deref() == Some(tf)).collect()
            } else {
                impacted
            };
            if json {
                let nodes: Vec<_> = impacted.iter().map(|n| serde_json::json!({"id": n.id, "title": n.title})).collect();
                println!("{}", serde_json::json!({"node": node, "impacted": nodes}));
            } else {
                if impacted.is_empty() {
                    println!("No nodes would be affected by changes to '{}'", node);
                } else {
                    println!("Changes to '{}' would affect {} node(s):", node, impacted.len());
                    for n in &impacted { println!("  {} — {}", n.id, n.title); }
                }
            }
            Ok(())
        }
    }
}

fn cmd_query_deps_ctx(ctx: &GraphContext, node: &str, transitive: bool, relation: Option<&str>, layer: LayerFilter, type_filter: Option<&str>, json: bool) -> Result<()> {
    match ctx.backend {
        StorageBackend::Yaml => cmd_query_deps(ctx, node, transitive, relation, layer, type_filter, json),
        _ => {
            let graph = ctx.load()?;
            let filtered = apply_layer_filter(&graph, layer);
            let engine = QueryEngine::new(&filtered);
            let rels: Option<Vec<&str>> = relation.map(|r| r.split(',').map(|s| s.trim()).collect());
            let deps = match &rels {
                Some(r) => engine.deps_filtered(node, transitive, Some(r)),
                None => engine.deps(node, transitive),
            };
            let deps: Vec<&Node> = if let Some(tf) = type_filter {
                deps.into_iter().filter(|n| n.node_type.as_deref() == Some(tf)).collect()
            } else {
                deps
            };
            if json {
                let nodes: Vec<_> = deps.iter().map(|n| serde_json::json!({"id": n.id, "title": n.title, "status": n.status.to_string()})).collect();
                println!("{}", serde_json::json!({"node": node, "transitive": transitive, "dependencies": nodes}));
            } else {
                let label = if transitive { "Transitive" } else { "Direct" };
                if deps.is_empty() {
                    println!("'{}' has no {} dependencies", node, label.to_lowercase());
                } else {
                    println!("{} dependencies of '{}' ({}):", label, node, deps.len());
                    for n in &deps { println!("  {} {} — {}", status_icon(&n.status), n.id, n.title); }
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



/// Handle `gid context` — assemble context for target nodes. **[GOAL-4.9, 4.12]**
fn cmd_context_ctx(
    ctx: &GraphContext,
    targets: Vec<String>,
    max_tokens: usize,
    depth: u32,
    include: Vec<String>,
    format: &str,
    project_root: Option<PathBuf>,
    json_flag: bool,
) -> Result<()> {
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

fn cmd_query_impact(ctx: &GraphContext, node: &str, relation: Option<&str>, layer: LayerFilter, type_filter: Option<&str>, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let filtered = apply_layer_filter(&graph, layer);

    let resolved_id = resolve_with_layer_fallback(&filtered, &graph, node, layer, json)?;
    let node = &resolved_id;

    let engine = QueryEngine::new(&filtered);
    let rels: Option<Vec<&str>> = relation.map(|r| r.split(',').map(|s| s.trim()).collect());
    let impacted = match &rels {
        Some(r) => engine.impact_filtered(node, Some(r)),
        None => engine.impact(node),
    };

    // Apply type filter
    let impacted: Vec<&Node> = if let Some(tf) = type_filter {
        impacted.into_iter().filter(|n| n.node_type.as_deref() == Some(tf)).collect()
    } else {
        impacted
    };

    if json {
        let nodes: Vec<_> = impacted.iter().map(|n| serde_json::json!({"id": n.id, "title": n.title})).collect();
        println!("{}", serde_json::json!({"node": node, "relation_filter": relation, "layer": format!("{:?}", layer), "type_filter": type_filter, "impacted": nodes}));
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
    }
    Ok(())
}

fn cmd_query_deps(ctx: &GraphContext, node: &str, transitive: bool, relation: Option<&str>, layer: LayerFilter, type_filter: Option<&str>, json: bool) -> Result<()> {
    let graph = ctx.load()?;
    let filtered = apply_layer_filter(&graph, layer);

    let resolved_id = resolve_with_layer_fallback(&filtered, &graph, node, layer, json)?;
    let node = &resolved_id;

    let engine = QueryEngine::new(&filtered);
    let rels: Option<Vec<&str>> = relation.map(|r| r.split(',').map(|s| s.trim()).collect());
    let deps = match &rels {
        Some(r) => engine.deps_filtered(node, transitive, Some(r)),
        None => engine.deps(node, transitive),
    };

    // Apply type filter
    let deps: Vec<&Node> = if let Some(tf) = type_filter {
        deps.into_iter().filter(|n| n.node_type.as_deref() == Some(tf)).collect()
    } else {
        deps
    };

    if json {
        let nodes: Vec<_> = deps.iter().map(|n| serde_json::json!({
            "id": n.id, "title": n.title, "status": n.status.to_string()
        })).collect();
        println!("{}", serde_json::json!({"node": node, "transitive": transitive, "relation_filter": relation, "layer": format!("{:?}", layer), "type_filter": type_filter, "dependencies": nodes}));
    } else {
        let label = if transitive { "Transitive" } else { "Direct" };
        let filter_note = relation.map(|r| format!(" (relations: {})", r)).unwrap_or_default();
        if deps.is_empty() {
            println!("'{}' has no {} dependencies{}", node, label.to_lowercase(), filter_note);
        } else {
            println!("{} dependencies of '{}' ({}){}:", label, node, deps.len(), filter_note);
            for n in &deps {
                println!("  {} {} — {}", status_icon(&n.status), n.id, n.title);
            }
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



fn cmd_extract(dir: &PathBuf, format: &str, output: Option<&std::path::Path>, json_flag: bool, lsp: bool, force: bool, no_semantify: bool, graph_override: Option<&PathBuf>, backend_arg: Option<String>) -> Result<()> {
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
                        "LSP refinement: {} refined, {} removed, {} failed, {} skipped (languages: {})",
                        stats.refined,
                        stats.removed,
                        stats.failed,
                        stats.skipped,
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
        "summary" | _ => {
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
        let analysis = analyze_impact(&[rel_path.clone()], &unified);
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

fn cmd_code_impact(dir: &PathBuf, files_str: &str, relation: Option<&str>, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let code_graph = load_code_graph(&dir);
    let files: Vec<String> = files_str.split(',').map(|s| s.trim().to_string()).collect();

    // Convert CodeGraph to Graph for impact analysis
    let (code_nodes, code_edges) = codegraph_to_graph_nodes(&code_graph, &dir);
    let mut graph = gid_core::graph::Graph::new();
    graph.nodes = code_nodes;
    graph.edges = code_edges;

    let analysis = if let Some(rel_str) = relation {
        let rels: Vec<&str> = rel_str
            .split(',')
            .map(|s| s.trim())
            .collect();
        analyze_impact_filtered(&files, &graph, Some(&rels))
    } else {
        analyze_impact(&files, &graph)
    };
    let formatted = format_impact_for_llm(&analysis);

    if json {
        println!("{}", serde_json::json!({
            "files_changed": files,
            "relation_filter": relation,
            "risk_level": format!("{:?}", analysis.risk_level),
            "affected_source": analysis.affected_source.len(),
            "affected_tests": analysis.affected_tests.len(),
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

/// Bridge: SimpleLlm trait → claude CLI for the infer pipeline.
struct CliSimpleLlm {
    model: String,
}

#[async_trait::async_trait]
impl gid_core::infer::SimpleLlm for CliSimpleLlm {
    async fn complete(&self, prompt: &str) -> Result<String> {
        let output = tokio::process::Command::new("claude")
            .arg("-p")
            .arg(prompt)
            .arg("--model")
            .arg(&self.model)
            .output()
            .await
            .context("Failed to run claude CLI. Is it installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("claude CLI failed: {}", stderr);
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

async fn cmd_infer(
    ctx: &GraphContext,
    level_str: &str,
    phase: Option<&str>,
    model: &str,
    no_llm: bool,
    dry_run: bool,
    format_str: &str,
    max_tokens: usize,
    source: Option<PathBuf>,
    hierarchical: bool,
    num_trials: Option<u32>,
    min_community_size: Option<usize>,
    max_cluster_size: Option<usize>,
    json: bool,
) -> Result<()> {
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
    let mut cluster_config = infer::ClusterConfig::default();
    cluster_config.hierarchical = hierarchical;
    if let Some(n) = num_trials {
        cluster_config.num_trials = n;
    }
    if let Some(n) = min_community_size {
        cluster_config.min_community_size = n;
    }
    if let Some(n) = max_cluster_size {
        cluster_config.max_cluster_size = Some(n);
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
    let llm_client: Option<CliSimpleLlm> = if no_llm || level == infer::InferLevel::Component {
        None
    } else {
        Some(CliSimpleLlm { model: model.to_string() })
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
