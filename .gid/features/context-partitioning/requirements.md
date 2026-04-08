# Requirements: `gid context` Command (Phase 2)

## Feature Overview

The `gid context` command assembles token-budget-aware code context for AI agents. Given a set of target nodes (files, functions, classes to be changed), it traverses the graph to collect relevant dependencies, callers, and related tests, then outputs a structured context package that fits within a specified token budget. This feature is the primary motivator for the SQLite migration — it requires fast graph traversal and weighted edge queries that are infeasible with full-YAML deserialization.

*Parent: [requirements.md](requirements.md) — see GUARDs there for cross-cutting constraints.*

## Goals

### Core Context Assembly

- **GOAL-4.1** [P0]: Running `gid context --targets <node_id>[,<node_id>...] --max-tokens <N>` produces a structured output containing: (a) full details of each target node (title, file_path, signature, doc_comment, description), (b) source code for each target node read from disk at `file_path` between `start_line` and `end_line` (if those fields are populated; omitted with a note if the file is missing or lines are unavailable), (c) direct dependencies sorted by relevance (see GOAL-4.4), (d) transitive dependencies truncated to fit the token budget, (e) callers of target nodes, and (f) related test nodes. *(ref: discussion, gid context — output structure)*

- **GOAL-4.2** [P0]: The output fits within the specified `--max-tokens` budget. Token estimation counts UTF-8 bytes of the final formatted output divided by 4 (1 token ≈ 4 bytes). The `--max-tokens` budget applies to the entire output including headers, formatting, and structural overhead. The total estimated tokens of the output never exceeds the budget. The token estimation method is not configurable in v1. *(ref: discussion, gid context — token-budget-aware)*

- **GOAL-4.3** [P0]: When the full context (all targets + all deps + all callers + all tests) exceeds the token budget, the system truncates in priority order: transitive dependencies are truncated first (furthest hops removed first; within the same hop distance, lower-relevance nodes per GOAL-4.4 are removed first), then callers, then direct dependencies. Target node details are never truncated. *(ref: discussion, gid context — truncated to budget)*

### Relevance Ranking

- **GOAL-4.4** [P0]: Dependencies are ranked by relevance score. The ranking uses the following edge relation mapping:

  | Rank | Category | `edge.relation` values |
  |------|----------|------------------------|
  | 1 | Direct call | `calls`, `imports` |
  | 2 | Type reference | `type_reference`, `inherits`, `implements`, `uses` |
  | 3 | Same-file | `contains`, `defined_in` (when from_node and to_node share `file_path`) |
  | 4 | Structural | `depends_on`, `part_of`, `blocks`, `tests_for` |
  | 5 | Transitive | any relation at hop > 1 |

  Within each category, edges with higher `weight` values rank higher. Transitive edges are penalized by hop distance (relevance decreases with each hop). Edges with `confidence` values contribute to ranking as a secondary factor within the same category and weight. *(ref: discussion, gid context — relevance ranking)*

- **GOAL-4.5** [P1]: The relevance score for each included node is visible in the output, so the consuming agent can assess confidence in the context. *(ref: discussion, gid context — sorted by relevance)*

### Input Options

- **GOAL-4.6** [P0]: The `--targets` parameter accepts one or more node IDs (comma-separated or repeated flag). At least one target must be specified or the command errors with a usage message. *(ref: discussion, gid context — input: targets)*

- **GOAL-4.7** [P1]: A `--depth <N>` parameter controls maximum traversal depth for transitive dependencies (default: 3). Setting depth=1 returns only direct dependencies. Depth acts as a hard ceiling on graph traversal; the token budget (GOAL-4.2) may further reduce results below this depth. If both constraints apply, the token budget takes precedence (nodes at allowed depth are still removed if the budget is exceeded). *(ref: discussion, gid context — input: depth)*

- **GOAL-4.8** [P1]: An `--include <pattern>` parameter (repeatable) filters included nodes by file path glob or node type. For example, `--include "*.rs"` limits context to Rust files, `--include "type:function"` limits to function nodes. Valid type values match `node_type` column values: `task`, `file`, `function`, `class`, `module`, `feature`, `component`, `layer`, `knowledge`. *(ref: discussion, gid context — input: includes)*

- **GOAL-4.9** [P1]: A `--format <json|yaml|markdown>` parameter controls output format (default: markdown). JSON format outputs machine-parseable structured data. Markdown format outputs human-readable sections. YAML outputs the same structure as JSON but in YAML. *(ref: discussion, gid context — input: format)*

### Output Content

- **GOAL-4.10** [P0]: The output includes an `estimated_tokens` field showing the total estimated token count of the assembled context. This count is present regardless of output format. *(ref: discussion, gid context — output: estimated tokens)*

- **GOAL-4.11** [P1]: Each node in the output includes enough information for an AI agent to understand its role: at minimum `id`, `file_path`, `signature` (if function/class), `doc_comment` (if present), and the edge relation that connects it to the target. *(ref: discussion, gid context — output: target details + deps)*

### Multi-Surface Availability

- **GOAL-4.12** [P2]: The context assembly logic is implemented in `gid-core` as a library function (not just CLI), so it can be called from the MCP server, LSP server, and Rust crate consumers with the same parameters and behavior. The CLI is a thin wrapper over this library function. **Note**: Implementation of GOAL-4.1 through GOAL-4.11 MUST structure the code as a library function in `gid-core` with a thin CLI wrapper, even though this GOAL is P2 — the library-first architecture is a prerequisite, not extra work. *(ref: discussion, gid context — exists at all 4 surfaces: Rust crate, CLI, MCP, LSP)*

### Observability

- **GOAL-4.13** [P1]: `gid context` logs traversal statistics to stderr: nodes visited, nodes included in output, nodes excluded by `--include` filter, token budget used vs. available, and elapsed time. *(ref: review FINDING-16, observability)*

### Terminology Note

> **`node_type` vs `node_kind`**: In `--include "type:function"` and output data, `node_type` refers to the high-level graph role (`task`, `file`, `function`, `class`, `module`, `feature`, `component`, `layer`, `knowledge`). `node_kind` is a separate, finer-grained code-level construct (e.g., `Struct`, `Impl`, `Trait`, `Enum`) defined in the storage schema (GOAL-1.2). The `--include` filter operates on `node_type` only. *(ref: review FINDING-20, terminology clarification)*

**13 GOALs** (6 P0 / 6 P1 / 1 P2 + 1 terminology note)
