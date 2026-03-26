# gid-core

[![crates.io](https://img.shields.io/crates/v/gid-core.svg)](https://crates.io/crates/gid-core)
[![docs.rs](https://docs.rs/gid-core/badge.svg)](https://docs.rs/gid-core)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

**Rust core library for Graph Indexed Development** — graph-based project management and code intelligence.

This is the source of truth for all GID logic. Used by [gid-cli](../gid-cli), [RustClaw](https://github.com/tonitangpotato/rustclaw), and [swebench-agent](https://github.com/tonitangpotato/swebench-agent).

---

## Installation

```toml
[dependencies]
gid-core = "0.1"
```

Or from git:
```toml
[dependencies]
gid-core = { git = "https://github.com/tonitangpotato/graph-indexed-development" }
```

---

## Quick Start

### Load and Query a Graph

```rust
use gid_core::{load_graph, Graph, NodeStatus};
use gid_core::query::QueryEngine;
use std::path::Path;

// Load the project graph
let graph = load_graph(Path::new(".gid/graph.yml"))?;

// Get ready tasks (todo with all deps done)
let ready = graph.ready_tasks();
for task in ready {
    println!("{} — {}", task.id, task.title);
}

// Query dependencies
let engine = QueryEngine::new(&graph);
let impacted = engine.impact("auth-service");  // What breaks if this changes?
let deps = engine.deps("my-task", true);       // Transitive dependencies
let path = engine.path("A", "B");              // Shortest path between nodes
```

### Create and Save a Graph

```rust
use gid_core::{Graph, Node, Edge, NodeStatus, save_graph};

let mut graph = Graph::new();

// Add nodes
graph.add_node(Node::new("auth-service", "Authentication Service"));
graph.add_node(Node::new("add-oauth", "Add OAuth support"));

// Add edges
graph.add_edge(Edge::new("add-oauth", "auth-service", "depends_on"));

// Update status
graph.update_status("add-oauth", NodeStatus::InProgress);

// Save
save_graph(&graph, Path::new(".gid/graph.yml"))?;
```

### Code Analysis (tree-sitter)

```rust
use gid_core::{CodeGraph, analyze_impact, format_impact_for_llm};
use std::path::Path;

// Extract code structure from a directory
let code_graph = CodeGraph::extract_from_dir(Path::new("./src"));

// Find relevant code for a bug description
let keywords = vec!["authentication", "login", "token"];
let relevant = code_graph.find_relevant_nodes(&keywords);

// Impact analysis — what breaks if I change these files?
let impact = analyze_impact(&["src/auth.py".to_string()], &code_graph);
println!("{}", format_impact_for_llm(&impact));

// Trace test failures to root cause
let symptoms = vec!["test_login"];
let chains = code_graph.trace_causal_chains_from_symptoms(&symptoms, 5, 10);
```

### Task Management with Knowledge

```rust
use gid_core::{Graph, Node, load_graph, save_graph};

let mut graph = load_graph(Path::new(".gid/graph.yml"))?;

// Nodes have built-in knowledge fields
if let Some(node) = graph.get_node_mut("fix-parser") {
    // Store findings (key-value discoveries)
    node.findings.insert("root_cause".into(), "race condition in tokenizer".into());
    
    // Cache file contents
    node.file_cache.insert("src/parser.rs".into(), "pub fn parse(...) { ... }".into());
    
    // Record tool usage
    node.tool_history.push(gid_core::task_graph_knowledge::ToolCallRecord {
        tool_name: "grep".into(),
        timestamp: chrono::Utc::now(),
        summary: "searched for callers of parse()".into(),
    });
}

save_graph(&graph, Path::new(".gid/graph.yml"))?;
```

---

## Modules

| Module | Description |
|--------|-------------|
| `graph` | Core types: `Graph`, `Node`, `Edge`, `NodeStatus` |
| `parser` | YAML load/save: `load_graph()`, `save_graph()` |
| `query` | Graph queries: impact, deps, path, common cause, topo sort |
| `validator` | Validation: cycles, orphans, missing refs, duplicates |
| `code_graph` | Code intelligence with tree-sitter (Python, extensible) |
| `history` | Version history: snapshots, diff, restore |
| `visual` | Visualization: ASCII, DOT, Mermaid |
| `advise` | Health analysis and improvement suggestions |
| `design` | Generate graph from natural language requirements |
| `semantify` | Upgrade file graph to semantic graph (layers, components) |
| `refactor` | Graph refactoring: rename, merge, split, extract |
| `working_mem` | Agent working memory and impact analysis |
| `task_graph_knowledge` | Per-node knowledge storage (findings, file cache, tool history) |
| `complexity` | Change complexity assessment |
| `unified` | Merge code graph + task graph |
| `ignore` | .gitignore-style pattern matching |

---

## Key Types

```rust
// Graph structure
pub struct Graph {
    pub project: Option<ProjectMeta>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

// Nodes can be tasks, components, files, etc.
pub struct Node {
    pub id: String,
    pub title: String,
    pub status: NodeStatus,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub node_type: Option<String>,
    pub layer: Option<String>,
    pub findings: HashMap<String, String>,      // Knowledge: discoveries
    pub file_cache: HashMap<String, String>,    // Knowledge: cached files
    pub tool_history: Vec<ToolCallRecord>,      // Knowledge: tool usage
}

pub enum NodeStatus {
    Todo,
    InProgress,
    Done,
    Blocked,
    Cancelled,
}

// Edges define relationships
pub struct Edge {
    pub from: String,
    pub to: String,
    pub relation: String,  // depends_on, implements, calls, tested_by, etc.
}

// Code analysis types
pub struct CodeGraph { ... }
pub struct CodeNode { id, name, kind, file_path, line, ... }
pub enum NodeKind { File, Module, Class, Function }
pub enum EdgeRelation { Imports, Calls, Contains, Inherits }
```

---

## Who Uses This

- **[gid-cli](../gid-cli)** — Command-line interface for GID
- **[RustClaw](https://github.com/tonitangpotato/rustclaw)** — Agent harness
- **[swebench-agent](https://github.com/tonitangpotato/swebench-agent)** — SWE-bench agent

---

## Stats

- 165 public functions
- 50 tests
- ~10,000 lines of Rust

---

## License

**MIT** — See [LICENSE](../../LICENSE) for details.
