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

## Packages

| Package | Language | Status | Features | Install |
|---------|----------|--------|----------|---------|
| [**gid-core**](./crates/gid-core) | Rust | ✅ **Full** (165 pub fn, 50 tests) | Library — all graph logic, code analysis, knowledge mgmt | `cargo add gid-core` |
| [**gid-cli**](./crates/gid-cli) | Rust | ✅ **Full** (39 commands) | CLI for terminal/scripts/agents | `cargo install gid-dev-cli` |
| [**@gid/mcp**](./packages/mcp) | TypeScript | ✅ **Full** (39 tools) | MCP server for AI assistants (Claude, Cursor, etc.) | `npx graph-indexed-development-mcp` |
| [**@gid/cli**](./packages/cli) | TypeScript | ⚠️ **Partial** (10/39 commands) | Lightweight CLI — **deprecated, use Rust CLI or MCP** | `npm i -g graph-indexed-development-cli` |

> **Note:** `gid-core` is the source of truth. The Rust CLI and MCP server are fully synced.
> The TypeScript CLI is outdated and not actively maintained — use the Rust CLI (`cargo install gid-dev-cli`) or MCP server instead.

### When to Use Which

| You want to... | Use | Why |
|----------------|-----|-----|
| Embed GID in a Rust project | `gid-core` | Direct library, fastest, type-safe |
| Run commands from terminal/scripts | `gid-cli` (Rust) | Full 39 commands, `--json` for agents |
| Give Claude/Cursor/VS Code GID tools | `@gid/mcp` | Auto-injects tools via MCP protocol |
| Use GID in a Rust agent harness | `gid-core` | RustClaw uses 13 built-in GID tools via gid-core |

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
gid extract .
# → Scans codebase, builds dependency graph
# → Use for impact analysis and refactoring
```

### Code Intelligence

GID parses your code with tree-sitter and builds a semantic graph:

```bash
# What breaks if I change this?
gid code-impact src/auth.py

# Search for relevant code
gid code-search "authentication,login"

# Trace test failures to root cause
gid code-trace test_auth::test_login
```

### Task Tracking

```bash
gid tasks                    # List all tasks
gid tasks --ready            # Show unblocked tasks
gid complete fix-bug-123     # Mark done, shows newly unblocked
gid task-update X --status in_progress
```

---

## MCP Server (AI Integration)

Give Claude, Cursor, or VS Code instant access to your architecture:

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
│   ├── gid-core/     # Rust core library (165 pub functions, 50 tests)
│   └── gid-cli/      # Rust CLI (39 commands)
├── packages/
│   ├── mcp/          # TypeScript MCP server (39 tools)
│   └── cli/          # TypeScript CLI (10 commands)
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
