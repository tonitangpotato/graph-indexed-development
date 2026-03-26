# @gid/mcp

[![npm](https://img.shields.io/npm/v/graph-indexed-development-mcp)](https://www.npmjs.com/package/graph-indexed-development-mcp)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL%203.0-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)

**MCP server for Graph Indexed Development** — 39 tools for AI assistants (Claude, Cursor, VS Code).

Give your AI assistant structural awareness of your codebase. GID answers questions like:
- *"What breaks if I change UserService?"*
- *"What's the dependency path from Controller to Database?"*
- *"Design an auth system with the right layers"*

---

## Installation

### Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "gid": {
      "command": "npx",
      "args": ["graph-indexed-development-mcp"]
    }
  }
}
```

### Claude Code

```bash
claude mcp add gid -- npx graph-indexed-development-mcp
```

### Cursor / VS Code

Add to your MCP settings:

```json
{
  "gid": {
    "command": "npx",
    "args": ["graph-indexed-development-mcp"]
  }
}
```

---

## Quick Start

After installation, ask your AI assistant:

```
"Initialize a GID graph for this project"
→ Uses gid_init

"Extract the dependency graph from the codebase"
→ Uses gid_extract

"What would break if I change UserService?"
→ Uses gid_query_impact

"Design an e-commerce backend with auth, payments, orders"
→ Uses gid_design

"Show me the project health score"
→ Uses gid_advise
```

---

## Tools (39 total)

### Query & Analysis

| Tool | Description |
|------|-------------|
| `gid_query_impact` | What's affected by changing a node |
| `gid_query_deps` | Dependencies or dependents of a node |
| `gid_query_common_cause` | Shared dependencies between two nodes |
| `gid_query_path` | Dependency path between nodes |
| `gid_query_topo` | Topological sort (valid execution order) |
| `gid_analyze` | Deep analysis of file/function/class |
| `gid_get_file_summary` | Structured file analysis |
| `gid_advise` | Health score and improvement suggestions |
| `gid_get_schema` | Graph schema with available relations |

### Graph Management

| Tool | Description |
|------|-------------|
| `gid_read` | Read graph (YAML, JSON, or summary) |
| `gid_init` | Initialize a new graph |
| `gid_edit_graph` | Add/update/delete nodes and edges |
| `gid_refactor` | Rename, move, or delete nodes |
| `gid_history` | Version history: list, diff, restore |
| `gid_validate` | Check for cycles, orphans, errors |

### Task Tracking

| Tool | Description |
|------|-------------|
| `gid_tasks` | Query tasks (all, ready, by status) |
| `gid_task_update` | Toggle task completion |
| `gid_complete` | Mark task done, show unblocked |

### AI-Assisted Design

| Tool | Description |
|------|-------------|
| `gid_design` | Generate graph from natural language |
| `gid_extract` | Extract graph from existing code |
| `gid_semantify` | Upgrade to semantic graph (layers, components) |
| `gid_complete_analysis` | Analyze docs for gaps and suggestions |

### Code Analysis

| Tool | Description |
|------|-------------|
| `gid_code_search` | Search code by keywords |
| `gid_code_impact` | Impact of changing files |
| `gid_code_failures` | Trace test failures |
| `gid_code_symptoms` | Find symptom nodes from problem |
| `gid_code_trace` | Trace causal chains |
| `gid_code_complexity` | Assess change complexity |
| `gid_code_snippets` | Extract relevant snippets |

### Visualization

| Tool | Description |
|------|-------------|
| `gid_visual` | Generate visualization (ASCII, DOT, Mermaid) |

### Resources

| Resource | Description |
|----------|-------------|
| `gid://graph` | Current dependency graph (YAML) |
| `gid://health` | Health score and validation |
| `gid://features` | List of features in graph |

---

## Example Conversations

### Design First, Then Build

```
You: "Design an e-commerce backend with auth, payments, order tracking"

Claude uses gid_design →
  Created 4 features: UserAuth, Payment, OrderTracking, ProductCatalog
  Created 8 components across 4 layers
  Created 15 dependency edges
  Health score: 95/100

You: "Now implement the AuthService based on the graph"

Claude uses gid_query_deps →
  AuthService depends on: UserRepository, TokenManager
  Implements: UserAuth feature
  Layer: application

Claude generates code that fits the architecture.
```

### Impact Analysis

```
You: "I need to refactor UserService. What would break?"

Claude uses gid_query_impact →
  Direct dependents: AuthController, ProfileController, OrderService
  Affected features: UserRegistration, OrderPayment
  5 components impacted, 2 features at risk
```

### Debug Correlated Failures

```
You: "Why do OrderService and PaymentService keep failing together?"

Claude uses gid_query_common_cause →
  Shared dependency: DatabaseService
  Both services depend on it — that's likely the root cause.
```

---

## Graph Format

GID uses `.gid/graph.yml`:

```yaml
nodes:
  UserAuth:
    type: Feature
    description: User authentication
    status: active
    
  AuthService:
    type: Component
    layer: application
    path: src/services/auth.ts

edges:
  - from: AuthService
    to: UserAuth
    relation: implements
```

**Node types:** Feature, Component, Interface, Data, File, Test

**Relations:** `implements`, `depends_on`, `calls`, `reads`, `writes`, `tested_by`, plus custom

---

## Requirements

- Node.js >= 20.0.0

---

## Related

- [gid-core](../../crates/gid-core) — Rust library (full API)
- [gid-cli](../../crates/gid-cli) — Rust CLI (39 commands)
- [GID Methodology](https://github.com/tonioyeme/graph-indexed-development-principle) — Specification
- [GID Paper](https://zenodo.org/records/18425984) — Formal methodology (Zenodo)

---

## License

**AGPL-3.0** — See [LICENSE](LICENSE) for details.

For commercial licensing, see [COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md).
