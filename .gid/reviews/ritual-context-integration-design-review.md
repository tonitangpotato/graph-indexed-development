# Review: ritual-context-integration/design.md

**Reviewer**: design-review skill (automated)
**Date**: 2026-04-07
**Document**: `.gid/features/ritual-context-integration/design.md`

---

## 🔴 Critical (blocks implementation)

### FINDING-1: [Check #8] UTF-8 unsafe byte slicing in existing executor code (pre-existing, but design perpetuates the pattern) ✅ Applied

The design's §4.2 resolution chain says "Load graph, find tasks, assemble context" — which is fine. But the **existing** `v2_executor.rs` at line 383 already has a UTF-8-unsafe byte slice:

```rust
format!("{}...\n[TRUNCATED — {} bytes total]", &design_content[..15000], design_content.len())
```

`design_content` is read from `DESIGN.md` which can contain non-ASCII (CJK characters, Unicode symbols, etc.). `&design_content[..15000]` will panic if byte 15000 falls inside a multi-byte UTF-8 sequence.

The design document doesn't introduce this bug (it's pre-existing in `run_planning`), but the design should acknowledge it or add it to scope since `enrich_implement_context` will read the same kind of content.

**Suggested fix**: Add a note in §5.2 or a guard:
```rust
// Safe truncation: find the nearest char boundary
fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes { return s; }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
}
```

### FINDING-2: [Check #6] Design references `{target_root}/.gid/graph.yml` in §4.2, but `target_root` may differ from `project_root` ✅ Applied

§4.2 says: "Load `{target_root}/.gid/graph.yml` → `Graph`"

However, looking at the actual code:
- `V2ExecutorConfig` has `project_root: PathBuf` 
- `RitualState` has `target_root: Option<String>` (which defaults to `None`)
- The executor currently uses `self.config.project_root` everywhere

The design doesn't specify **which** path to use for loading the graph. If `state.target_root` is set and differs from `config.project_root`, the wrong graph will be loaded.

**Suggested fix**: Clarify in §4.2:
```
Resolution chain:
1. Determine gid_root:
   - If state.target_root is Some, use {state.target_root}/.gid/
   - Else use {self.config.project_root}/.gid/
2. Load {gid_root}/graph.yml → Graph
```

---

## 🟡 Important (should fix before implementation)

### FINDING-3: [Check #1] `TaskContext::render_prompt()` specified in §4.4 but `TaskContext` fields need verification ✅ Applied

The design specifies `TaskContext` has fields: `task_info`, `goals_text`, `design_excerpt`, `dependency_interfaces`, `guards`.

Verified against `harness/types.rs` — **all fields match** ✅. However, the `render_prompt()` pseudocode accesses `self.task_info.title` and `self.task_info.description`. Looking at `TaskInfo`:

- `title: String` ✅
- `description: String` ✅ (not `Option<String>`, always present — defaults to empty string via `unwrap_or_default()` in `extract_task_info_from_node`)

This is correct, but the rendered output will include `## Task: \n` for tasks with no description, which is harmless but noisy.

**Suggested fix**: Add a note in §4.4:
```rust
if !self.task_info.description.is_empty() {
    parts.push(format!("## Task: {}\n{}", self.task_info.title, self.task_info.description));
} else {
    parts.push(format!("## Task: {}", self.task_info.title));
}
```

### FINDING-4: [Check #21] §4.3 Task Node Discovery is ambiguous — "by title substring" matching is underspecified ✅ Applied

§4.3 says: "Find task nodes that match the current task (by title substring or find all task nodes)"

Two problems:
1. **"or" is ambiguous** — which strategy is used when? Is it "try substring match first, then fall back to all"?
2. **Substring matching is fragile** — `state.task = "implement auth"` could match a node titled "implement authentication module" AND "implement authorization layer" — yielding unrelated contexts.
3. The final strategy says "Filter graph nodes by `node_type == "task"` with `status != "done"`" — this ignores the substring approach mentioned earlier and just returns **all** pending tasks.

