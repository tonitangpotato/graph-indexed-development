# Design: Ritual v2 ← Harness Context Integration

## 1 Problem Statement

Ritual v2 (`state_machine.rs` + `v2_executor.rs`) sends `state.task.clone()` — a raw user task string — as the `context` parameter to every `RunSkill` action. This means the LLM implementing a task receives only "implement the auth middleware" with no design doc excerpts, no GOAL text, no guards, no dependency interfaces.

Meanwhile, `harness/context.rs::assemble_task_context()` already does exactly the right thing:
- Resolves `implements` edge → feature node → design doc path
- Extracts the relevant design section via `design_ref`
- Resolves GOAL text from requirements via `satisfies`
- Injects guards from graph metadata
- Collects dependency interface descriptions

The function exists and has 7 passing tests. It just isn't called by ritual v2.

## 2 Goals & Non-Goals

### Goals
- GOAL-1: The `Implementing` phase's `RunSkill` action carries assembled context (design excerpt + goals + guards + dependency info) instead of just `state.task`
- GOAL-2: Other phases (`Designing`, `WritingRequirements`) continue to use `state.task` — they need user intent, not code-level context
- GOAL-3: Context assembly is best-effort — if graph has no `implements` edges or no design docs, fall back gracefully to `state.task`
- GOAL-4: The `TaskContext` struct renders into a prompt-friendly string that the LLM can act on immediately
- GOAL-5: Review depth scales with triage size — `medium` tasks get light review (10 core checks, Sonnet, 30 iter), `large` tasks get full review (28 checks, Opus, 55 iter)

