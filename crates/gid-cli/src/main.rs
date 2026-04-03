//! GID CLI - Graph Indexed Development tool
//!
//! A unified graph-based project and task management CLI.

mod llm_client;
mod mcp;

use std::path::PathBuf;
use std::io::{self, Read};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gid_core::{
    Graph, Node, Edge, NodeStatus,
    load_graph, save_graph,
    parser::find_graph_file,
    query::QueryEngine,
    validator::Validator,
    CodeGraph, CodeNode, NodeKind,
    analyze_impact, format_impact_for_llm,
    assess_complexity_from_graph, assess_risk_level,
    build_unified_graph,
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
    Read,

    /// Validate the graph (cycles, orphans, missing refs)
    Validate,

    /// List tasks with optional status filter
    Tasks {
        /// Filter by status (todo, in_progress, done, blocked, cancelled)
        #[arg(short, long)]
        status: Option<String>,
        /// Show only ready tasks (todo with all deps done)
        #[arg(short, long)]
        ready: bool,
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

    // ═══════════════════════════════════════════════════════════════════════════════
    // Ritual Commands (requires "ritual" feature)
    // ═══════════════════════════════════════════════════════════════════════════════

    /// Ritual pipeline orchestration
    #[command(subcommand)]
    Ritual(RitualCommands),

    /// Start MCP server (stdio mode) for AI agent integration
    Mcp,
}

#[derive(Subcommand)]
enum QueryCommands {
    /// Impact analysis: what nodes are affected if this node changes?
    Impact {
        /// Node ID to analyze
        node: String,
    },

    /// Show dependencies of a node
    Deps {
        /// Node ID
        node: String,
        /// Include transitive dependencies
        #[arg(short, long)]
        transitive: bool,
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

    /// Diff current graph against a historical version
    Diff {
        /// Version filename to compare against
        version: String,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { name, desc } => cmd_init(name, desc, cli.json),
        Commands::Read => cmd_read(resolve_graph_path(cli.graph)?, cli.json),
        Commands::Validate => cmd_validate(resolve_graph_path(cli.graph)?, cli.json),
        Commands::Tasks { status, ready } => cmd_tasks(resolve_graph_path(cli.graph)?, status, ready, cli.json),
        Commands::TaskUpdate { id, status } => cmd_task_update(resolve_graph_path(cli.graph)?, &id, &status, cli.json),
        Commands::Complete { id } => cmd_complete(resolve_graph_path(cli.graph)?, &id, cli.json),
        Commands::AddNode { id, title, desc, status, tags, node_type } => {
            cmd_add_node(resolve_graph_path(cli.graph)?, &id, &title, desc, status, tags, node_type, cli.json)
        }
        Commands::RemoveNode { id } => cmd_remove_node(resolve_graph_path(cli.graph)?, &id, cli.json),
        Commands::AddEdge { from, to, relation } => {
            cmd_add_edge(resolve_graph_path(cli.graph)?, &from, &to, &relation, cli.json)
        }
        Commands::RemoveEdge { from, to, relation } => {
            cmd_remove_edge(resolve_graph_path(cli.graph)?, &from, &to, relation.as_deref(), cli.json)
        }
        Commands::Query(qc) => match qc {
            QueryCommands::Impact { node } => cmd_query_impact(resolve_graph_path(cli.graph)?, &node, cli.json),
            QueryCommands::Deps { node, transitive } => {
                cmd_query_deps(resolve_graph_path(cli.graph)?, &node, transitive, cli.json)
            }
            QueryCommands::Path { from, to } => cmd_query_path(resolve_graph_path(cli.graph)?, &from, &to, cli.json),
            QueryCommands::CommonCause { a, b } => cmd_query_common(resolve_graph_path(cli.graph)?, &a, &b, cli.json),
            QueryCommands::Topo => cmd_query_topo(resolve_graph_path(cli.graph)?, cli.json),
        },
        Commands::EditGraph { operations } => cmd_edit_graph(resolve_graph_path(cli.graph)?, &operations, cli.json),
        Commands::Extract { dir, format, output } => cmd_extract(&dir, &format, output.as_deref(), cli.json),
        Commands::Analyze { file, callers, callees, impact } => cmd_analyze(&file, callers, callees, impact, cli.json),
        Commands::CodeSearch { keywords, dir, format_llm } => cmd_code_search(&dir, &keywords, format_llm, cli.json),
        Commands::CodeFailures { changed, p2p, f2p, dir } => cmd_code_failures(&dir, &changed, p2p.as_deref(), f2p.as_deref(), cli.json),
        Commands::CodeSymptoms { problem, tests, dir } => cmd_code_symptoms(&dir, &problem, &tests, cli.json),
        Commands::CodeTrace { symptoms, depth, max_chains, dir } => cmd_code_trace(&dir, &symptoms, depth, max_chains, cli.json),
        Commands::CodeComplexity { nodes, dir } => cmd_code_complexity(&dir, &nodes, cli.json),
        Commands::CodeImpact { files, dir } => cmd_code_impact(&dir, &files, cli.json),
        Commands::CodeSnippets { keywords, max_lines, dir } => cmd_code_snippets(&dir, &keywords, max_lines, cli.json),
        Commands::Schema { dir } => cmd_schema(&dir, cli.json),
        Commands::FileSummary { file, dir } => cmd_file_summary(&dir, &file, cli.json),
        
        // New commands
        Commands::History(hc) => {
            let graph_path = resolve_graph_path(cli.graph)?;
            let gid_dir = graph_path.parent().unwrap_or(std::path::Path::new("."));
            match hc {
                HistoryCommands::List => cmd_history_list(gid_dir, cli.json),
                HistoryCommands::Save { message } => cmd_history_save(&graph_path, gid_dir, message.as_deref(), cli.json),
                HistoryCommands::Diff { version } => cmd_history_diff(&graph_path, gid_dir, &version, cli.json),
                HistoryCommands::Restore { version, force } => cmd_history_restore(&graph_path, gid_dir, &version, force, cli.json),
            }
        }
        Commands::Visual { format, output } => cmd_visual(resolve_graph_path(cli.graph)?, &format, output.as_deref(), cli.json),
        Commands::Advise { errors_only } => cmd_advise(resolve_graph_path(cli.graph)?, errors_only, cli.json),
        Commands::Design { requirements, parse } => cmd_design(requirements, parse, cli.graph, cli.json),
        Commands::Semantify { heuristic, parse } => cmd_semantify(resolve_graph_path(cli.graph)?, heuristic, parse, cli.json),
        Commands::Refactor(rc) => match rc {
            RefactorCommands::Rename { old, new, apply } => {
                cmd_refactor_rename(resolve_graph_path(cli.graph)?, &old, &new, apply, cli.json)
            }
            RefactorCommands::Merge { a, b, new_id, apply } => {
                cmd_refactor_merge(resolve_graph_path(cli.graph)?, &a, &b, &new_id, apply, cli.json)
            }
            RefactorCommands::Split { node, into, apply } => {
                cmd_refactor_split(resolve_graph_path(cli.graph)?, &node, &into, apply, cli.json)
            }
            RefactorCommands::Extract { nodes, parent, title, apply } => {
                cmd_refactor_extract(resolve_graph_path(cli.graph)?, &nodes, &parent, &title, apply, cli.json)
            }
        },

        // Task Harness commands
        Commands::Plan { format } => cmd_plan(resolve_graph_path(cli.graph)?, &format, cli.json),
        Commands::Execute { max_concurrent, model, approval_mode, dry_run } => {
            cmd_execute(resolve_graph_path(cli.graph)?, max_concurrent, model, approval_mode, dry_run, cli.json)
        }
        Commands::Stats => cmd_stats(resolve_graph_path(cli.graph)?, cli.json),
        Commands::Approve => cmd_approve(resolve_graph_path(cli.graph)?, cli.json),
        Commands::Stop => cmd_stop(resolve_graph_path(cli.graph)?, cli.json),

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

        // MCP server
        Commands::Mcp => {
            let cwd = std::env::current_dir()?;
            mcp::cmd_mcp(&cwd)
        }
    }
}

/// Resolve graph path: use provided path or auto-find in cwd.
fn resolve_graph_path(provided: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = provided {
        return Ok(p);
    }

    let cwd = std::env::current_dir()?;
    find_graph_file(&cwd).context(
        "No graph file found. Use --graph <path> or run 'gid init' to create one."
    )
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

fn cmd_read(path: PathBuf, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&graph)?);
    } else {
        let yaml = serde_yaml::to_string(&graph)?;
        print!("{}", yaml);
    }
    Ok(())
}