Two engineers would implement this differently.

**Suggested fix**: Pick one concrete strategy and document it:
```
Discovery strategy:
1. Filter graph nodes: node_type == "task" AND status != "done"
2. For each matching node, call assemble_task_context()
3. Combine all results (multiple task contexts concatenated)

Rationale: in single-LLM mode, ALL pending tasks are relevant 
since the ritual is working on the whole project.
```

### FINDING-5: [Check #7] §4.5 `run_skill` needs `state` but the design's pseudocode has a signature mismatch ✅ Applied

§4.5 shows:
```rust
async fn run_skill(&self, name: &str, context: &str) -> RitualEvent {
    let enriched_context = if name == "implement" {
        self.enrich_implement_context(context, state).await
    } else {
        context.to_string()
    };
```

The variable `state` is used but not in the function signature. §4.6 acknowledges this and proposes Option C (pass through `execute()`), but the pseudocode in §4.5 is inconsistent with §4.6's resolution.

More importantly: `run_harness` internally calls `self.run_skill("implement", &context).await` (line 437 in v2_executor.rs) — this call site **does not have state available** and would break after the signature change.

**Suggested fix**: Update §4.5 pseudocode to match the resolved signature from §4.6:
```rust
async fn run_skill(&self, name: &str, context: &str, state: &RitualState) -> RitualEvent {
```
And document the `run_harness` call site fix:
```
§5.2 additions:
- Update run_harness() to pass state to run_skill()
```

### FINDING-6: [Check #15] §4.2 `enrich_implement_context` is marked as `async` but does only sync IO ✅ Applied

The pseudocode in §4.2 shows `enrich_implement_context` as needing to:
1. Load graph from filesystem (sync: `std::fs::read_to_string`)
2. Call `assemble_task_context` (sync: no async in the function)
3. Render to string (sync)

None of these operations require `async`. But §4.5 shows `self.enrich_implement_context(context, state).await` — implying it's async.

Making it sync is simpler and avoids unnecessary `.await`. The existing `assemble_task_context` and all graph operations are synchronous.

**Suggested fix**: Document that `enrich_implement_context` should be a sync `fn`, not `async fn`. The caller `run_skill` (which is async) can call it directly.

### FINDING-7: [Check #22] Missing helper: `build_graph_context` referenced in §4.7 but not specified ✅ Applied

§4.7 shows:
```rust
fn enrich_implement_context(&self, raw_context: &str, state: &RitualState) -> String {
    let graph_context = self.build_graph_context(state);
```

`build_graph_context` is listed in §5.2 as "Add `build_graph_context()` helper (loads graph, finds tasks, assembles)" but has no pseudocode, no signature, and no specification of what it returns.

This is the most complex function in the design (it does graph loading, task discovery, multiple `assemble_task_context` calls, and result combination) but it's the least specified.

**Suggested fix**: Add a concrete spec for `build_graph_context`:
```rust
/// Load graph and assemble context for all pending task nodes.
/// Returns None if graph doesn't exist or has no task nodes.
fn build_graph_context(&self, state: &RitualState) -> Option<String> {
    let gid_root = self.resolve_gid_root(state);
    let graph_path = gid_root.join("graph.yml");
    
    let content = std::fs::read_to_string(&graph_path).ok()?;
    let graph: Graph = serde_yaml::from_str(&content)
        .map_err(|e| tracing::warn!("Failed to parse graph: {}", e))
        .ok()?;
    
    let task_ids: Vec<&str> = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("task"))
        .filter(|n| !matches!(n.status, NodeStatus::Done))
        .map(|n| n.id.as_str())
        .collect();
    
    if task_ids.is_empty() { return None; }
    
    let contexts: Vec<String> = task_ids.iter()
        .filter_map(|id| {
            assemble_task_context(&graph, id, &gid_root)
                .map_err(|e| tracing::warn!("Failed to assemble context for {}: {}", id, e))
                .ok()
        })
        .map(|ctx| ctx.render_prompt())
        .collect();
    
    if contexts.is_empty() { None }
    else { Some(contexts.join("\n\n---\n\n")) }
}
```

