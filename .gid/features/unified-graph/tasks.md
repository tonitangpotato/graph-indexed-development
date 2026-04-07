# Task Breakdown: Unified Graph

## Implementation Order

Tasks grouped into 6 layers. Each layer can start after the previous completes.
Within a layer, tasks can be parallelized.

---

## Layer -1: Tool Prerequisites (no deps — can be done before Layer 0)

> These tasks expand the RustClaw agent tool API (`rustclaw/src/tools.rs`) to support unified graph operations. All tasks are independent of each other and independent of Layers 0–4. They can be completed before, during, or after unified graph core work.

### T-1.1: Expand gid_add_task schema
- **GOAL**: GOAL-10.1
- **Description**: Add optional parameters to `GidAddTaskTool` JSON schema: `node_type` (string), `source` (string, default "project"), `node_kind` (string), `file_path` (string), `metadata` (JSON object). Map these to `Node` struct fields when constructing the node. Update tool description to reflect it can create any node type (task, feature, component, planned code node). Regarding alias: prefer single registration of `gid_add_task` with updated description over dual registration (`gid_add_node` alias) — dual registration doubles schema tokens (~200 tokens/request) for minimal UX benefit. Only add alias if LLM testing shows confusion with the `gid_add_task` name.
- **Files**: `rustclaw/src/tools.rs`
- **Done when**: Calling `gid_add_task` with `node_type: "code"`, `node_kind: "struct"`, `status: "planned"` creates a planned code node in graph.yml with correct fields. Calling `gid_add_task` with only `id`/`title`/`status` still works (backward compat).
- **Tests**: Create a planned code node via tool, verify graph.yml contains it with correct `node_type`, `node_kind`, `source`, `status`. Create a regular task via tool, verify no regression.

### T-1.2: Expand gid_add_edge relations
- **GOAL**: GOAL-10.2
- **Description**: Change `relation` parameter in `GidAddEdgeTool` from `enum: ["depends_on", "blocks", "subtask_of", "relates_to"]` to `type: "string"` with description listing all common relation values: depends_on, blocks, subtask_of, relates_to, implements, contains, tests_for, calls, imports, defined_in, belongs_to, maps_to, overrides, inherits. No validation on the tool side — `Edge.relation` is a free-form `String` in gid-core.
- **Files**: `rustclaw/src/tools.rs`
- **Done when**: Calling `gid_add_edge` with `relation: "implements"` creates the edge. Calling with `relation: "depends_on"` still works.
- **Tests**: Add edge with "implements" relation, verify it exists in graph.yml. Add edge with "maps_to", verify. Existing "depends_on" still works.

### T-1.3: Expand gid_update_task schema
- **GOAL**: GOAL-10.3
- **Description**: Add optional parameters to `GidUpdateTaskTool` JSON schema: `tags` (array of strings), `metadata` (JSON object — shallow merge semantics: top-level keys added/overwritten/preserved, nested objects replaced entirely), `priority` (integer), `node_type` (string), `node_kind` (string). Update logic: only modify fields that are provided (`None` = no change).
- **Files**: `rustclaw/src/tools.rs`
- **Done when**: Calling with `tags: ["gate:human"]` updates the node's tags. Calling with only `status: "done"` still works without touching other fields.
- **Tests**: Update tags on existing node, verify tags changed. Update metadata with merge, verify existing metadata keys preserved. Update only status, verify other fields unchanged.

