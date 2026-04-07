# Review: Unified Graph — Agent Tool API (§10 + Design + Layer -1 Tasks)

**Reviewer**: RustClaw  
**Date**: 2026-04-07  
**Scope**: GOAL-10.1–10.9, Design §Agent Tool API Changes, Tasks T-1.1–T-1.7  
**Source docs verified against**: `rustclaw/src/tools.rs`, `gid-core/src/query.rs`, `gid-core/src/graph.rs`

---

### 🔴 Critical (blocks implementation)

1. **[T-1.5] FINDING-1: deps_filtered signature mismatch** — T-1.5 description says `graph.deps_filtered(id, Some(&relations))` but actual signature is `deps_filtered(node_id: &str, transitive: bool, relations: Option<&[&str]>)` — 3 params, not 2. `transitive` is required. The tool currently passes `transitive` from user input, so the corrected call should be `engine.deps_filtered(id, transitive, Some(&relations_refs))`.  
   **Also in**: Design GOAL-10.5 section says "call `graph.deps_filtered(id, Some(&relations))`" — same error.  
   **Suggested fix**: Update T-1.5 description and Design GOAL-10.5 to use `engine.deps_filtered(id, transitive, Some(&relations_refs))`. Note the tool already has a `transitive` param — just pass it through.

---

### 🟡 Important (should fix before starting)

2. **[T-1.5] FINDING-2: QueryEngine vs Graph method call** — Both T-1.5 and design say `graph.impact_filtered()` / `graph.deps_filtered()`, but these are methods on `QueryEngine`, not `Graph`. The current tools create `let engine = QueryEngine::new(&graph);` then call `engine.impact()` / `engine.deps()`. Design and task should say `engine.impact_filtered()` / `engine.deps_filtered()`, not `graph.*`.  
   **Suggested fix**: Replace `graph.impact_filtered()` → `engine.impact_filtered()` and `graph.deps_filtered()` → `engine.deps_filtered()` in both Design and T-1.5.

