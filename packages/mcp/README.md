# GID MCP Server

Model Context Protocol server for Graph-Indexed Development.

This is a **thin wrapper** around the `gid` Rust CLI binary. All graph operations go through `gid-core` (Rust) for consistency and schema correctness.

## Prerequisites

Install the gid CLI:

```bash
cargo install gid-dev-cli
```

Verify it's installed:

```bash
gid --version
```

## Installation

```bash
npm install @gid/mcp
```

## Usage

### With mcporter

```bash
# Add to your MCP config
mcporter add gid /path/to/packages/mcp/dist/index.js

# List tools
mcporter list gid

# Call a tool
mcporter call gid.gid_read
mcporter call gid.gid_tasks
```

### Standalone

```bash
# Build
npm run build

# Run (stdio transport)
node dist/index.js
```

## Tools

All 39 tools from the original GID MCP, now backed by the Rust CLI:

### Query Operations (5)
- `gid_query_impact` - Impact analysis: what nodes are affected by a change?
- `gid_query_deps` - Get dependencies/dependents of a node
- `gid_query_common_cause` - Find shared dependencies between two nodes
- `gid_query_path` - Find path between two nodes
- `gid_query_topo` - Topological sort of the graph

### Graph Operations (11)
- `gid_read` - Read the graph (yaml/json/summary)
- `gid_init` - Initialize a new graph
- `gid_validate` - Validate graph structure
- `gid_tasks` - List tasks with status filter
- `gid_task_update` - Update task status
- `gid_add_node` - Add a node
- `gid_remove_node` - Remove a node
- `gid_add_edge` - Add an edge
- `gid_remove_edge` - Remove an edge
- `gid_edit_graph` - Batch operations (JSON array)
- `gid_complete` - Mark task done, show unblocked tasks

### Code Analysis (11)
- `gid_extract` - Extract graph from code
- `gid_analyze` - Analyze file dependencies
- `gid_code_search` - Search code by keywords
- `gid_code_failures` - Analyze test failures
- `gid_code_symptoms` - Find symptoms from problem description
- `gid_code_trace` - Trace causal chains to root causes
- `gid_code_complexity` - Assess code complexity
- `gid_code_impact` - Impact of changing files
- `gid_code_snippets` - Extract relevant code snippets
- `gid_file_summary` - File summary for AI
- `gid_schema` - Show code graph schema

### Design/AI Operations (3)
- `gid_design` - Generate LLM prompt for graph design
- `gid_semantify` - Generate prompt to semantify graph
- `gid_advise` - Validate and get improvement suggestions

### History (4)
- `gid_history_list` - List snapshots
- `gid_history_save` - Save snapshot
- `gid_history_diff` - Diff against historical version
- `gid_history_restore` - Restore historical version

### Refactor (4)
- `gid_refactor_rename` - Rename node, update edges
- `gid_refactor_merge` - Merge two nodes
- `gid_refactor_split` - Split a node
- `gid_refactor_extract` - Extract nodes to new parent

### Visual (1)
- `gid_visual` - Visualize graph (ASCII/DOT/Mermaid)

## Architecture

```
MCP Client (AI assistant)
    │
    ▼
┌─────────────────────────┐
│   GID MCP Server        │  ← This package (thin wrapper)
│   (TypeScript)          │
└────────────┬────────────┘
             │ execSync("gid --json ...")
             ▼
┌─────────────────────────┐
│   gid CLI               │  ← Rust binary
│   (gid-cli crate)       │
└────────────┬────────────┘
             │
             ▼
┌─────────────────────────┐
│   gid-core              │  ← Rust library
│   (graph engine)        │
└─────────────────────────┘
```

All graph operations go through `gid-core` in Rust, ensuring:
- Single source of truth for graph schema
- Consistent validation across CLI and MCP
- Optimal performance for large graphs
- No duplicate implementations to maintain

## Development

```bash
# Install dependencies
npm install

# Build
npm run build

# Run in dev mode
npm run dev

# Lint
npm run lint
```

## License

AGPL-3.0-or-later
