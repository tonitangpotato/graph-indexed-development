# @gid/cli

[![npm](https://img.shields.io/npm/v/graph-indexed-development-cli)](https://www.npmjs.com/package/graph-indexed-development-cli)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL%203.0-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)

**TypeScript CLI for Graph Indexed Development.**

> ⚠️ **Deprecated:** Only 10/39 commands implemented. Use the [Rust CLI](../../crates/gid-cli) (`cargo install gid-dev-cli`) or [MCP server](../../packages/mcp) instead. This package is no longer actively maintained.

---

## Installation

```bash
npm install -g graph-indexed-development-cli
```

Or use directly with npx:

```bash
npx graph-indexed-development-cli <command>
```

---

## Commands

| Command | Description |
|---------|-------------|
| `gid init` | Initialize a new `.gid/graph.yml` |
| `gid extract [dirs...]` | Extract dependency graph from TypeScript/JavaScript |
| `gid query impact <node>` | Analyze what's affected by changes |
| `gid query deps <node>` | Show dependencies of a node |
| `gid query path <from> <to>` | Find path between nodes |
| `gid advise` | Validate graph and suggest improvements |
| `gid visual` | Open interactive visualization in browser |
| `gid semantify` | Upgrade to semantic graph (layers, components) |
| `gid design` | AI-assisted graph design |
| `gid analyze <file>` | Deep file analysis |

---

## Quick Start

```bash
# Initialize in your project
cd your-project
gid init

# Extract dependencies from code
gid extract .

# What breaks if I change this?
gid query impact UserService

# Visualize
gid visual
```

---

## When to Use This vs Rust CLI

| Use TypeScript CLI | Use Rust CLI |
|-------------------|--------------|
| Quick JavaScript/TypeScript projects | Full feature set (39 commands) |
| Don't want to install Rust | Need all code analysis commands |
| Simpler scripting needs | Performance-critical workloads |

For the full feature set, install the Rust CLI:

```bash
cargo install gid-dev-cli
```

---

## Related

- [gid-cli (Rust)](../../crates/gid-cli) — Full CLI with 39 commands
- [gid-core](../../crates/gid-core) — Rust library
- [@gid/mcp](../mcp) — MCP server for AI assistants

---

## License

**AGPL-3.0** — See [LICENSE](LICENSE) for details.

For commercial licensing, see [COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md).
