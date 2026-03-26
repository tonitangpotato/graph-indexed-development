# gid-cli

[![crates.io](https://img.shields.io/crates/v/gid-dev-cli.svg)](https://crates.io/crates/gid-dev-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

**Rust CLI for Graph Indexed Development** — 39 commands for graph-based project management and code intelligence.

Built on [gid-core](../gid-core). All commands support `--json` for agent/script consumption.

---

## Installation

```bash
cargo install gid-dev-cli
```

Binary name is `gid`.

---

## Quick Start

```bash
# Initialize a graph in your project
gid init

# Add tasks and dependencies
gid add-node auth-service "Authentication Service" --type component
gid add-node add-oauth "Add OAuth" --status todo
gid add-edge add-oauth auth-service --relation depends_on

# View and manage tasks
gid tasks --ready
gid complete add-oauth

# Analyze your codebase
gid extract .
gid code-impact src/auth.py
gid query impact auth-service

# Visualize
gid visual --format mermaid
```

---

## Commands

### Graph Management

| Command | Description |
|---------|-------------|
| `gid init` | Initialize `.gid/graph.yml` |
| `gid read` | Dump graph as YAML/JSON |
| `gid validate` | Check for cycles, orphans, missing refs |
| `gid add-node <id> <title>` | Add a node |
| `gid remove-node <id>` | Remove a node (and edges) |
| `gid add-edge <from> <to>` | Add an edge |
| `gid remove-edge <from> <to>` | Remove an edge |
| `gid edit-graph <json>` | Batch operations (JSON array) |

### Task Tracking

| Command | Description |
|---------|-------------|
| `gid tasks` | List tasks |
| `gid tasks --ready` | Show unblocked tasks |
| `gid tasks --status todo` | Filter by status |
| `gid task-update <id> --status <s>` | Update task status |
| `gid complete <id>` | Mark done, show newly unblocked |

### Query & Analysis

| Command | Description |
|---------|-------------|
| `gid query impact <node>` | What's affected if this changes? |
| `gid query deps <node>` | Dependencies (add `--transitive`) |
| `gid query path <from> <to>` | Shortest path between nodes |
| `gid query common-cause <a> <b>` | Shared dependencies |
| `gid query topo` | Topological sort |

### Code Analysis

| Command | Description |
|---------|-------------|
| `gid extract [dir]` | Extract dependency graph from code |
| `gid analyze <file>` | Deep file analysis (functions, classes) |
| `gid schema [dir]` | Show all files/classes/functions |
| `gid file-summary <file>` | Summary of a specific file |
| `gid code-search <keywords>` | Find relevant code nodes |
| `gid code-failures --changed <nodes>` | Trace test failures |
| `gid code-symptoms <problem>` | Find symptom nodes from description |
| `gid code-trace <symptoms>` | Trace causal chains to root cause |
| `gid code-complexity <nodes>` | Assess change complexity |
| `gid code-impact <files>` | Impact analysis for file changes |
| `gid code-snippets <keywords>` | Extract relevant code snippets |

### History & Versioning

| Command | Description |
|---------|-------------|
| `gid history list` | List snapshots |
| `gid history save` | Save a snapshot |
| `gid history diff <version>` | Compare to current |
| `gid history restore <version>` | Restore a version |

### Visualization

| Command | Description |
|---------|-------------|
| `gid visual` | ASCII graph (default) |
| `gid visual --format dot` | Graphviz DOT |
| `gid visual --format mermaid` | Mermaid diagram |

### AI Integration

| Command | Description |
|---------|-------------|
| `gid design <requirements>` | Generate prompt for graph design |
| `gid design --parse` | Parse LLM response → graph |
| `gid semantify` | Generate prompt for semantic upgrade |
| `gid semantify --heuristic` | Auto-assign layers (no LLM) |
| `gid semantify --parse` | Parse LLM response → apply |
| `gid advise` | Health score and suggestions |

### Refactoring

| Command | Description |
|---------|-------------|
| `gid refactor rename <old> <new>` | Rename node (preview) |
| `gid refactor rename <old> <new> --apply` | Apply rename |
| `gid refactor merge <a> <b> <new>` | Merge two nodes |
| `gid refactor split <node> --into a,b,c` | Split a node |
| `gid refactor extract --nodes a,b --parent p` | Extract into parent |

---

## JSON Output

All commands support `--json` for programmatic use:

```bash
$ gid tasks --json
{
  "tasks": [
    {"id": "add-oauth", "title": "Add OAuth", "status": "todo", ...}
  ],
  "summary": {"total": 5, "todo": 2, "in_progress": 1, "done": 2, ...}
}

$ gid query impact auth-service --json
{
  "node": "auth-service",
  "impacted": [
    {"id": "user-controller", "title": "User Controller"},
    {"id": "api-gateway", "title": "API Gateway"}
  ]
}
```

---

## Example Workflows

### Planning a Feature

```bash
# Design the architecture
gid design "Add user notification system with email and push"
# → Generates prompt, send to LLM, then:
gid design --parse < llm-response.txt

# Check what you're working with
gid tasks --ready
gid query deps notification-service --transitive
```

### Impact Analysis Before Refactoring

```bash
# What breaks if I change auth.py?
gid code-impact src/auth.py

# Specific node impact
gid query impact AuthService

# Complexity assessment
gid code-complexity AuthService,TokenManager
```

### Debugging Test Failures

```bash
# Find symptom nodes from the error
gid code-symptoms "login fails with invalid token"

# Trace to root cause
gid code-trace test_login::test_valid_token --depth 5

# Full failure analysis
gid code-failures --changed AuthService --p2p test_login
```

### Refactoring

```bash
# Preview rename
gid refactor rename UserService AuthUserService
# → Shows what changes

# Apply
gid refactor rename UserService AuthUserService --apply

# Merge two related services
gid refactor merge EmailNotifier PushNotifier NotificationService --apply
```

---

## Global Options

| Option | Description |
|--------|-------------|
| `--graph <path>` | Path to graph file (default: auto-find `.gid/graph.yml`) |
| `--json` | Output as JSON |

---

## For the Full API

See [gid-core](../gid-core) for the underlying Rust library with all 165 public functions.

---

## License

**MIT** — See [LICENSE](../../LICENSE) for details.