fn cmd_validate(path: PathBuf, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;
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

fn cmd_tasks(path: PathBuf, status_filter: Option<String>, ready_only: bool, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;

    let tasks: Vec<&Node> = if ready_only {
        graph.ready_tasks()
    } else if let Some(status_str) = &status_filter {
        let status: NodeStatus = status_str.parse()?;
        graph.tasks_by_status(&status)
    } else {
        graph.nodes.iter().collect()
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
        let summary = graph.summary();
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
        let summary = graph.summary();
        println!("\n{}", summary);
    }

    Ok(())
}

fn cmd_task_update(path: PathBuf, id: &str, status_str: &str, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;
    let status: NodeStatus = status_str.parse()?;

    if !graph.update_status(id, status.clone()) {
        bail!("Node not found: {}", id);
    }

    save_graph(&graph, &path)?;
    
    if json {
        println!("{}", serde_json::json!({
            "success": true,
            "id": id,
            "status": status.to_string()
        }));
    } else {
        println!("✓ Updated {} to {}", id, status);
    }
    Ok(())
}

fn cmd_complete(path: PathBuf, id: &str, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;

    if graph.get_node(id).is_none() {
        bail!("Node not found: {}", id);
    }

    let ready_before: std::collections::HashSet<String> = graph
        .ready_tasks()
        .iter()
        .map(|n| n.id.clone())
        .collect();

    graph.update_status(id, NodeStatus::Done);
    save_graph(&graph, &path)?;

    let ready_after: std::collections::HashSet<String> = graph
        .ready_tasks()
        .iter()
        .map(|n| n.id.clone())
        .collect();

    let newly_unblocked: Vec<&String> = ready_after.difference(&ready_before).collect();
    
    if json {
        println!("{}", serde_json::json!({
            "success": true,
            "id": id,
            "newly_unblocked": newly_unblocked
        }));
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

fn cmd_add_node(
    path: PathBuf,
    id: &str,
    title: &str,
    desc: Option<String>,
    status: Option<String>,
    tags: Option<String>,
    node_type: Option<String>,
    json: bool,
) -> Result<()> {
    let mut graph = load_graph(&path)?;

    if graph.get_node(id).is_some() {
        bail!("Node already exists: {}", id);
    }

    let mut node = Node::new(id, title);
    if let Some(d) = desc {
        node.description = Some(d);
    }
    if let Some(s) = status {
        node.status = s.parse()?;
    }
    if let Some(t) = tags {
        node.tags = t.split(',').map(|s| s.trim().to_string()).collect();
    }
    if let Some(nt) = node_type {
        node.node_type = Some(nt);
    }

    graph.add_node(node);
    save_graph(&graph, &path)?;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "id": id}));
    } else {
        println!("✓ Added node: {}", id);
    }
    Ok(())
}