### FINDING-8: [Check #14] Coupling smell — `enrich_implement_context` takes `state` but only uses `target_root` ⚠️ Not Applied

§4.7 and §4.6 thread the entire `RitualState` to `enrich_implement_context`. But looking at what it actually needs:
- `state.target_root` (to determine gid root path)
- That's it. It doesn't need `state.task`, `state.phase`, etc.

Passing the entire state creates coupling between the enrichment logic and the ritual state structure.

**Suggested fix**: Pass only what's needed:
```rust
fn enrich_implement_context(&self, raw_context: &str, gid_root: Option<&Path>) -> String
```
Or, since §4.2 says the executor already has `self.config.project_root`, consider:
```rust
fn enrich_implement_context(&self, raw_context: &str) -> String {
    let gid_root = self.config.project_root.join(".gid");
    // ...
}
```
This eliminates the need to pass `state` through `run_skill` entirely, simplifying the design significantly.

---

## 🟢 Minor (can fix during implementation)

### FINDING-9: [Check #4] Inconsistent naming: "implement" vs "Implementing" ℹ️ Acknowledged

The design uses:
- `name == "implement"` (§4.5) — the skill name
- `Implementing` phase (§2) — the state machine phase
- "implement phases" (§4.2 doc comment)
- "implementation phases" (§4.2 alt wording)

This is actually consistent with the existing codebase (skill names are lowercase, phases are CamelCase), but the design doc itself mixes "implement phase" and "Implementing phase". Minor readability issue.

### FINDING-10: [Check #20] §4.4 render_prompt is at implementation level, not design level ✅ Applied

The `render_prompt()` pseudocode in §4.4 is essentially the final implementation — it specifies exact format strings, markdown heading levels, etc. This is fine for a small function, but note that the prompt format may need tuning based on LLM behavior. Consider making the section headings configurable or at least noting they may change.

### FINDING-11: [Check #3] §4.7 `enrich_implement_context` logic order — design context before error ✅ Applied

§4.7 renders as: `"{graph_context}\n\n## Original Task\n{raw_context}"`. For verify-fix cycles, `raw_context` contains the error message. This means the design context comes **first**, then the error.

This is actually better for the LLM (design context provides grounding before the error), but it's worth confirming this ordering is intentional since the error is what needs immediate attention.

---

## 📋 Path Traces

### Context enrichment flow (happy path):
1. State machine emits `RunSkill { name: "implement", context: state.task }` ✅
2. `execute()` matches `RunSkill`, passes to `run_skill(name, context, state)` ✅
3. `run_skill` sees `name == "implement"`, calls `enrich_implement_context(context, state)` ✅
4. `enrich_implement_context` → `build_graph_context(state)` → loads `graph.yml` ✅
5. Finds task nodes with `node_type == "task"` and `status != "done"` ✅
6. For each task, calls `assemble_task_context(graph, task_id, gid_root)` ✅
7. Renders via `TaskContext::render_prompt()` ✅
8. Combines graph context + raw_context → enriched string ✅
9. `run_skill` uses enriched_context in LLM prompt ✅

### Fallback path (no graph):
1. `build_graph_context` → `read_to_string(graph.yml)` → `Err` ✅
2. Returns `None` ✅
3. `enrich_implement_context` → `None` match → returns `raw_context.to_string()` ✅
4. LLM receives original task string, same as current behavior ✅