3. **[T-1.4] FINDING-3: node_type filter logic has subtle issue** — Default filter described as: `node.node_type.as_deref().map_or(true, |t| ["task", "feature", "component"].contains(&t))`. This is correct for the `None` case (legacy nodes pass through), but there's a gap: what about nodes with `node_type: Some("code")` that have `source: "project"` (planned code nodes from GOAL-9.1)? They would be filtered OUT by default, even though they're project-layer nodes. Is this intentional?  
   **Suggested fix**: Either (a) explicitly note that planned code nodes require `node_type="code"` filter to see (acceptable — they're implementation artifacts), or (b) add to design that the default also includes `source == "project"` as a secondary pass. I recommend (a) — it's simpler and correct: planned code nodes are an implementation detail of the design process, not something the LLM needs to see in `gid_tasks` default view.

4. **[Design] FINDING-4: gid_add_node alias registration feasibility** — Design says "push the same tool impl twice with different names into the tools registration list." This works for the internal `ToolRegistry`, but the LLM-facing schema (sent to Claude/OpenAI) registers tools by name. Two tools with the same schema but different names would double the schema tokens. Consider whether the alias is worth the token cost. Alternative: just update `gid_add_task` description to say "can add any node type" and skip the alias entirely.  
   **Suggested fix**: Add a note to T-1.1 and design: "Evaluate token cost of dual registration. If schema size is a concern, skip the alias and only update gid_add_task's description."

5. **[T-1.7] FINDING-5: remove_node return type** — T-1.7 says `graph.remove_node(id)` "automatically cleans up all associated edges" and reports "N associated edges deleted." But checking the actual code, `remove_node` returns `Option<Node>` (the removed node), and the edge cleanup logic needs verification. Let me check — yes, `graph.remove_node()` at line 337 does `self.edges.retain(|e| e.from != id && e.to != id)`. But it doesn't return the count of removed edges. The task's output format `"Deleted node '{id}' and {N} associated edges"` would require counting edges before deletion (or comparing lengths before/after).  
   **Suggested fix**: Update T-1.7 implementation note: "Count edges before `remove_node()` via `graph.edges.iter().filter(|e| e.from == id || e.to == id).count()`, then call `remove_node()`, then report the pre-counted edge count."

---

### 🟢 Minor (can fix during implementation)

6. **[Requirements] FINDING-6: GOAL-10.1 verify condition inconsistency** — GOAL-10.1 verification says `gid_add_node` with `node_type: "code"`, but `gid_add_node` is described as an alias. If alias is not implemented (see FINDING-4), verification should use `gid_add_task` instead.  
   **Suggested fix**: Change to "Verify: calling `gid_add_task` (or `gid_add_node` alias if registered) with ..."

7. **[Design] FINDING-7: metadata merge semantics undefined for nested objects** — GOAL-10.3 says "JSON merge — new keys added, existing keys overwritten, unprovided keys preserved." This is clear for flat objects. But what about nested objects? If existing metadata is `{"a": {"b": 1, "c": 2}}` and update is `{"a": {"b": 99}}`, does `c` survive? Standard JSON merge patch (RFC 7396) would DELETE `c`. Shallow merge would replace the entire `"a"` value. Deep merge would preserve `c`.  
   **Suggested fix**: Specify in design: "Use shallow merge (top-level key merge only). Nested objects are replaced entirely, not deep-merged. This is simple and predictable."

8. **[Tasks] FINDING-8: T-1.4 depends on T0.2 for graph.project_nodes()** — T-1.4 says to update summary/count logic "consistent with `graph.summary()` changes in T0.2 / GOAL-6.8" and the design says to use `graph.project_nodes()`. But Layer -1 is described as having NO dependencies on Layers 0-4. If T-1.4 uses `graph.project_nodes()`, it depends on T0.2. Without T0.2, T-1.4 must implement its own inline filter.  
   **Suggested fix**: Clarify in T-1.4: "If T0.2 is already done, use `graph.project_nodes()`. Otherwise, implement inline filter: `node.node_type.as_deref().map_or(true, |t| ...)`. The inline filter will be replaced by `project_nodes()` in T4.2."

9. **[Requirements] FINDING-9: GOAL-10.8 gid_search — FTS not mentioned** — GOAL-10.8 describes keyword search as title matching. But gid-core's SqliteStorage already has FTS5 search capability. The design says "traverse graph.nodes and filter in memory." This is correct for YAML backend, but misses an optimization opportunity for SQLite backend.  
   **Suggested fix**: Add note to design: "For YAML backend, use in-memory filtering. For SQLite backend (future), delegate to `storage.search()` FTS5 query. P2 — not blocking."

---

### ✅ Passed Checks

- **GOAL numbering**: 10.1–10.9 sequential, no gaps ✅
- **GOAL/GUARD naming**: Only GOAL/GUARD used, no CR/INV/REQ ✅
- **Priority assignments**: P0 for add_task, add_edge, tasks filter (correct — these enable unified graph ops). P1 for update, query filter, execute removal, delete. P2 for search, get_node. ✅
- **Verify conditions**: Each GOAL has concrete verification ✅
- **Task-GOAL traceability**: Every GOAL maps to exactly one task ✅
- **File targets**: All tasks correctly identify `rustclaw/src/tools.rs` ✅
- **gid-core API existence verified**: `impact_filtered`, `deps_filtered`, `remove_node` all exist with correct signatures ✅
- **Layer -1 independence**: Confirmed — no Layer -1 task requires gid-core code changes (all in tools.rs, which just passes through to existing gid-core APIs) ✅
- **Backward compat**: All new params are optional, existing calls unaffected ✅
- **Design-task consistency**: Design describes implementation for each GOAL, tasks reference correct GOALs ✅

---

### Summary

| Severity | Count |
|----------|-------|
| 🔴 Critical | 1 (signature mismatch) |
| 🟡 Important | 4 |
| 🟢 Minor | 4 |
| ✅ Passed | 10 checks |

**Recommendation**: ~~Fix FINDING-1 (critical — wrong function signature will cause compile error). FINDING-2 and FINDING-5 should also be fixed to prevent confusion during implementation. The rest are advisory.~~

**✅ All 9 Findings Applied** (2026-04-07)

| Finding | Status | Document(s) Changed |
|---------|--------|---------------------|
| FINDING-1 | ✅ Applied | design.md §10.5, tasks.md T-1.5 |
| FINDING-2 | ✅ Applied | design.md §10.5, tasks.md T-1.5 |
| FINDING-3 | ✅ Applied | requirements.md GOAL-10.4 |
| FINDING-4 | ✅ Applied | design.md §10.1, tasks.md T-1.1 |
| FINDING-5 | ✅ Applied | design.md §10.7, tasks.md T-1.7 |
| FINDING-6 | ✅ Applied | requirements.md GOAL-10.1 |
| FINDING-7 | ✅ Applied | design.md §10.3, tasks.md T-1.3 |
| FINDING-8 | ✅ Applied | tasks.md T-1.4 |
| FINDING-9 | ✅ Applied | design.md GidSearchTool |