### T-1.4: Filter gid_tasks by node_type
- **GOAL**: GOAL-10.4
- **Description**: Add optional `node_type` parameter to `GidTasksTool`. Default behavior (no param): filter to show only nodes where `node_type` is `None` (legacy), `"task"`, `"feature"`, or `"component"`. When `node_type` is explicitly set: `"code"` shows only code nodes, `"all"` shows everything, any other value filters to that specific type. Update summary/count logic to only count project-layer nodes. If T0.2's `graph.project_nodes()` is available, use it; otherwise implement inline filter: `node.node_type.as_deref().map_or(true, |t| ["task", "feature", "component"].contains(&t))`. The inline filter will be replaced by `project_nodes()` when T0.2 lands (Layer -1 has no dependency on Layer 0).
- **Files**: `rustclaw/src/tools.rs`
- **Done when**: Graph with 10 tasks + 100 code nodes → `gid_tasks` (no params) shows 10, not 110. `gid_tasks(node_type="code")` shows 100. `gid_tasks(node_type="all")` shows 110.
- **Tests**: Create mixed graph (project + code nodes), verify default shows only project nodes. Verify explicit `node_type` param filters correctly. Verify summary counts only project nodes.

### T-1.5: Add relations filter to query tools
- **GOAL**: GOAL-10.5
- **Description**: Add optional `relations` parameter (array of strings) to `GidQueryImpactTool` and `GidQueryDepsTool` JSON schemas. When provided, construct `&[&str]` from the input and call `QueryEngine` methods:
  - Impact: `engine.impact_filtered(id, Some(&relations_refs))`
  - Deps: `engine.deps_filtered(id, transitive, Some(&relations_refs))` — note `deps_filtered` has 3 params (node_id, transitive, relations), pass the existing `transitive` param through
  When not provided, call the unfiltered methods `engine.impact(id)` / `engine.deps(id, transitive)` (current behavior). Add description text listing common relation values.
- **Files**: `rustclaw/src/tools.rs`
- **Done when**: `gid_query_impact(id, relations=["tests_for"])` only follows tests_for edges. `gid_query_impact(id)` (no relations) follows all edges as before.
- **Tests**: Create graph with mixed edge types. Query with relation filter, verify only matching edges traversed. Query without filter, verify all edges traversed.

