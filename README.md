# Graph Indexed Development (GID)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![crates.io](https://img.shields.io/crates/v/gid-core.svg)](https://crates.io/crates/gid-core)
[![npm](https://img.shields.io/npm/v/graph-indexed-development-mcp)](https://www.npmjs.com/package/graph-indexed-development-mcp)

**Graph-based project management and code intelligence for AI agents and developers.**

GID gives your project a `.gid/graph.yml` — like `.git/` for version control, but for understanding your codebase architecture. AI agents can query dependencies, analyze impact, and track tasks. Humans get visualization and validation.

---

## Quick Start

```bash
# Install the CLI
cargo install gid-dev-cli

# Initialize in your project
cd your-project
gid init

# See your tasks
gid tasks

# Analyze what breaks if you change something
gid query impact UserService

# Visualize the architecture
gid visual --format mermaid
```

---

## Architecture

```
gid-core (Rust)          ← Single source of truth. All logic lives here.
    ↑
gid CLI (Rust binary)    ← 39 commands, --json output for automation
    ↑
MCP Server (TS wrapper)  ← 850-line thin wrapper, calls gid CLI via execSync
```

**One implementation. One schema. Everywhere.**

The MCP server contains zero graph logic — it translates MCP tool calls to `gid --json` CLI commands. When gid-core gets a new feature, every consumer gets it automatically.

## Packages

| Package | Language | Version | Install |
|---------|----------|---------|---------|
| [**gid-core**](./crates/gid-core) | Rust | v0.2.0 | `cargo add gid-core` |
| [**gid-dev-cli**](./crates/gid-cli) | Rust | v0.2.0 | `cargo install gid-dev-cli` |
| [**MCP Server**](./packages/mcp) | TypeScript | — | `npx graph-indexed-development-mcp` |

### When to Use Which

| You want to... | Use | Why |
|----------------|-----|-----|
| Embed GID in a Rust project | `gid-core` | Direct library, fastest, type-safe |
| Run commands from terminal/scripts | `gid` CLI | Full 39 commands, `--json` for agents |
| Give Claude/Cursor/VS Code GID tools | MCP Server | Auto-injects 39 tools via MCP protocol |

---

## The Graph

Every GID project has a `.gid/graph.yml`:

```yaml
# .gid/graph.yml
project:
  name: my-app

nodes:
  - id: auth-service
    title: Authentication Service
    status: in_progress
    node_type: component
    
  - id: add-oauth
    title: Add OAuth support
    status: todo

edges:
  - from: add-oauth
    to: auth-service
    relation: depends_on
```

**Nodes** are tasks, components, features, or files. **Edges** define relationships: `depends_on`, `implements`, `calls`, `tested_by`.

---

## Core Concepts

### Two Workflows

**Top-down:** Design first, then build
```bash
gid design "E-commerce with auth, payments, orders"
# → Generates graph with features, components, layers
# → Build against the architecture
```

**Bottom-up:** Extract from existing code
```bash
gid extract src/
# → Parses codebase with tree-sitter, builds dependency graph
# → Use for impact analysis and refactoring
```

### Code Intelligence

GID parses your code with **tree-sitter** (full AST, not regex) for Python, Rust, and TypeScript/JavaScript:

```bash
# What breaks if I change this?
gid code-impact auth.py --dir src/

# Search for relevant code
gid code-search "authentication,login" --dir src/

# Trace test failures to root cause
gid code-trace test_auth::test_login --dir src/

# Extract full code graph
gid extract src/
```

**Supported languages:**
- **Python** — classes, functions, decorators, imports, docstrings, error handling
- **Rust** — structs, enums, traits, impl blocks (with method association), modules, macros, type aliases
- **TypeScript/JavaScript** — classes (with extends), interfaces, enums, arrow functions, decorators, namespaces, type aliases

### Task Tracking

```bash
gid tasks                    # List all tasks
gid tasks --ready            # Show unblocked tasks
gid complete fix-bug-123     # Mark done, shows newly unblocked
gid task-update X --status in_progress
```

---

## All 39 Commands

### Graph Operations
`init` · `read` · `validate` · `add-node` · `remove-node` · `add-edge` · `remove-edge` · `edit-graph`

### Task Management
`tasks` · `task-update` · `complete`

### Code Analysis
`extract` · `analyze` · `schema` · `file-summary` · `code-search` · `code-snippets` · `code-failures` · `code-symptoms` · `code-trace` · `code-complexity` · `code-impact`

### Graph Queries
`query impact` · `query deps` · `query path` · `query topo` · `query common-cause`

### AI & Design
`design` · `semantify` · `advise`

### History & Refactoring
`history list` · `history save` · `history diff` · `history restore` · `refactor rename` · `refactor merge` · `refactor split` · `refactor extract`

### Visualization
`visual` (ASCII, DOT, Mermaid)

All commands support `--json` for machine-readable output.

---

## MCP Server (AI Integration)

Give Claude, Cursor, or any MCP-compatible IDE instant access to your architecture:

```json
// Claude Desktop: ~/Library/Application Support/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "gid": {
      "command": "npx",
      "args": ["graph-indexed-development-mcp"]
    }
  }
}
```

The MCP server is a thin wrapper (~850 lines) that translates tool calls to `gid --json` CLI commands. It requires the `gid` binary to be installed (`cargo install gid-dev-cli`).

Then ask Claude:
- *"What would break if I change UserService?"* → `gid_query_impact`
- *"Show me the project health"* → `gid_advise`
- *"Design a notification system"* → `gid_design`

---

## Example Session

```bash
$ gid init --name my-api
✓ Created .gid/graph.yml
  Project: my-api

$ gid add-node auth-service "Authentication Service" --type component
✓ Added node: auth-service

$ gid add-node add-oauth "Add OAuth support" --status todo
✓ Added node: add-oauth

$ gid add-edge add-oauth auth-service --relation depends_on
✓ Added edge: add-oauth → auth-service (depends_on)

$ gid tasks --ready
○ add-oauth — Add OAuth support [depends_on: auth-service]

$ gid advise
Health Score: 95/100

Suggestions:
  - [info] auth-service has no tests — consider adding test coverage
```

---

## Monorepo Structure

```
graph-indexed-development/
├── crates/
│   ├── gid-core/     # Rust core library (v0.2.0, 52 tests)
│   └── gid-cli/      # Rust CLI binary (v0.2.0, 39 commands)
├── packages/
│   └── mcp/          # MCP server (thin wrapper, ~850 lines)
├── Cargo.toml        # Rust workspace
└── package.json      # npm workspace
```

---

## Related

- [GID Methodology](https://github.com/tonioyeme/graph-indexed-development-principle) — Specification and examples
- [GID Paper](https://zenodo.org/records/18425984) — Formal methodology (Zenodo)

---

## License

**MIT** — See [LICENSE](LICENSE) for details.

---

## Author

**Toni Tang** — [@tonitangpotato](https://github.com/tonitangpotato)