fn cmd_remove_node(path: PathBuf, id: &str, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;

    if graph.remove_node(id).is_none() {
        bail!("Node not found: {}", id);
    }

    save_graph(&graph, &path)?;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "id": id}));
    } else {
        println!("✓ Removed node: {} (and associated edges)", id);
    }
    Ok(())
}

fn cmd_add_edge(path: PathBuf, from: &str, to: &str, relation: &str, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;

    if graph.get_node(from).is_none() {
        bail!("Source node not found: {}", from);
    }
    if graph.get_node(to).is_none() {
        bail!("Target node not found: {}", to);
    }

    if relation == "depends_on" {
        let validator = Validator::new(&graph);
        if validator.would_create_cycle(from, to) {
            bail!("Adding this edge would create a cycle");
        }
    }

    graph.add_edge(Edge::new(from, to, relation));
    save_graph(&graph, &path)?;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "from": from, "to": to, "relation": relation}));
    } else {
        println!("✓ Added edge: {} → {} ({})", from, to, relation);
    }
    Ok(())
}

fn cmd_remove_edge(path: PathBuf, from: &str, to: &str, relation: Option<&str>, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;

    let before = graph.edges.len();
    graph.remove_edge(from, to, relation);
    let after = graph.edges.len();

    if before == after {
        bail!("No matching edge found: {} → {}", from, to);
    }

    save_graph(&graph, &path)?;
    let removed = before - after;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "removed": removed}));
    } else {
        println!("✓ Removed {} edge(s) from {} → {}", removed, from, to);
    }
    Ok(())
}