### Verify-fix cycle path:
1. `ShellFailed` → state machine emits `RunSkill { name: "implement", context: "FIX BUILD/TEST ERROR:\n{stderr}\n\nOriginal task: {task}" }` ✅
2. `run_skill` enriches → `build_graph_context` returns `Some(ctx)` ✅
3. Result: `"{design_context}\n\n## Original Task\nFIX BUILD/TEST ERROR:\n{stderr}\n\nOriginal task: {task}"` ✅
4. LLM gets design context + error context ✅

### Non-implement phase path:
1. State machine emits `RunSkill { name: "draft-design", context: state.task }` ✅
2. `run_skill` sees `name != "implement"`, skips enrichment ✅
3. LLM receives raw task string ✅

---

## ✅ Passed Checks

- **Check #0**: Document size — 5 major sections (§3-§7), well under 8 ✅
- **Check #1**: Types fully defined — `TaskContext` verified against `harness/types.rs`, all 5 fields present ✅ (with FINDING-3 noting a minor rendering edge case)
- **Check #2**: References resolve — §3.1 references `state_machine.rs` ✅, §3.2 references `v2_executor.rs` ✅, §3.3 references `harness/context.rs` ✅, all exist and match descriptions
- **Check #3**: No dead definitions — all types/functions defined are used in the integration flow ✅
- **Check #5**: No state machine changes — design explicitly keeps transition() pure ✅ (non-goal)
- **Check #9**: Integer overflow — no new counter increments introduced ✅
- **Check #10**: Option/None handling — design uses `match graph_context { Some(ctx) => ..., None => raw_context }` ✅, no `.unwrap()` on fallible paths
- **Check #11**: Match exhaustiveness — only matching on `name == "implement"` with else fallthrough, correct ✅
- **Check #12**: Ordering sensitivity — no guard chains in the design ✅
- **Check #13**: Separation of concerns — enrichment happens in executor (IO layer), state machine stays pure ✅ (explicitly stated as design invariant)
- **Check #16**: API surface — `render_prompt()` is the only new public method on `TaskContext`, minimal surface ✅
- **Check #17**: Goals and non-goals explicit — 4 goals, 4 non-goals, all clear and non-conflicting ✅
- **Check #18**: Trade-offs documented — §4.1 explains why enrichment is in executor not state machine; §4.6 compares 3 options for threading state ✅
- **Check #19**: Cross-cutting concerns — error handling addressed in §6 (all failures non-fatal), performance implications reasonable (one graph load per implement phase) ✅
- **Check #23**: Dependency assumptions — uses only existing crate-internal modules (`harness::context`, `graph::Graph`), no external deps ✅
- **Check #24**: Migration path — §8 explicitly states "purely additive, no existing behavior changes" ✅
- **Check #25**: Testability — §7 has 6 unit tests + 1 integration test, core logic is testable with mock graph ✅
- **Check #26**: Existing functionality — `assemble_task_context` already exists and is reused, not duplicated ✅
- **Check #27**: API compatibility — `run_skill` is internal (`fn`, not `pub fn`), signature change doesn't break external callers ✅. However, `run_harness` is an internal caller that needs updating (see FINDING-5).
- **Check #28**: Feature flag / gradual rollout — design is additive with graceful fallback; no feature flag needed since enrichment is best-effort ✅

---

## Summary

- **Critical**: 2 (FINDING-1, FINDING-2)
- **Important**: 6 (FINDING-3 through FINDING-8)
- **Minor**: 3 (FINDING-9 through FINDING-11)
- **Total**: 11 findings

### Recommendation: **needs fixes first**

The design is fundamentally sound — the strategy of enriching context in the executor while keeping the state machine pure is correct. However:

1. **FINDING-2** (target_root vs project_root ambiguity) could cause context to be loaded from the wrong directory
2. **FINDING-7** (missing `build_graph_context` spec) is the most complex function and has no pseudocode
3. **FINDING-5** (`run_harness` call site breakage) would cause a compile error
4. **FINDING-8** (coupling) suggests a simpler design that avoids threading state through `run_skill` entirely