### T-1.6: Remove gid_execute, enhance gid_plan
- **GOAL**: GOAL-10.6
- **Description**: Delete `GidExecuteTool` struct and its `Tool` impl entirely. Remove from tools registration list. Add optional `detail: bool` parameter to `GidPlanTool` schema. When `detail == true`, append critical path analysis and estimated turns to plan output (extract this logic from the deleted GidExecuteTool's implementation). When `detail` is false or unset, output the standard plan.
- **Files**: `rustclaw/src/tools.rs`
- **Done when**: No `gid_execute` tool exists in the tools list. `gid_plan(detail=true)` includes critical path info. `gid_plan()` works as before.
- **Tests**: Verify gid_execute is not in tool registry. Verify `gid_plan(detail=true)` output contains critical path. Verify `gid_plan()` output is standard plan.

### T-1.7: Add delete operation to gid_refactor
- **GOAL**: GOAL-10.7
- **Description**: Add `"delete"` to the `operation` enum in `GidRefactorTool` schema (alongside existing `rename`, `merge`, `update_title`). Implementation: when `operation == "delete"`, read the `id` parameter, count associated edges first via `graph.edges.iter().filter(|e| e.from == id || e.to == id).count()`, then call `graph.remove_node(id)` which removes the node and cleans up all associated edges. Return message: `"Deleted node '{id}' and {N} associated edges"` where N is the pre-counted edge count. No confirmation needed — graph is version-controlled.
- **Files**: `rustclaw/src/tools.rs`
- **Done when**: `gid_refactor(operation="delete", id="old-node")` removes the node and all edges from/to it from graph.yml. Output shows what was deleted.
- **Tests**: Create a node with edges, delete via refactor, verify node and all its edges removed from graph. Verify other nodes untouched.

---

## Layer 0: Foundation (no deps)

### T0.1: Edge.source() helper method
- **GOAL**: ADR-2 Edge Metadata convention
- **Description**: Add `pub fn source(&self) -> Option<&str>` on `Edge`. Extracts `metadata.source` from `Option<serde_json::Value>`. All existing code that checks edge metadata source should use this method.
- **Files**: `crates/gid-core/src/graph.rs`
- **Done when**: `edge.source() == Some("extract")` compiles and works; no callers need to manually dig into `metadata` JSON for source checks.
- **Tests**: Unit test for `Edge::source()` with metadata present, absent, wrong type.

### T0.2: Layer filtering helpers on Graph
- **GOAL**: GOAL-6.1, GOAL-6.8
- **Description**: Add helper methods to `Graph`:
  - `code_nodes() -> Vec<&Node>` — filter `source == Some("extract")`
  - `project_nodes() -> Vec<&Node>` — filter `source == Some("project")` OR `source == None` (backward compat, remove None branch after T4.1)
  - `code_edges() -> Vec<&Edge>` — filter `edge.source() == Some("extract")`
  - `project_edges() -> Vec<&Edge>` — filter `edge.source() != Some("extract") && != Some("auto-bridge")`
  - `bridge_edges() -> Vec<&Edge>` — filter `edge.source() == Some("auto-bridge")`
  - Update `summary()` and `ready_tasks()` to use `project_nodes()` — code nodes excluded from task stats.
- **Files**: `crates/gid-core/src/graph.rs`
- **Done when**: `graph.summary()` with 100 code nodes returns same todo/done counts as before. `graph.code_nodes()` returns only extract-sourced nodes.
- **Tests**: Unit tests with mixed graph (project + code nodes), verify each filter method.

---

## Layer 1: Extract Core (depends on Layer 0)

### T1.1: CodeGraph → Graph node/edge conversion
- **GOAL**: GOAL-1.1, GOAL-1.2, GUARD-3
- **Description**: Create `fn codegraph_to_graph_nodes(cg: &CodeGraph, project_root: &Path) -> (Vec<Node>, Vec<Edge>)` in a new module `crates/gid-core/src/unify.rs` (replaces unified.rs over time). Maps CodeNode → graph::Node with:
  - `source: Some("extract")`
  - `node_type: Some("code")`
  - `node_kind: Some(kind.to_string())` — raw NodeKind value
  - `status: Done`
  - `file_path`, `start_line`, `end_line`, `language`, `signature`, `doc_comment`, `body` — from CodeNode
  - `skip_serializing_if` on all Option fields
  Maps CodeEdge → graph::Edge with:
  - relation: snake_case (imports, calls, inherits, etc.)
  - metadata: `{"source": "extract", ...original metadata}`
- **Files**: `crates/gid-core/src/unify.rs` (new)
- **Done when**: Round-trip test — convert CodeGraph to nodes/edges, verify field mapping for every NodeKind variant.
- **Tests**: Conversion test with all node kinds; edge relation format test; metadata preservation test.

### T1.2: Extract writes to graph.yml
- **GOAL**: GOAL-1.3, GOAL-1.5, GOAL-1.6, GUARD-1, GUARD-2
- **Description**: Modify `cmd_extract` flow:
  1. Parse files → build CodeGraph (unchanged)
  2. Convert CodeGraph → (Vec<Node>, Vec<Edge>) via T1.1
  3. Load existing graph.yml
  4. Remove old code nodes: `retain(|n| n.source.as_deref() != Some("extract"))`
  5. Remove old code edges: `retain(|e| e.source() != Some("extract") && e.source() != Some("auto-bridge"))`
  6. Append new code nodes + edges
  7. Atomic write: write to `.gid/graph.yml.tmp` → rename to `graph.yml`
  - For `--force`: skip extract-meta.json, clear ALL code nodes, then rebuild
  - For incremental: use extract-meta.json node_ids to remove only changed files' nodes
  - **No longer write code_graph.json**
- **Files**: `crates/gid-cli/src/main.rs` (cmd_extract), `crates/gid-core/src/unify.rs`
- **Done when**: `gid extract` on a project with existing tasks produces graph.yml with both task and code nodes. Tasks unchanged. No code_graph.json written.
- **Tests**: Integration test — extract on fixture project, verify graph.yml contains code nodes + project nodes intact.

### T1.3: Update extract-meta.json semantics
- **GOAL**: GOAL-1.4
- **Description**: `FileState.node_ids` now refers to graph.yml node IDs (same format, just different target). Update incremental extract to:
  - Read extract-meta.json
  - For modified/deleted files: remove node_ids from graph.yml (instead of code_graph.json)
  - After extract: update extract-meta.json with new node_ids
- **Files**: `crates/gid-core/src/code_graph/extract.rs` or wherever FileState is used, `crates/gid-core/src/unify.rs`
- **Done when**: Incremental extract removes only the changed file's nodes from graph.yml, not all code nodes.
- **Tests**: Test: modify one file, re-extract, verify only that file's nodes changed.

---

## Layer 2: Auto-features (depends on Layer 1)

### T2.1: Auto-semantify after extract
- **GOAL**: GOAL-2.1, GOAL-2.2
- **Description**: After code nodes are written to graph.yml, auto-run semantify on the newly added code nodes. Add `--no-semantify` flag to skip.
- **Files**: `crates/gid-cli/src/main.rs` (cmd_extract), `crates/gid-core/src/semantify.rs`
- **Done when**: `gid extract` auto-adds `metadata["layer"]` to code nodes. `gid extract --no-semantify` skips it.
- **Tests**: Extract with semantify → code nodes have layer metadata. Extract with --no-semantify → no layer metadata.

### T2.2: Bridge edge generation
- **GOAL**: GOAL-3.1, GOAL-3.2, GOAL-3.3, GOAL-3.4
- **Description**: After extract + semantify, generate bridge edges:
  1. Delete all edges with `source == "auto-bridge"`
  2. For each feature node in graph:
     a. If `metadata["code_paths"]` exists → prefix-match code nodes' `file_path`/`id` against each path → create `maps_to` edge with confidence 1.0
     b. Else → prefix-match feature `id` against module/file node paths → create `maps_to` edge with confidence 0.8
  3. All bridge edges get `metadata: {"source": "auto-bridge"}`
  Create `fn generate_bridge_edges(graph: &Graph) -> Vec<Edge>` in `unify.rs`.
- **Files**: `crates/gid-core/src/unify.rs`
- **Done when**: Feature with `code_paths: ["src/auth"]` gets bridge edges to all code nodes under `src/auth/`. Feature without code_paths uses ID-based matching as fallback.
- **Tests**: code_paths matching test; fallback ID matching test; confidence values test; bridge edge cleanup on re-extract test.

### T2.3: --layer filter for CLI commands
- **GOAL**: GOAL-6.2, GOAL-6.3, GOAL-6.4, GOAL-6.7
- **Description**: Add `--layer <code|project|all>` to: `gid tasks` (default: project), `gid visual` (default: all), `gid read` (default: all), `gid query impact` (default: all), `gid query deps` (default: all). Filter implementation:
  - Load full graph
  - Apply node filter (code_nodes/project_nodes/all)
  - Apply edge filter (keep edges where both endpoints are visible)
  - For query commands: `--layer code` → only traverse code edges; `--layer project` → only project edges; `--layer all` → traverse all including bridge
  - Add `--type` filter for query commands (GOAL-4.3): filter results by `node_type`
- **Files**: `crates/gid-cli/src/main.rs`, `crates/gid-core/src/query.rs`
- **Done when**: `gid tasks` shows no code nodes by default. `gid visual --layer code` renders only code graph. `gid query impact task-1 --layer all` crosses into code layer via bridge edges.
- **Tests**: CLI integration tests for each --layer mode; query traversal tests with bridge edges.

### T2.4: gid stats layer breakdown
- **GOAL**: GOAL-6.6
- **Description**: Update `gid stats` to show per-layer counts:
  ```
  Project layer:  12 nodes, 18 edges
  Code layer:     147 nodes, 312 edges
  Bridge edges:   9
  Total:          159 nodes, 339 edges
  ```
  Uses the helper methods from T0.2.
- **Files**: `crates/gid-cli/src/main.rs` (cmd_stats or equivalent)
- **Done when**: `gid stats` output includes per-layer breakdown.
- **Tests**: Unit test on stats formatting with known counts.

---

## Layer 3: Migration & Deprecation (depends on Layer 1)

### T3.1: working_mem.rs migration
- **GOAL**: GOAL-8.1
- **Description**: Change `query_gid_context`, `analyze_impact`, `analyze_impact_filtered` parameters from `&CodeGraph` to `&Graph`. Internally filter for code nodes via `graph.code_nodes()`. Update `FunctionInfo::from_code_node` to accept `&Node` (graph::Node) instead of `&CodeNode`. Update `find_test_files` similarly.
- **Files**: `crates/gid-core/src/working_mem.rs`
- **Done when**: working_mem.rs has zero imports from `code_graph::types`. All callers compile.
- **Tests**: Existing working_mem tests updated to use Graph instead of CodeGraph.

### T3.2: CLI cmd_extract migration
- **GOAL**: GOAL-8.2
- **Description**: `cmd_extract` in gid-cli stops calling `build_unified_graph()`. Instead: extract → write to graph.yml (T1.2) → reload graph from graph.yml → return. Remove the `build_unified_graph` import.
- **Files**: `crates/gid-cli/src/main.rs`
- **Done when**: `cmd_extract` no longer calls `build_unified_graph`. Graph is loaded directly from graph.yml after extract.
- **Tests**: CLI e2e test: `gid extract` produces valid graph.yml.

### T3.3: harness scheduler migration
- **GOAL**: GOAL-8.3
- **Description**: `post_layer_extract` in scheduler.rs stops calling `build_unified_graph()`. Instead loads graph from graph.yml which now contains code nodes post-extract.
- **Files**: `crates/gid-core/src/harness/scheduler.rs`
- **Done when**: `post_layer_extract` no longer imports or calls `build_unified_graph`. Uses `Graph::load()` directly.
- **Tests**: Scheduler unit test with graph.yml containing code nodes.

### T3.4: ritual executor migration
- **GOAL**: GOAL-8.4
- **Description**: `executor.rs` extract phase stops calling `build_unified_graph()`. Same pattern as T3.3.
- **Files**: `crates/gid-core/src/ritual/executor.rs`
- **Done when**: Extract phase in ritual uses graph.yml directly.
- **Tests**: Ritual executor test with post-extract graph.

### T3.5: Design-to-graph writes use layer-aware merge
- **GOAL**: GOAL-6.5, GOAL-9.1, GOAL-9.2
- **Description**: `gid design --parse` and ritual `generate-graph` phase currently overwrite the entire graph.yml. In a unified graph this would destroy code nodes. Change both to layer-aware merge:
  1. Load existing graph.yml
  2. `retain` all code-layer nodes (`source == "extract"`) and bridge edges (`source == "auto-bridge"`)
  3. Replace project-layer nodes (`source == "project"` or `None`) with the LLM-generated nodes (set `source: "project"` on all new nodes)
  4. Write back atomically
  - CLI: `parse_llm_response()` in gid-cli → after deserializing the new Graph, merge instead of `save_graph()`
  - Ritual: `generate-graph` phase in v2_executor.rs → same merge logic
  - Ritual: `update-graph` phase already does append-only — verify it also preserves code nodes (add guard)
  - Extract the merge logic into a shared helper: `fn merge_project_layer(existing: &mut Graph, new_project: Graph)` in `unify.rs`
  
  **Planned code nodes**: The merge must also handle planned code nodes (`node_type: "code"`, `status: planned`, `source: "project"`). These are project-layer nodes that represent expected code structure from design. The merge logic treats them the same as task/feature nodes (they share `source: "project"`), so the existing retain/replace logic naturally covers them. Verify that planned code nodes survive both generate-graph and update-graph operations.
- **Files**: `crates/gid-core/src/unify.rs` (new helper), `crates/gid-cli/src/main.rs` (parse path), `crates/gid-core/src/ritual/v2_executor.rs`
- **Done when**: `gid design --parse` on a graph with 100 code nodes produces a graph with those 100 code nodes intact + new task nodes + planned code nodes. Same for ritual generate-graph. LLM-generated output includes planned code structure nodes for major structs/traits/modules described in design.
- **Tests**: Integration test — graph with code+project nodes → design --parse → code nodes preserved. Ritual generate-graph → code nodes preserved + planned code nodes created. Update-graph → code nodes preserved + new planned code nodes appended.

### T3.6: Generate-graph and update-graph prompt updates for planned code nodes
- **GOAL**: GOAL-9.1, GOAL-9.2
- **Description**: Update the LLM prompts used in `generate-graph` and `update-graph` ritual phases to produce planned code structure nodes alongside task/feature/component nodes.
  
  **generate-graph prompt changes**:
  - Add rule: "For major structs, traits, modules, and public functions described in the design, create planned code structure nodes with `node_type: code`, `node_kind: <kind>`, `status: planned`, `source: project`."
  - Add YAML examples showing planned code nodes (struct, trait, module, function) with correct field values
  - Add examples of `implements` edges from task nodes to planned code nodes
  - Add guidance on what qualifies: core structs ✅, public traits ✅, modules ✅, key public functions ✅, private helpers ❌, test functions ❌
  
  **update-graph prompt changes**:
  - Extend allowed node types to include `node_type: code` with `status: planned`
  - Add rule: "Preserve existing planned code nodes. For new features, create corresponding planned code structure nodes."
  - Add examples of planned code nodes in the update-graph YAML output
  
  **Naming convention**: `{kind}:{path_or_name}` — e.g. `module:src/auth`, `struct:AuthService`, `trait:Storage`, `fn:auth::login`
- **Files**: `crates/gid-core/src/ritual/v2_executor.rs` (prompt templates), or wherever generate-graph/update-graph prompts are defined
- **Done when**: Running `generate-graph` on a design that describes "AuthService struct with login() method" produces graph.yml containing `struct:AuthService` and `fn:auth::login` nodes with `status: planned`. Running `update-graph` to add a new feature produces new planned code nodes without losing existing ones.
- **Tests**: Prompt output validation — parse LLM response and verify planned code nodes are present with correct fields. Manual test with real LLM output.

### T3.7: Deprecate build_unified_graph()
- **GOAL**: GOAL-5.2, GOAL-5.3
- **Description**: After all callers migrated (T3.1-T3.4):
  - Add `#[deprecated(since = "0.X.0", note = "Use Graph directly; code nodes are now in graph.yml")]` to `build_unified_graph()`, `merge_relevant_code()`, `link_tasks_to_code()`
  - Add deprecation note to `CodeGraph` pub interface (not the type itself yet — P2)
  - Remove re-exports from `lib.rs`
- **Files**: `crates/gid-core/src/unified.rs`, `crates/gid-core/src/lib.rs`
- **Done when**: `cargo build` produces deprecation warnings only from test code, not from production code paths.
- **Tests**: Verify no production code calls deprecated functions.

### T3.8: gid schema migration
- **GOAL**: GOAL-5.4
- **Description**: `gid schema` tries to read code nodes from graph.yml first (filter `source == "extract"`). If graph has code nodes → generate schema from them. If not → fallback to running extract (backward compat).
- **Files**: `crates/gid-cli/src/main.rs` (cmd_schema)
- **Done when**: After `gid extract`, `gid schema` reads from graph.yml without re-extracting.
- **Tests**: Schema from graph.yml test; fallback to extract test.

---

## Layer 4: Backward Compat & Cleanup (depends on Layer 2 + 3)

### T4.1: One-time code_graph.json migration + source backfill
- **GOAL**: GOAL-7.1, GOAL-7.2
- **Description**: On first extract, if `code_graph.json` exists AND graph.yml has no `source == "extract"` nodes:
  1. Read code_graph.json
  2. Convert to graph nodes/edges (using T1.1 conversion)
  3. Merge into graph.yml
  4. Log: "Migrated N code nodes from code_graph.json to graph.yml"
  
  Additionally, **backfill `source` on all existing nodes**:
  5. Scan all nodes in graph.yml — any node with `source == None` gets `source: "project"` 
  6. Log: "Backfilled source on N legacy nodes"
  7. After this migration, `project_nodes()` helper can drop the `None` compat branch (TODO comment in T0.2)
  
  This runs once per project — detect via presence of `source == None` nodes or `code_graph.json`.
- **Files**: `crates/gid-core/src/unify.rs`, `crates/gid-cli/src/main.rs`
- **Done when**: Old project with code_graph.json gets code nodes in graph.yml after first extract. All nodes have explicit `source` values after migration. Old graph.yml without source fields loads fine pre-migration.
- **Tests**: Migration test with fixture code_graph.json; source backfill test; old graph.yml compatibility test.

### T4.2: RustClaw tools migration
- **GOAL**: GOAL-8.5
- **Description**: Update RustClaw tools:
  - `GidExtractTool` → use new extract (graph.yml output)
  - `GidSchemaTool` → use graph.yml-based schema
  - `GidComplexityTool` → use graph code nodes
  - `GidWorkingMemoryTool` → use migrated working_mem
- **Files**: `rustclaw/src/tools.rs` (this is in the RustClaw project, not gid-rs)
- **Done when**: All 4 tools work with unified graph. No CodeGraph direct usage.
- **Tests**: Tool function tests in RustClaw.

### T4.3: ADR-5 verification test
- **GOAL**: GOAL-4.1
- **Description**: Integration test: create graph with task→feature→code bridge path. Run `query.impact("task-1")` and verify code nodes appear in results. Run `query.deps("func:src/auth/login")` and verify feature/task nodes appear. This is a verification test, not an implementation task — ADR-5 says it should work automatically once bridge edges exist.
- **Files**: `crates/gid-core/src/query.rs` (test only)
- **Done when**: Cross-layer traversal test passes without any query engine changes.
- **Tests**: This IS the test.

### T4.4: Performance benchmark
- **GOAL**: GOAL-6.7, GUARD-4
- **Description**: Create benchmark:
  - `gid tasks` latency with 0 vs 2000 code nodes
  - `gid extract` time: graph.yml vs old code_graph.json path
  - Conditions: 500 files, ~2000 code nodes
  - Tool: `hyperfine` or criterion
  - Pass criteria: tasks latency <2x, extract time <1.2x
- **Files**: `benches/` or `tests/` in gid-core
- **Done when**: Benchmark runs and results documented.
- **Tests**: Benchmark IS the test.

---

## Summary

| Layer | Tasks | Deps | Est. Effort |
|-------|-------|------|-------------|
| -1 | T-1.1–T-1.7 | none | Small (tool schema changes) |
| 0 | T0.1, T0.2 | none | Small (helpers) |
| 1 | T1.1, T1.2, T1.3 | Layer 0 | Medium (core change) |
| 2 | T2.1–T2.4 | Layer 1 | Medium (features) |
| 3 | T3.1–T3.8 | Layer 1 | Medium (migration) |
| 4 | T4.1–T4.4 | Layer 2+3 | Small (cleanup) |

**Total**: 28 tasks across 6 layers
**Critical path**: T0.1 → T1.1 → T1.2 → T2.2 → T4.3 (5 sequential, rest parallelizable)
**Layer -1 is fully independent** — can be done before, during, or after Layers 0–4. All 7 tasks within Layer -1 are parallelizable.
**Layer 2 and 3 can run in parallel** — they share Layer 1 as dependency but don't depend on each other.