fn cmd_query_impact(path: PathBuf, node: &str, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;

    if graph.get_node(node).is_none() {
        bail!("Node not found: {}", node);
    }

    let engine = QueryEngine::new(&graph);
    let impacted = engine.impact(node);

    if json {
        let nodes: Vec<_> = impacted.iter().map(|n| serde_json::json!({"id": n.id, "title": n.title})).collect();
        println!("{}", serde_json::json!({"node": node, "impacted": nodes}));
    } else {
        if impacted.is_empty() {
            println!("No nodes would be affected by changes to '{}'", node);
        } else {
            println!("Changes to '{}' would affect {} node(s):", node, impacted.len());
            for n in impacted {
                println!("  {} — {}", n.id, n.title);
            }
        }
    }
    Ok(())
}

fn cmd_query_deps(path: PathBuf, node: &str, transitive: bool, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;

    if graph.get_node(node).is_none() {
        bail!("Node not found: {}", node);
    }

    let engine = QueryEngine::new(&graph);
    let deps = engine.deps(node, transitive);

    if json {
        let nodes: Vec<_> = deps.iter().map(|n| serde_json::json!({
            "id": n.id, "title": n.title, "status": n.status.to_string()
        })).collect();
        println!("{}", serde_json::json!({"node": node, "transitive": transitive, "dependencies": nodes}));
    } else {
        let label = if transitive { "Transitive" } else { "Direct" };
        if deps.is_empty() {
            println!("'{}' has no {} dependencies", node, label.to_lowercase());
        } else {
            println!("{} dependencies of '{}' ({}):", label, node, deps.len());
            for n in deps {
                println!("  {} {} — {}", status_icon(&n.status), n.id, n.title);
            }
        }
    }
    Ok(())
}