If FINDING-8's suggestion is adopted (use `self.config.project_root` directly), then FINDING-2, FINDING-5, and FINDING-6 all resolve themselves — no state threading needed, no async needed, no `run_harness` breakage.

### Estimated implementation confidence: **medium**

The design intent is clear but the key integration function (`build_graph_context`) is underspecified. An engineer would need to make several judgment calls about task discovery, multi-context combination, and path resolution.

---

## Applied Changes (2026-04-07)

### FINDING-1 ✅ Applied
- **Section**: §5.2.2
- **Change**: Added `safe_truncate()` helper specification with UTF-8 char boundary check
- **Details**: Documented pre-existing UTF-8 safety issue in `run_planning` and provided helper to prevent panics on non-ASCII content truncation

### FINDING-2 ✅ Applied
- **Section**: §4.2
- **Change**: Clarified gid_root resolution logic
- **Details**: Added explicit step 1 in resolution chain: "Determine gid_root: If state.target_root is Some, use {state.target_root}/.gid/, else use {self.config.project_root}/.gid/"

### FINDING-3 ✅ Applied
- **Section**: §4.4
- **Change**: Added guard for empty task descriptions in `render_prompt()`
- **Details**: Modified pseudocode to check `!self.task_info.description.is_empty()` before including description text

### FINDING-4 ✅ Applied
- **Section**: §4.3
- **Change**: Made task discovery strategy concrete and unambiguous
- **Details**: Replaced vague "by title substring or find all" with explicit: "Filter nodes: node_type == 'task' AND status != 'done', call assemble_task_context() for each, combine all results. Rationale: in single-LLM mode, ALL pending tasks are relevant."

### FINDING-5 ✅ Applied
- **Section**: §4.5, §5.2
- **Change**: Fixed `run_skill` signature to match §4.6 resolution
- **Details**: Updated pseudocode to `async fn run_skill(&self, name: &str, context: &str, state: &RitualState)` and removed `.await` on `enrich_implement_context` call. Added note in §5.2 to update `run_harness()` call site.

### FINDING-6 ✅ Applied
- **Section**: §4.5, §5.2
- **Change**: Marked `enrich_implement_context` as sync fn, not async
- **Details**: Removed `.await` from `enrich_implement_context` call in pseudocode, documented it as sync fn in §5.2

### FINDING-7 ✅ Applied
- **Section**: §5.2.1 (new subsection)
- **Change**: Added full `build_graph_context()` specification
- **Details**: Inserted complete pseudocode with signature, error handling, task filtering, context assembly, and result combination logic

### FINDING-8 ⚠️ Not Applied
- **Reason**: Declined to reduce coupling by passing only `gid_root` path. Current design threads full `state` to `enrich_implement_context` for flexibility. Future enhancements may need other state fields. Coupling is acceptable for this design iteration.

### FINDING-9 ℹ️ Acknowledged
- **Change**: None (informational)
- **Details**: Confirmed naming is consistent with existing codebase conventions (skill names lowercase, phases CamelCase). Minor doc wording variance is acceptable.

### FINDING-10 ✅ Applied
- **Section**: §4.4
- **Change**: Added note that prompt format may need tuning
- **Details**: Appended note after pseudocode: "The section headings and markdown structure specified above are at implementation-detail level for clarity. These may need tuning based on observed LLM behavior."

### FINDING-11 ✅ Applied
- **Section**: §4.7
- **Change**: Added note confirming context ordering (design before error) is intentional
- **Details**: Appended explanation: "Design context appears **before** the error message. This is intentional — it provides grounding context before the LLM sees the failure."

### Summary
- **Applied**: 9/11 findings
- **Not Applied**: 1 (FINDING-8: coupling — deliberate design decision)
- **Acknowledged**: 1 (FINDING-9: naming — already consistent with codebase)

All critical and important findings addressed. Design is now ready for implementation.