### Non-Goals
- Changing `assemble_task_context()` itself (it's already correct)
- Adding graph query enhancements (that's ISS-009)
- Multi-agent harness integration (harness already uses `assemble_task_context` directly)
- Changing the pure state machine (`transition()`) — it stays IO-free

## 3 Current Architecture

### 3.1 State Machine (pure, no IO)

```
transition(state, event) → (new_state, actions)
```

Actions include `RunSkill { name: String, context: String }`.

Currently, ALL `RunSkill` actions set `context: state.task.clone()`.

The state machine is intentionally pure — no filesystem access, no graph access. It can only use data already in `RitualState`.

### 3.2 Executor (IO layer)

`V2Executor::execute()` receives `RitualAction` and produces `RitualEvent`.

For `RunSkill`, the executor:
1. Loads the skill prompt from disk
2. Composes `full_prompt = context + skill_prompt`
3. Calls `llm.run_skill()` with tool definitions

The executor already has `self.config.project_root` — it can access the graph.

### 3.3 Harness Context (existing, working)

```rust
assemble_task_context(
    graph: &Graph,      // the gid graph (.gid/graph.yml)
    task_id: &str,      // node ID in the graph
    gid_root: &Path,    // .gid/ directory
) -> Result<TaskContext>
```

Returns `TaskContext { task_info, goals_text, design_excerpt, dependency_interfaces, guards }`.

**Problem**: ritual v2 doesn't work with individual task nodes — it operates on the whole task description. In single-LLM mode, there's no per-task graph node. In multi-agent mode, the harness handles per-task context directly.

## 4 Design

### 4.1 Strategy: Enrich Context in the Executor, Not the State Machine

The state machine stays pure. It continues to emit `RunSkill { name: "implement", context: state.task.clone() }`.

The executor intercepts `RunSkill` for the `"implement"` phase and enriches the context before passing it to the LLM.

This preserves the architecture invariant: state machine = pure function, executor = IO boundary.

### 4.2 Context Enrichment in V2Executor

Add a method to `V2Executor`:

```rust
/// Enrich the task context for implementation phases.
///
/// For "implement" skills, loads the gid graph and assembles context
/// from design docs, requirements, and guards.
///
/// Falls back to the raw task string if:
/// - No .gid/graph.yml exists
/// - Graph has no task nodes
/// - Graph has no implements edges or design docs
fn enrich_implement_context(
    &self,
    raw_context: &str,  // state.task
    state: &RitualState,
) -> String
```

Resolution chain:
1. Determine gid_root:
   - If state.target_root is Some, use {state.target_root}/.gid/
   - Else use {self.config.project_root}/.gid/
2. Load {gid_root}/graph.yml → Graph
3. Find task nodes that match the current task (see §4.3 for strategy)
4. For each task node, call `assemble_task_context(graph, task_id, gid_root)`
5. Render `TaskContext` into a prompt-friendly string
6. Combine all task contexts + the original task description

### 4.3 Task Node Discovery

In single-LLM mode, the graph might have:
- Multiple task nodes (from `gid design --parse`)
- A single "umbrella" task node
- No task nodes at all (graph only has code nodes from `gid extract`)

Discovery strategy:
1. Filter graph nodes: node_type == "task" AND status != "done"
2. For each matching node, call assemble_task_context()
3. Combine all results (multiple task contexts concatenated)

Rationale: in single-LLM mode, ALL pending tasks are relevant since the ritual is working on the whole project.

### 4.4 TaskContext Rendering

Add `impl Display for TaskContext` (or a `render_prompt()` method):

```rust
impl TaskContext {
    pub fn render_prompt(&self) -> String {
        let mut parts = Vec::new();

        // Task description
        if !self.task_info.description.is_empty() {
            parts.push(format!("## Task: {}\n{}", self.task_info.title, self.task_info.description));
        } else {
            parts.push(format!("## Task: {}", self.task_info.title));
        }

        // Design excerpt
        if let Some(ref excerpt) = self.design_excerpt {
            parts.push(format!("## Design Reference\n{}", excerpt));
        }

        // Goals
        if !self.goals_text.is_empty() {
            let goals = self.goals_text.join("\n");
            parts.push(format!("## Requirements (GOALs to satisfy)\n{}", goals));
        }

        // Guards
        if !self.guards.is_empty() {
            let guards = self.guards.join("\n");
            parts.push(format!("## Guards (invariants to preserve)\n{}", guards));
        }

        // Dependencies
        if !self.dependency_interfaces.is_empty() {
            let deps = self.dependency_interfaces.join("\n");
            parts.push(format!("## Completed Dependencies\n{}", deps));
        }

        parts.join("\n\n")
    }
}
```

**Note on prompt format**: The section headings and markdown structure specified above are at implementation-detail level for clarity. These may need tuning based on observed LLM behavior — particularly the heading levels and the "## Completed Dependencies" phrasing.

### 4.5 Integration Point

In `V2Executor::run_skill()`, before composing the full prompt:

```rust
async fn run_skill(&self, name: &str, context: &str, state: &RitualState) -> RitualEvent {
    // ← NEW: Enrich context for implement phases
    let enriched_context = if name == "implement" {
        self.enrich_implement_context(context, state)
    } else {
        context.to_string()
    };

    // ... rest of existing logic, using enriched_context instead of context
}
```

**Issue**: `run_skill` currently doesn't receive `state`. Need to thread it through.

### 4.6 Threading State to run_skill

Option A: Change `run_skill` signature to include `state: &RitualState`.
Option B: Store relevant state in `V2ExecutorConfig` when entering Implementing phase.
Option C: Pass `state` through `execute()` → `run_skill()` (already have `state` in `execute()`).

**Decision**: Option C. `execute()` already receives `state: &RitualState`. Just forward it to `run_skill()`.

Current:
```rust
pub async fn execute(&self, action: &RitualAction, state: &RitualState) -> Option<RitualEvent> {
    match action {
        RitualAction::RunSkill { name, context } => {
            Some(self.run_skill(name, context).await)  // state not passed
        }
        ...
    }
}
```

New:
```rust
RitualAction::RunSkill { name, context } => {
    Some(self.run_skill(name, context, state).await)  // state passed
}
```

### 4.7 Handling Verify-Fix Cycles

When verification fails and the state machine retries implementing, it sends:

```rust
RunSkill {
    name: "implement",
    context: format!("FIX BUILD/TEST ERROR:\n{}\n\nOriginal task: {}", stderr, state.task),
}
```

The enriched context should include the error AND the design context:

```rust
fn enrich_implement_context(&self, raw_context: &str, state: &RitualState) -> String {
    let graph_context = self.build_graph_context(state);

    match graph_context {
        Some(ctx) => format!("{}\n\n## Original Task\n{}", ctx, raw_context),
        None => raw_context.to_string(),
    }
}
```

This way, fix cycles get both the error message (from `raw_context`) AND the design context.

**Note on ordering**: Design context appears **before** the error message. This is intentional — it provides grounding context before the LLM sees the failure, which helps it understand the architectural constraints while fixing the error.

## 5 Changes Required

### 5.1 `harness/types.rs`
- Add `TaskContext::render_prompt(&self) -> String`

### 5.2 `ritual/v2_executor.rs`
- Change `run_skill` signature: add `state: &RitualState` parameter
- Update `execute()` to pass `state` to `run_skill()`
- Update `run_harness()` to pass `state` to `run_skill()` (internal call site fix)
- Add `enrich_implement_context()` method (sync fn, not async)
- Add `build_graph_context()` helper (loads graph, finds tasks, assembles) — see §5.2.1 for spec
- Add `safe_truncate()` helper for UTF-8 safe string truncation
- Use `enriched_context` in `run_skill()` for "implement" phase
- Add `review_config_for_triage_size()` → returns model, iterations, check set based on `state.triage_size`
- For review phases: inject check scope into prompt (light = 10 checks, full = 28 checks)
- Select review model from config (Sonnet for medium, Opus for large)

#### 5.2.1 build_graph_context() Specification

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

#### 5.2.2 safe_truncate() Helper

```rust
/// Safe truncation: find the nearest char boundary to avoid UTF-8 panics.
fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes { return s; }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
}
```

Note: This helper addresses a pre-existing UTF-8 safety issue in `run_planning` (line 383 of v2_executor.rs uses `&design_content[..15000]` which can panic on non-ASCII content). The enrichment code should use this helper when truncating design excerpts or context strings.

### 5.3 `harness/context.rs`
- No changes (already correct)

### 5.4 `harness/mod.rs`
- Ensure `assemble_task_context` is `pub` and accessible from `ritual` module

## 6 Error Handling

All enrichment failures are non-fatal:
- Graph file missing → return raw context
- Graph parse error → log warning, return raw context
- No task nodes → return raw context
- `assemble_task_context` error → log warning, return raw context

The enrichment is **additive** — it can only make context better, never worse.

## 7 Testing

### 7.1 Unit Tests

1. `test_enrich_with_graph_context` — mock graph with task+feature+design, verify enriched output contains design excerpt and goals
2. `test_enrich_no_graph` — no graph.yml exists, verify fallback to raw context
3. `test_enrich_no_task_nodes` — graph exists but has only code nodes, verify fallback
4. `test_enrich_with_error_context` — verify fix cycle context includes both error and design excerpt
5. `test_render_prompt` — verify TaskContext renders all sections correctly
6. `test_render_prompt_partial` — verify rendering with only some fields populated

### 7.2 Integration Tests

1. Full ritual flow test with a project that has `.gid/graph.yml` + design docs, verify the LLM receives enriched context (mock LLM client, inspect prompt)

## 8 Migration Path

This is purely additive — no existing behavior changes:
- State machine untouched
- `run_skill` signature gains one parameter (internal API, not public)
- Existing tests pass unchanged
- New tests added for enrichment logic

## 9 Review Depth Scaling by Triage Size

### 9.1 Problem

Currently, review-design skill always runs the full 28-check suite regardless of task complexity. This is overkill for small/medium tasks — a 287-line "wire function A to B" design doesn't need the same scrutiny as a 1500-line system architecture. Full reviews consume Opus + 55 iterations (~3-5 min, significant token cost) even when a quick sanity check would suffice.

The triage phase already outputs `size = "small" | "medium" | "large"`, and the state machine already uses it to skip review for incremental updates on non-large tasks. But when review IS triggered (new designs, large tasks), it always runs at maximum depth.

### 9.2 Review Depth Tiers

| Triage Size | Review Depth | Model | Iterations | Checks | When |
|---|---|---|---|---|---|
| `small` | **skip** | — | — | — | Incremental update, low risk. State machine already skips review for `updated + !large`. |
| `medium` | **light** | Sonnet | 30 | 10 core checks | New design but bounded scope (1-3 files, known patterns). |
| `large` | **full** | Opus | 55 | All 28 checks | New system, state machine, data model, cross-module refactor. |

### 9.3 Core Checks for Light Review (10 of 28)

Selected for highest bug-prevention value per check:

**Structural (2):**
- #1 Every type fully defined
- #2 Every reference resolves

**Logic (3):**
- #5 State machine invariants (if applicable)
- #6 Data flow completeness
- #7 Error handling completeness

**Type Safety (2):**
- #8 String/UTF-8 safety
- #11 Match exhaustiveness

**Architecture (1):**
- #13 Separation of concerns

**Implementability (2):**
- #21 Ambiguous prose
- #27 API compatibility

### 9.4 Implementation: Executor-Level Dispatch

The state machine stays unchanged — it already emits `RunSkill { name: "review-design", context }` when review is needed. The executor reads `state.triage_size` to configure the review sub-agent.

In `V2Executor::run_skill()`, when `name == "review-design"` or `name == "review-requirements"` or `name == "review-tasks"`:

```rust
fn review_config_for_triage_size(size: &str) -> ReviewConfig {
    match size {
        "small" => unreachable!(), // state machine skips review for small
        "medium" => ReviewConfig {
            model: "claude-sonnet-4-5-20250929",
            max_iterations: 30,
            checks: CheckSet::Light, // 10 core checks
        },
        "large" | _ => ReviewConfig {
            model: "claude-opus-4-6",
            max_iterations: 55,
            checks: CheckSet::Full, // all 28 checks
        },
    }
}
```

The executor modifies the skill prompt to include which checks to run:

```rust
let review_prompt = if config.checks == CheckSet::Light {
    format!(
        "{}\n\n## REVIEW SCOPE: LIGHT\nRun ONLY checks #1, #2, #5, #6, #7, #8, #11, #13, #21, #27.\nSkip all other checks. Write findings to file.",
        base_prompt,
    )
} else {
    base_prompt // full 28 checks as written in the skill
};
```

### 9.5 State Machine Change: Medium Tasks Should Also Review New Designs

Current logic (line ~554):
```rust
if design_was_updated && !is_large {
    // skip review
}
```

This skips review for ALL non-large updates. But a `medium` task with a **new** design (not updated) should still get a light review.

Proposed: No state machine change needed. The existing logic already handles this correctly:
- `design_was_updated && !is_large` → skip (covers small + medium updates)
- New design → always review (state machine sends RunSkill)
- Executor picks light vs full based on triage_size

The only scenario to reconsider: medium task that updates (not creates) a design. Currently skipped. This is acceptable — medium incremental changes are low risk.

### 9.6 Testing

1. `test_review_config_small` — verify small returns unreachable (state machine should never call)
2. `test_review_config_medium` — verify Sonnet + 30 iter + Light checks
3. `test_review_config_large` — verify Opus + 55 iter + Full checks
4. `test_light_review_prompt_injection` — verify only 10 check numbers appear in prompt
5. `test_review_depth_with_state` — integration: mock state with triage_size="medium", verify executor uses light config

## 10 Future Enhancements (post-ISS-009)

Once ISS-009 adds cross-layer edges:
- `enrich_implement_context` can additionally use `gid query impact` to find related code
- Can inject relevant source code snippets for the files the task will modify
- Can inject test file paths that should be updated