fn cmd_query_path(path: PathBuf, from: &str, to: &str, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;

    if graph.get_node(from).is_none() {
        bail!("Node not found: {}", from);
    }
    if graph.get_node(to).is_none() {
        bail!("Node not found: {}", to);
    }

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

fn cmd_query_common(path: PathBuf, a: &str, b: &str, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;

    if graph.get_node(a).is_none() {
        bail!("Node not found: {}", a);
    }
    if graph.get_node(b).is_none() {
        bail!("Node not found: {}", b);
    }

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

fn cmd_query_topo(path: PathBuf, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;
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

fn cmd_edit_graph(path: PathBuf, operations_json: &str, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;

    let ops: Vec<serde_json::Value> = serde_json::from_str(operations_json)
        .context("Invalid JSON. Expected an array of operations.")?;

    let mut applied = 0;

    for op in ops {
        let op_type = op.get("op").and_then(|v| v.as_str()).unwrap_or("");

        match op_type {
            "add_node" => {
                let id = op.get("id").and_then(|v| v.as_str()).context("add_node: missing 'id'")?;
                let title = op.get("title").and_then(|v| v.as_str()).context("add_node: missing 'title'")?;
                if graph.get_node(id).is_none() {
                    let mut node = Node::new(id, title);
                    if let Some(d) = op.get("description").and_then(|v| v.as_str()) {
                        node.description = Some(d.to_string());
                    }
                    if let Some(s) = op.get("status").and_then(|v| v.as_str()) {
                        node.status = s.parse().unwrap_or(NodeStatus::Todo);
                    }
                    if let Some(arr) = op.get("tags").and_then(|v| v.as_array()) {
                        node.tags = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                    }
                    graph.add_node(node);
                    applied += 1;
                }
            }
            "remove_node" => {
                let id = op.get("id").and_then(|v| v.as_str()).context("remove_node: missing 'id'")?;
                if graph.remove_node(id).is_some() {
                    applied += 1;
                }
            }
            "add_edge" => {
                let from = op.get("from").and_then(|v| v.as_str()).context("add_edge: missing 'from'")?;
                let to = op.get("to").and_then(|v| v.as_str()).context("add_edge: missing 'to'")?;
                let relation = op.get("relation").and_then(|v| v.as_str()).unwrap_or("depends_on");
                graph.add_edge(Edge::new(from, to, relation));
                applied += 1;
            }
            "remove_edge" => {
                let from = op.get("from").and_then(|v| v.as_str()).context("remove_edge: missing 'from'")?;
                let to = op.get("to").and_then(|v| v.as_str()).context("remove_edge: missing 'to'")?;
                let relation = op.get("relation").and_then(|v| v.as_str());
                let before = graph.edges.len();
                graph.remove_edge(from, to, relation);
                if graph.edges.len() < before {
                    applied += 1;
                }
            }
            "update_status" => {
                let id = op.get("id").and_then(|v| v.as_str()).context("update_status: missing 'id'")?;
                let status = op.get("status").and_then(|v| v.as_str()).context("update_status: missing 'status'")?;
                if let Ok(s) = status.parse() {
                    if graph.update_status(id, s) {
                        applied += 1;
                    }
                }
            }
            other => {
                if !json {
                    println!("⚠ Unknown operation: {}", other);
                }
            }
        }
    }

    save_graph(&graph, &path)?;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "applied": applied}));
    } else {
        println!("✓ Applied {} operation(s)", applied);
    }
    Ok(())
}

