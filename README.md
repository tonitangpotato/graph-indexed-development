# Graph Indexed Development (GID)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![crates.io](https://img.shields.io/crates/v/gid-core.svg)](https://crates.io/crates/gid-core)
[![npm](https://img.shields.io/npm/v/graph-indexed-development-mcp)](https://www.npmjs.com/package/graph-indexed-development-mcp)
[![Tests](https://img.shields.io/badge/tests-1%2C080_passing-brightgreen)]()

**An operating system for AI-driven software development.**

Every AI coding tool fights the same battle: *what context should the agent see?* Cursor dumps entire files. Devin guesses. Copilot Workspace hopes for the best. GID solves this with a code knowledge graph — your codebase as a queryable structure where agents trace dependencies, assess impact, and receive only the precise context they need.

**69,000 lines of Rust. 1,080 tests. Zero hand-waving.**

```
.gid/graph.yml   ← Like .git/ for version control, but for architecture intelligence
```

---

## What GID Does That Nobody Else Does

### 1. Understands Code Structure — Not Just Text

**Code Graph Engine** (11,106 lines) — tree-sitter AST parsing for Rust, TypeScript, and Python. Extracts functions, classes, modules, and their real relationships: calls, imports, inheritance, type references. Not regex. Not keyword search. Full structural understanding.

```bash
gid extract src/
# → Parses every file, builds a typed dependency graph
# → Functions know what they call, modules know what they import
```

### 2. Discovers Architecture Automatically

**Infomap Community Detection** (8,084 lines) — treats your code graph as an information flow network and runs [Infomap](https://www.mapequation.org/) to discover natural module boundaries. Seven edge-weight strategies (calls=1.0, imports=0.8, type references=0.5, co-citation=0.4, ...) capture different coupling signals. Often finds better module boundaries than humans drew.

```bash
gid analyze --dir src/
# → "Component A: auth.rs, session.rs, middleware.rs (cohesion: 0.87)"
# → "Component B: db.rs, models.rs, migrations.rs (cohesion: 0.91)"
```

### 3. Predicts Impact Before You Break Things

**Impact Analysis + Working Memory** — before changing a function, GID tells you exactly what else is affected: which callers, which modules, which tests. The Working Memory module tracks the blast radius of in-progress changes so agents don't fix A and break B.

```bash
gid query impact UserService
# → 12 callers affected across 3 modules
# → 2 hub nodes in the dependency chain
# → 4 test files need updating

gid code-impact auth.py --dir src/
# → Traces through the actual code graph, not guessing
```

### 4. Orchestrates Multi-Agent Execution

**Task Harness** (10,549 lines) — not a todo list. A full execution engine with topological scheduling (parallel what can be parallel, serialize what must be serial), critical path analysis, and automatic orphan/cycle detection. Each task gets **precise context assembly** — the harness resolves the task's graph edges to extract exactly the right design doc sections, requirement GOALs, and project guards.

```bash
gid tasks --ready
# → Shows tasks whose dependencies are all satisfied

gid complete fix-auth-bug
# → Marks done, shows what's newly unblocked
# → Updates execution state, logs telemetry
```

### 5. Enforces Development Process

**Ritual Engine** (9,945 lines) — a pure-function state machine with 14 states: `Idle → Triage → Requirements → Design → Review → Plan → Graph → Implement → Verify → Done`. The **Composer** scans your project (Has graph? Has tests? What language?) and dynamically assembles the right ritual phases. Every phase has approval gates — what needs human review gets human review.

```
Ritual flow (dynamically composed per project):

  Triage → Requirements → Design → Review → Plan → Implement → Verify → Done
     ↑                       ↓         ↑
     └── Clarification       └── Approval Gate
```

### 6. Controls Agent Access to Source Code

**Tool Gating** — no active ritual? Source code directories are write-locked. Agents must go through `design → implement → verify` before touching production code. Configuration-driven (glob/regex patterns), overridable for specific paths.

### 7. Upgrades Understanding Over Time

**Semantify** — LLM-assisted graph enrichment. Promotes file-level nodes to named components, assigns architectural layers (API / service / storage), discovers cross-cutting features. Your graph gets smarter the more you use it.

### 8. Assembles Precise Context — The Killer Feature

This is what makes GID fundamentally different from "just another code search tool."

When a sub-agent implements a task, it doesn't get the whole repo dumped into its context window. GID resolves the task's graph edges:

```
Task "add-oauth"
  → implements → auth-feature
    → design_doc → .gid/features/auth/design.md § 3.2 (OAuth Flow)
    → satisfies → GOAL-auth.3 from requirements.md
    → project guards → GUARD-1 (no plaintext secrets)
```

**Result: the agent sees only what it needs.** Not 50 files. Not the whole repo. The exact design section, the exact requirement, the exact constraints. This is why GID agents produce better code — they're not drowning in irrelevant context.

---

## Quick Start

```bash
# Install
cargo install gid-dev-cli

# Initialize in your project
cd your-project
gid init

# Option A: Top-down (design first)
gid design "E-commerce with auth, payments, orders"
# → Outputs a structured prompt. Feed to LLM, get YAML graph back.
echo "<yaml from LLM>" | gid design --parse

# Option B: Bottom-up (extract from code)
gid extract src/
# → Parses codebase with tree-sitter, builds dependency graph

# Start working
gid tasks --ready          # What can I work on?
gid query impact AuthSvc   # What breaks if I change this?
gid advise                 # How healthy is the project?
gid visual --format mermaid  # See the architecture
```

---

## Architecture

```
┌─────────────────────────────────────────────────┐
│  gid-core  (Rust, 59K lines)                    │
│                                                 │
│  ┌──────────────┐  ┌──────────────┐             │
│  │  Code Graph   │  │   Infomap    │             │
│  │  Engine       │  │  Clustering  │             │
│  │  (tree-sitter)│  │  (community  │             │
│  │              │  │   detection) │             │
│  └──────┬───────┘  └──────┬───────┘             │
│         │                 │                     │
│  ┌──────▼─────────────────▼───────┐             │
│  │      Graph (.gid/graph.yml)    │             │
│  └──────┬───────────────┬─────────┘             │
│         │               │                       │
│  ┌──────▼───────┐ ┌─────▼────────┐              │
│  │  Harness      │ │  Ritual       │             │
│  │  (execution   │ │  (state       │             │
│  │   engine)     │ │   machine)    │             │
│  └──────┬───────┘ └──────┬───────┘              │
│         │                │                       │
│  ┌──────▼────────────────▼───────┐              │
│  │  Context Assembly + Gating    │              │
│  └───────────────────────────────┘              │
│                                                 │
└────────────────────┬────────────────────────────┘
                     │
        ┌────────────┼────────────┐
        ▼            ▼            ▼
   gid CLI      MCP Server    Rust embed
   (39 cmds)    (TS wrapper)  (cargo add)
```

**One implementation. One schema. Everywhere.** The MCP server is an 850-line thin wrapper — it translates MCP tool calls to `gid --json` CLI commands. Zero graph logic duplicated.

## Packages

| Package | Version | Install |
|---------|---------|---------|
| [**gid-core**](./crates/gid-core) | v0.3.1 | `cargo add gid-core` |
| [**gid-dev-cli**](./crates/gid-cli) | v0.3.1 | `cargo install gid-dev-cli` |
| [**MCP Server**](./packages/mcp) | npm | `npx graph-indexed-development-mcp` |

### Feature Flags (gid-core)

```toml
[dependencies]
gid-core = "0.3.1"                          # Graph only (minimal)
gid-core = { version = "0.3.1", features = ["infomap"] }    # + community detection
gid-core = { version = "0.3.1", features = ["harness"] }    # + task execution engine
gid-core = { version = "0.3.1", features = ["ritual"] }     # + development pipeline
gid-core = { version = "0.3.1", features = ["full"] }       # Everything
```

---

## All 39 Commands

<details>
<summary><strong>Graph Operations</strong></summary>

`init` · `read` · `validate` · `add-node` · `remove-node` · `add-edge` · `remove-edge` · `edit-graph`

</details>

<details>
<summary><strong>Task Management</strong></summary>

`tasks` · `task-update` · `complete`

</details>

<details>
<summary><strong>Code Analysis</strong></summary>

`extract` · `analyze` · `schema` · `file-summary` · `code-search` · `code-snippets` · `code-failures` · `code-symptoms` · `code-trace` · `code-complexity` · `code-impact`

</details>

<details>
<summary><strong>Graph Queries</strong></summary>

`query impact` · `query deps` · `query path` · `query topo` · `query common-cause`

</details>

<details>
<summary><strong>AI & Design</strong></summary>

`design` · `semantify` · `advise`

</details>

<details>
<summary><strong>History & Refactoring</strong></summary>

`history list` · `history save` · `history diff` · `history restore` · `refactor rename` · `refactor merge` · `refactor split` · `refactor extract`

</details>

<details>
<summary><strong>Visualization</strong></summary>

`visual` (ASCII, DOT, Mermaid)

</details>

All commands support `--json` for machine-readable output.

---

## MCP Server (Claude, Cursor, VS Code)

Give any MCP-compatible IDE instant access to your architecture:

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

Then ask your AI:
- *"What would break if I change UserService?"* → `gid_query_impact`
- *"Show me the project health"* → `gid_advise`
- *"Design a notification system"* → `gid_design`
- *"What are the ready tasks?"* → `gid_tasks`

---

## The Graph

Every GID project has a `.gid/graph.yml`:

```yaml
project:
  name: my-app

nodes:
  - id: auth-service
    title: Authentication Service
    status: in_progress
    node_type: component
    metadata:
      design_doc: ".gid/features/auth/design.md"

  - id: add-oauth
    title: Add OAuth support
    status: todo
    metadata:
      design_ref: "3.2"        # Links to § 3.2 of the design doc
      satisfies: ["GOAL-auth.3"] # Traces to requirement

edges:
  - from: add-oauth
    to: auth-service
    relation: implements
  - from: add-oauth
    to: user-model
    relation: depends_on
```

**Nodes** are tasks, components, features, or code entities. **Edges** define relationships: `depends_on`, `implements`, `calls`, `imports`, `tested_by`.

---

## How It Compares

| Capability | GID | Cursor/Copilot | Devin | Aider |
|---|---|---|---|---|
| AST-level code graph | ✅ tree-sitter, 3 langs | ❌ text search | ❌ | ❌ |
| Automatic module discovery | ✅ Infomap clustering | ❌ | ❌ | ❌ |
| Impact analysis | ✅ graph traversal | ❌ | ❌ | ❌ |
| Precise context assembly | ✅ graph-driven | ❌ full files | ❌ repo map | ❌ repo map |
| Multi-agent task orchestration | ✅ Harness engine | ❌ | Partial | ❌ |
| Development pipeline enforcement | ✅ Ritual + Gating | ❌ | ❌ | ❌ |
| Task dependency tracking | ✅ DAG + topo sort | ❌ | ❌ | ❌ |
| Source code access control | ✅ Tool Gating | ❌ | ❌ | ❌ |

---

## Monorepo Structure

```
graph-indexed-development/
├── crates/
│   ├── gid-core/          # Core library (59K lines, 1,080 tests)
│   │   └── src/
│   │       ├── code_graph/    # tree-sitter extraction (Rust/TS/Python)
│   │       ├── infer/         # Infomap community detection
│   │       ├── harness/       # Task execution engine
│   │       ├── ritual/        # Development pipeline state machine
│   │       ├── graph.rs       # Core graph types
│   │       ├── query.rs       # Impact/deps/path queries
│   │       ├── working_mem.rs # Change blast radius tracking
│   │       └── ...
│   └── gid-cli/           # CLI binary (39 commands)
├── packages/
│   └── mcp/               # MCP server (thin TS wrapper)
├── Cargo.toml
└── package.json
```

---

## Related

- 📄 [GID Paper](https://zenodo.org/records/18425984) — Formal methodology (Zenodo)
- 📐 [GID Methodology](https://github.com/tonitangpotato/graph-indexed-development-principle) — Specification and examples

---

## License

**MIT** — See [LICENSE](LICENSE) for details.

## Author

**Toni Tang** — [@tonitangpotato](https://github.com/tonitangpotato)