fn cmd_extract(dir: &PathBuf, format: &str, output: Option<&std::path::Path>, json_flag: bool) -> Result<()> {
    let dir = if dir.is_absolute() {
        dir.clone()
    } else {
        std::env::current_dir()?.join(dir)
    };
    
    if !dir.exists() {
        bail!("Directory not found: {}", dir.display());
    }
    
    if !json_flag {
        eprintln!("Extracting code graph from {}...", dir.display());
    }
    let code_graph = CodeGraph::extract_from_dir(&dir);
    
    // Load existing graph if output file exists (for merge behavior)
    let existing_graph = if let Some(out_path) = output {
        if out_path.exists() {
            load_graph(out_path).ok()
        } else {
            None
        }
    } else {
        None
    };
    
    // Convert to unified Graph format
    let task_graph = existing_graph.unwrap_or_else(Graph::default);
    let unified = build_unified_graph(&code_graph, &task_graph);
    
    let output_str = match format {
        "yaml" | "yml" => serde_yaml::to_string(&unified)?,
        "json" => serde_json::to_string_pretty(&unified)?,
        "summary" | _ => {
            if json_flag {
                serde_json::to_string_pretty(&unified)?
            } else {
                // Count nodes by node_type from unified graph
                let file_count = unified.nodes.iter()
                    .filter(|n| n.node_type.as_deref() == Some("file"))
                    .count();
                let class_count = unified.nodes.iter()
                    .filter(|n| n.node_type.as_deref() == Some("class"))
                    .count();
                let func_count = unified.nodes.iter()
                    .filter(|n| n.node_type.as_deref() == Some("function"))
                    .count();
                let task_count = unified.nodes.iter()
                    .filter(|n| n.node_type.is_none() || 
                            !["file", "class", "function", "module"].contains(&n.node_type.as_deref().unwrap_or("")))
                    .count();
                
                // Count edges by relation
                let import_count = unified.edges.iter()
                    .filter(|e| e.relation == "imports")
                    .count();
                let call_count = unified.edges.iter()
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
                    unified.edges.len(), import_count, call_count));
                
                // Count entities per file from metadata
                let mut file_entities: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                for node in &unified.nodes {
                    if let Some(file_path) = node.metadata.get("file_path").and_then(|v| v.as_str()) {
                        if node.node_type.as_deref() != Some("file") {
                            *file_entities.entry(file_path.to_string()).or_default() += 1;
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
    let graph = CodeGraph::extract_from_dir(&project_root);
    
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
        let analysis = analyze_impact(&[rel_path.clone()], &graph);
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

fn cmd_history_list(gid_dir: &std::path::Path, json: bool) -> Result<()> {
    let mgr = HistoryManager::new(gid_dir);
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

fn cmd_history_save(graph_path: &PathBuf, gid_dir: &std::path::Path, message: Option<&str>, json: bool) -> Result<()> {
    let graph = load_graph(graph_path)?;
    let mgr = HistoryManager::new(gid_dir);
    let filename = mgr.save_snapshot(&graph, message)?;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "filename": filename}));
    } else {
        println!("✓ Saved snapshot: {}", filename);
    }
    Ok(())
}

fn cmd_history_diff(graph_path: &PathBuf, gid_dir: &std::path::Path, version: &str, json: bool) -> Result<()> {
    let current = load_graph(graph_path)?;
    let mgr = HistoryManager::new(gid_dir);
    let diff = mgr.diff_against(version, &current)?;
    
    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
    } else {
        println!("\n📊 Comparing {} → current\n", version);
        println!("{}", diff);
    }
    Ok(())
}

fn cmd_history_restore(graph_path: &PathBuf, gid_dir: &std::path::Path, version: &str, force: bool, json: bool) -> Result<()> {
    if !force && !json {
        println!("Warning: This will overwrite the current graph.");
        println!("Use --force to confirm.");
        return Ok(());
    }
    
    let mgr = HistoryManager::new(gid_dir);
    mgr.restore(version, graph_path)?;
    
    if json {
        println!("{}", serde_json::json!({"success": true, "restored": version}));
    } else {
        println!("✓ Restored graph from {}", version);
    }
    Ok(())
}

fn cmd_visual(path: PathBuf, format: &str, output: Option<&std::path::Path>, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;
    let fmt: VisualFormat = format.parse()?;
    let result = render(&graph, fmt);
    
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

fn cmd_advise(path: PathBuf, errors_only: bool, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;
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

fn cmd_design(requirements: Option<String>, parse: bool, graph_path: Option<PathBuf>, json: bool) -> Result<()> {
    if parse {
        // Read LLM response from stdin and parse it
        let mut response = String::new();
        io::stdin().read_to_string(&mut response)?;
        
        let graph = parse_llm_response(&response)?;
        
        if let Some(path) = graph_path {
            // Save to specified path
            save_graph(&graph, &path)?;
            if json {
                println!("{}", serde_json::json!({"success": true, "path": path.display().to_string()}));
            } else {
                println!("✓ Saved graph to {}", path.display());
            }
        } else {
            // Output as YAML
            if json {
                println!("{}", serde_json::to_string_pretty(&graph)?);
            } else {
                println!("{}", serde_yaml::to_string(&graph)?);
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

fn cmd_semantify(path: PathBuf, heuristic: bool, parse: bool, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;
    
    if heuristic {
        // Apply heuristic layer assignments
        let assigned = apply_heuristic_layers(&mut graph);
        save_graph(&graph, &path)?;
        
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
        save_graph(&graph, &path)?;
        
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

fn cmd_refactor_rename(path: PathBuf, old: &str, new: &str, apply: bool, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;
    
    if let Some(preview) = preview_rename(&graph, old, new) {
        if apply {
            if apply_rename(&mut graph, old, new) {
                save_graph(&graph, &path)?;
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

fn cmd_refactor_merge(path: PathBuf, a: &str, b: &str, new_id: &str, apply: bool, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;
    
    if let Some(preview) = preview_merge(&graph, a, b, new_id) {
        if apply {
            if apply_merge(&mut graph, a, b, new_id) {
                save_graph(&graph, &path)?;
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

fn cmd_refactor_split(path: PathBuf, node: &str, into: &[String], apply: bool, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;
    
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
                save_graph(&graph, &path)?;
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

fn cmd_refactor_extract(path: PathBuf, nodes: &[String], parent: &str, title: &str, apply: bool, json: bool) -> Result<()> {
    let mut graph = load_graph(&path)?;
    
    if let Some(preview) = preview_extract(&graph, nodes, parent, title) {
        if apply {
            if apply_extract(&mut graph, nodes, parent, title) {
                save_graph(&graph, &path)?;
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

fn cmd_plan(path: PathBuf, format: &str, json: bool) -> Result<()> {
    let graph = load_graph(&path)?;
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
    path: PathBuf,
    max_concurrent: Option<usize>,
    model: Option<String>,
    approval_mode: Option<String>,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let graph = load_graph(&path)?;
    let plan = create_plan(&graph)?;

    // Load config from .gid/execution.yml (or defaults)
    let gid_dir = path.parent().unwrap_or(std::path::Path::new("."));
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
            cmd_plan(path, "text", false)?;
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
        &gid_dir,
    ));

    // Save updated graph state
    save_graph(&graph_mut, &path)?;

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
            let _ = save_graph(&graph_mut, &path);
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

fn cmd_stats(path: PathBuf, json: bool) -> Result<()> {
    let gid_dir = path.parent().unwrap_or(std::path::Path::new("."));
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
        estimation_accuracy: 0.0, // Would need plan data to compute
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
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

fn cmd_approve(path: PathBuf, json: bool) -> Result<()> {
    let gid_dir = path.parent().unwrap_or(std::path::Path::new("."));
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

fn cmd_stop(path: PathBuf, json: bool) -> Result<()> {
    let gid_dir = path.parent().unwrap_or(std::path::Path::new("."));
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

fn cmd_code_search(dir: &PathBuf, keywords_str: &str, format_llm: Option<usize>, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let graph = CodeGraph::extract_from_dir(&dir);
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
                        NodeKind::Class => "🔷",
                        NodeKind::Function => "🔹",
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
    let graph = CodeGraph::extract_from_dir(&dir);
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
    let graph = CodeGraph::extract_from_dir(&dir);
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
                    NodeKind::Class => "🔷",
                    NodeKind::Function => "🔹",
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
    let graph = CodeGraph::extract_from_dir(&dir);
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
    let graph = CodeGraph::extract_from_dir(&dir);
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

fn cmd_code_impact(dir: &PathBuf, files_str: &str, json: bool) -> Result<()> {
    let dir = resolve_dir(dir)?;
    let graph = CodeGraph::extract_from_dir(&dir);
    let files: Vec<String> = files_str.split(',').map(|s| s.trim().to_string()).collect();

    let analysis = analyze_impact(&files, &graph);
    let formatted = format_impact_for_llm(&analysis);

    if json {
        println!("{}", serde_json::json!({
            "files_changed": files,
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
    let graph = CodeGraph::extract_from_dir(&dir);
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
    let graph = CodeGraph::extract_from_dir(&dir);
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
    let graph = CodeGraph::extract_from_dir(&dir);
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

    // Create LLM client for skill/harness phases
    let llm_client = llm_client::CliLlmClient::new().into_arc();

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
