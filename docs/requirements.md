# Requirements: GID Task Execution Harness

## Overview

A deterministic, algorithm-driven task execution engine that reads a GID graph's task topology and orchestrates sub-agents to implement them. The harness replaces LLM-based "what to do next" decisions with code-driven topological scheduling, enabling parallel execution, verification, and adaptive re-planning. This is Phase 4-7 of the GID pipeline: Idea → Requirements → Design → **Graph → Plan → Execute → Verify**.

The harness addresses critical sub-agent efficiency problems: token waste from repeated context loading, lack of parallelism, missing verification, and granularity mismatches between project scope and task atomicity.

## Priority Levels

- **P0**: Core — required for the harness to function at all
- **P1**: Important — needed for production-quality execution
- **P2**: Enhancement — improves efficiency, UX, or observability

## Guard Severity

- **hard**: Violation = system is broken, execution must stop
- **soft**: Violation = degraded quality, should warn but can continue

## Goals

### Core Planner (Module 1)

#### Topology Analysis
- **GOAL-1.1** [P0]: Topology analyzer detects dependency cycles and rejects cyclic graphs with cycle path details *(ref: DESIGN, Architecture/Topology Analyzer)*
- **GOAL-1.2** [P0]: Topology analyzer groups tasks into execution layers where layer N depends only on layers 0..N-1, enabling parallel execution within layers *(ref: DESIGN, Architecture/Topology Analyzer)*
- **GOAL-1.3** [P1]: Topology analyzer computes critical path (longest dependency chain) to identify execution bottleneck *(ref: DESIGN, Architecture/Execution Planner)*
- **GOAL-1.4** [P1]: Topology analyzer detects orphan tasks (no edges) and reports them as warnings *(ref: DESIGN, Architecture/Topology Analyzer)*

#### Execution Plan Generation
- **GOAL-1.5** [P0]: `create_plan(graph)` returns ExecutionPlan with layers, critical path, total tasks, and estimated total turns *(ref: DESIGN, Architecture/Execution Planner)*
- **GOAL-1.6** [P0]: ExecutionPlan includes per-task metadata: id, title, description, goals, verify command, estimated_turns, dependencies *(ref: DESIGN, Architecture/Execution Planner)*
- **GOAL-1.7** [P1]: ExecutionPlan includes per-layer checkpoint commands for validation between layers *(ref: DESIGN, Architecture/Execution Planner)*
#### Context Assembly
- **GOAL-1.8** [P0]: `assemble_task_context()` resolves the task's parent feature node to locate the correct design.md and requirements.md (via feature's `design_doc` metadata → `.gid/features/{name}/`; fallback to `.gid/` root) *(ref: DESIGN, File Structure/Multi-feature)*
- **GOAL-1.9** [P0]: `assemble_task_context()` extracts design section text from the resolved design.md using `design_ref` metadata (e.g., "3.2" → section 3.2 content, captured until next same-or-higher-level heading) *(ref: DESIGN, Context Assembly/Design Reference Extraction)*
- **GOAL-1.10** [P0]: `assemble_task_context()` resolves `satisfies` metadata to full goal text from the resolved requirements.md *(ref: DESIGN, Context Assembly/Strategy 1)*
- **GOAL-1.11** [P1]: `assemble_task_context()` collects dependency task outputs/interfaces via `depends_on` edges *(ref: DESIGN, Context Assembly/Strategy 1)*
- **GOAL-1.12** [P0]: `assemble_task_context()` includes all project-level guards in every task context *(ref: DESIGN, Invariant Verification/Guards in Sub-agent Context)*
- **GOAL-1.13** [P1]: Design reference extraction handles edge cases: section "3" captures all subsections; missing section returns None with warning; multiple matches take first *(ref: DESIGN, Design Reference Extraction/Edge Cases)*

#### Schema Extensions
- **GOAL-1.14** [P0]: Task nodes support `metadata.satisfies` array referencing GOAL IDs from requirements.md *(ref: DESIGN, Graph Schema Extensions)*
- **GOAL-1.15** [P0]: Task nodes support `metadata.design_ref` string pointing to numbered design doc sections *(ref: DESIGN, Graph Schema Extensions)*
- **GOAL-1.16** [P0]: Task nodes support `metadata.verify` command string for per-task verification *(ref: DESIGN, Graph Schema Extensions)*
- **GOAL-1.17** [P1]: Task nodes support `metadata.estimated_turns` integer for execution planning *(ref: DESIGN, Graph Schema Extensions)*
- **GOAL-1.18** [P0]: Feature nodes support `metadata.goals` array of numbered goal strings (GOAL-X.Y format) *(ref: DESIGN, Graph Schema Extensions)*
- **GOAL-1.19** [P1]: Feature nodes support `metadata.design_doc` string identifying the feature's doc directory under `.gid/features/` *(ref: DESIGN, File Structure/Multi-feature)*
- **GOAL-1.20** [P0]: Graph root supports `metadata.guards` array of invariant strings (GUARD-X format) *(ref: DESIGN, Graph Schema Extensions)*

### Execution Engine (Module 2)

#### Parallel Execution
- **GOAL-2.1** [P0]: Scheduler executes tasks within a layer in parallel up to `max_concurrent` limit (configurable, default 3) *(ref: DESIGN, Architecture/Scheduler)*
- **GOAL-2.2** [P0]: Scheduler only schedules tasks whose `depends_on` edges all point to `done` tasks *(ref: DESIGN, Architecture/Scheduler)*
- **GOAL-2.3** [P2]: Scheduler can eagerly schedule next-layer tasks when their specific dependencies complete, not waiting for full layer completion *(ref: DESIGN, Architecture/Scheduler)*
- **GOAL-2.4** [P0]: Scheduler respects API rate limits (429 responses) by pausing new spawns, waiting for cooldown, then resuming *(ref: DESIGN, Error Handling)*

#### Git Worktree Isolation
- **GOAL-2.5** [P0]: Each parallel sub-agent executes in an isolated git worktree on a unique branch (`gid/task-{id}`) *(ref: DESIGN, Git Worktree Strategy)*
- **GOAL-2.6** [P0]: Worktrees are created in an isolated temporary directory branched from latest main *(ref: DESIGN, Git Worktree Strategy)*
- **GOAL-2.7** [P0]: After task completion and verification, worktree is merged to main with `--no-ff` and then removed *(ref: DESIGN, Git Worktree Strategy)*
- **GOAL-2.8** [P0]: Merge conflicts trigger `needs_resolution` state, pause execution, and report to caller *(ref: DESIGN, Git Worktree Strategy/Merge Ordering)*
- **GOAL-2.9** [P0]: Failed task verification discards worktree without merging (natural rollback) *(ref: DESIGN, Architecture/Executor)*
- **GOAL-2.10** [P0]: Merges within a layer are serialized (one at a time) to prevent race conditions *(ref: DESIGN, Merge Ordering/Within a Layer)*
- **GOAL-2.11** [P1]: Before merging, worktree is rebased on latest main; rebase conflicts trigger `needs_resolution` *(ref: DESIGN, Merge Ordering/Within a Layer)*
- **GOAL-2.12** [P1]: Sequential tasks (single dependency chain, no parallelism) skip worktree creation and run directly on main *(ref: DESIGN, Git Worktree Strategy/Sequential tasks)*

#### Task State Management
- **GOAL-2.13** [P0]: Tasks transition through states: `todo` → `in_progress` → `done` | `failed` | `needs_resolution` | `blocked` *(ref: DESIGN, Task State Machine)*
- **GOAL-2.14** [P0]: Failed tasks are retried up to `max_retries` (default 1) with enhanced prompt including failure context *(ref: DESIGN, Architecture/Scheduler)*
- **GOAL-2.15** [P1]: Sub-agents producing no output trigger retry with enhanced prompt before marking failed *(ref: DESIGN, Error Handling)*
- **GOAL-2.16** [P0]: When a task fails and retries are exhausted, all dependent tasks are marked `blocked` *(ref: DESIGN, Architecture/Scheduler)*
- **GOAL-2.17** [P0]: Graph state updates (status changes) are persisted to `.gid/graph.yml` after each transition *(ref: DESIGN, Recovery)*

#### Code Graph Synchronization
- **GOAL-2.18** [P0]: After all tasks in a layer merge to main, harness runs `gid extract` on the project source directory to update code nodes (file, class, function) in the graph *(ref: DESIGN, §3.4/Post-layer Extract)*
- **GOAL-2.19** [P1]: Post-layer extract merges code nodes into the existing graph, preserving all semantic nodes (feature, task) and only updating structural nodes *(ref: DESIGN, §3.4/Post-layer Extract)*
- **GOAL-2.20** [P1]: Extract uses file path as the deduplication key — re-extracting the same file updates the existing node rather than creating a duplicate *(ref: DESIGN, §3.4/Post-layer Extract)*
- **GOAL-2.21** [P0]: After full execution completes (all layers done), harness runs `gid advise` as a final quality check and logs the result *(ref: DESIGN, §3.7/Post-execution Advise)*
- **GOAL-2.22** [P1]: Post-execution advise failure (non-passing score) is logged as a warning but does not revert completed work *(ref: DESIGN, §3.7/Post-execution Advise)*

#### Verification
- **GOAL-2.23** [P0]: After sub-agent completes, harness runs task's `metadata.verify` command in the worktree before merging *(ref: DESIGN, Architecture/Executor)*
- **GOAL-2.24** [P0]: After all tasks in a layer merge, harness runs layer checkpoint command on main branch *(ref: DESIGN, Architecture/Scheduler/Verifier)*
- **GOAL-2.25** [P1]: Default checkpoint is language-appropriate: `cargo check` (Rust), `npm test` (Node), `pytest` (Python) *(ref: DESIGN, Architecture/Verifier)*
- **GOAL-2.26** [P1]: Custom checkpoint commands can be specified in graph metadata or `.gid/execution.yml` *(ref: DESIGN, Architecture/Verifier)*
- **GOAL-2.27** [P1]: Guards can be mapped to executable checks via `invariant_checks` in `.gid/execution.yml` (command + expected output pattern); unmapped guards serve as documentation constraints in sub-agent prompts *(ref: DESIGN, Invariant Verification/When INVs Are Checked)*

#### Sub-agent Spawning
- **GOAL-2.28** [P0]: Sub-agents receive focused system prompt with no workspace files (SOUL/AGENTS/USER/MEMORY excluded) *(ref: DESIGN, Sub-agent System Prompt)*
- **GOAL-2.29** [P0]: Sub-agent context includes: task description, goals, design excerpt, verify command, worktree path, project guards *(ref: DESIGN, Sub-agent System Prompt)*
- **GOAL-2.30** [P1]: Sub-agents have restricted tool access: read_file, write_file, edit_file, exec only (no web, message, gid, engram tools) *(ref: DESIGN, Sub-agent System Prompt)*
- **GOAL-2.31** [P1]: Sub-agents have configurable max_iterations limit (default 80) *(ref: DESIGN, RustClaw Integration)*
- **GOAL-2.32** [P1]: Sub-agents that cannot complete a task due to missing dependencies or unclear requirements report blockers clearly; the harness feeds this into adaptive re-planning *(ref: DESIGN, Sub-agent System Prompt/Rules)*

### Adaptive Re-planning (Module 3)

#### Failure Analysis
- **GOAL-3.1** [P0]: When a task fails, harness captures failure reason from sub-agent output (including blocker reports) and provides to main agent for analysis *(ref: DESIGN, Adaptive Re-planning)*
- **GOAL-3.2** [P0]: Main agent (LLM) analyzes failures and decides: retry, add new tasks, modify dependencies, or escalate to human *(ref: DESIGN, Adaptive Re-planning)*

#### Graph Modification
- **GOAL-3.3** [P0]: Main agent can add new tasks mid-execution via `gid_add_task` *(ref: DESIGN, Adaptive Re-planning)*
- **GOAL-3.4** [P0]: Main agent can add new dependency edges mid-execution via `gid_add_edge` *(ref: DESIGN, Adaptive Re-planning)*
- **GOAL-3.5** [P0]: After graph modification, `create_plan()` is called again to incorporate new tasks into execution plan *(ref: DESIGN, Adaptive Re-planning)*
- **GOAL-3.6** [P0]: Re-planning skips already-completed tasks and only schedules new or remaining tasks *(ref: DESIGN, Adaptive Re-planning)*

#### Re-planning Limits
- **GOAL-3.7** [P1]: Maximum re-plans per execution is configurable (default 3) *(ref: DESIGN, Adaptive Re-planning/Limits)*
- **GOAL-3.8** [P0]: If re-plan count exceeds limit, execution pauses and reports to human for intervention *(ref: DESIGN, Adaptive Re-planning/Limits)*
- **GOAL-3.9** [P1]: All re-planning events (new tasks added, edges modified) are logged in execution telemetry *(ref: DESIGN, Adaptive Re-planning/Limits)*

### Recovery & Observability (Module 4)

#### Execution Telemetry
- **GOAL-4.1** [P0]: All execution events are logged to `.gid/execution-log.jsonl` in append-only JSONL format *(ref: DESIGN, Observability)*
- **GOAL-4.2** [P0]: Telemetry includes events: plan, task_start, task_done, task_failed, checkpoint, re-plan, complete *(ref: DESIGN, Observability)*
- **GOAL-4.3** [P1]: Each event includes timestamp, event type, task ID (if applicable), and event-specific metadata (turns, tokens, duration, verify result) *(ref: DESIGN, Observability)*

#### Statistics & Reporting
- **GOAL-4.4** [P1]: `gid stats` command displays execution summary: tasks completed/failed, total/avg turns, total/avg tokens, duration, estimation accuracy *(ref: DESIGN, Observability)*
- **GOAL-4.5** [P1]: Statistics are queryable from `.gid/execution-log.jsonl` without external database *(ref: DESIGN, Observability)*

#### Crash Recovery
- **GOAL-4.6** [P0]: After crash/interruption, `gid execute` resumes by inspecting graph state and surviving worktrees *(ref: DESIGN, Recovery)*
- **GOAL-4.7** [P0]: Tasks marked `done` are skipped on resume *(ref: DESIGN, Recovery)*
- **GOAL-4.8** [P0]: Surviving worktrees are inspected: if verify passes → merge and mark done; if verify fails → discard and reset to `todo` *(ref: DESIGN, Recovery)*
- **GOAL-4.9** [P0]: Running `gid execute` multiple times on the same graph is idempotent — done tasks are never re-executed *(ref: DESIGN, Recovery)*

#### Execution Cancellation
- **GOAL-4.10** [P1]: User can cancel execution via `gid execute --cancel` or Ctrl+C *(ref: DESIGN, Execution Cancellation)*
- **GOAL-4.11** [P1]: Cancellation signals running sub-agents to stop gracefully with timeout *(ref: DESIGN, Execution Cancellation)*
- **GOAL-4.12** [P1]: On cancellation, in-progress tasks are reset to `todo` (not `failed`) *(ref: DESIGN, Execution Cancellation)*
- **GOAL-4.13** [P2]: On cancellation, worktrees are preserved for inspection unless `--cleanup` flag is used *(ref: DESIGN, Execution Cancellation)*

### Adaptive Scheduling (Module 5)

#### History-Driven Estimation
- **GOAL-5.1** [P2]: Execution history (actual vs estimated turns) is stored in execution telemetry and feeds into future estimates *(ref: DESIGN, Observability/Future Extensions)*
- **GOAL-5.2** [P1]: Agent recalls past execution experiences from Engram before graph creation to improve task sizing *(ref: DESIGN, Context Assembly/Engram Integration)*
- **GOAL-5.3** [P1]: After execution, significant learnings (estimation errors, common failures) are stored in Engram *(ref: DESIGN, Context Assembly/Engram Integration)*

#### Model Routing
- **GOAL-5.4** [P0]: Default model for sub-agents is configurable at graph, framework, and CLI levels *(ref: DESIGN, Harness Configuration)*
- **GOAL-5.5** [P2]: Simple tasks route to Sonnet, complex tasks route to Opus based on execution history *(ref: DESIGN, Adaptive Scheduling)*

#### Cost Estimation
- **GOAL-5.6** [P2]: `gid plan --cost` estimates total cost based on estimated_turns and model pricing *(ref: DESIGN, Adaptive Scheduling)*

#### Harness Configuration
- **GOAL-5.7** [P0]: All harness settings (`max_concurrent`, `max_retries`, `max_replans`, `default_checkpoint`, `approval_mode`, `model`) support three-level cascading precedence: CLI flag > `.gid/execution.yml` > framework config > built-in defaults *(ref: DESIGN, Harness Configuration)*
- **GOAL-5.8** [P0]: `.gid/execution.yml` is the project-level configuration file for all harness settings *(ref: DESIGN, Harness Configuration)*

### Interfaces & Human Interaction (Module 6)

#### Multi-Surface Support
- **GOAL-6.1** [P0]: Execution status is queryable via Telegram, CLI, and gidterm using shared file backend (graph.yml, execution-log.jsonl, execution-state.json) *(ref: DESIGN, Module 6/Interfaces/Architecture)*
- **GOAL-6.2** [P0]: All surfaces read execution state from files (no server process required) *(ref: DESIGN, Module 6/Interfaces/Architecture)*
- **GOAL-6.3** [P1]: `.gid/execution-state.json` tracks current run state (running/paused/cancelled, active tasks, pending approvals) and serves as the command interface between surfaces and engine *(ref: DESIGN, Module 6/Interfaces/Architecture)*

#### Approval Modes
- **GOAL-6.4** [P0]: Three approval modes are supported: `mixed` (default), `manual`, `auto` *(ref: DESIGN, Approval Modes)*
- **GOAL-6.5** [P0]: In `mixed` mode: Phase 1-3 pause for human approval; Phase 4-7 auto-execute *(ref: DESIGN, Approval Modes)*
- **GOAL-6.6** [P1]: In `manual` mode: all phases pause for human approval at each gate *(ref: DESIGN, Approval Modes)*
- **GOAL-6.7** [P1]: In `auto` mode: Phase 1-3 are collaborative but don't block; Phase 4-7 auto-execute *(ref: DESIGN, Approval Modes)*

#### Phase-Level Gates
- **GOAL-6.8** [P0]: After Phase 2 (requirements), harness pauses and sends approval request showing goal/guard count *(ref: DESIGN, Module 6/Approval Gates)*
- **GOAL-6.9** [P0]: After Phase 3 (design), harness pauses and sends approval request *(ref: DESIGN, Module 6/Approval Gates)*
- **GOAL-6.10** [P0]: After Phase 4 (graph), harness pauses and sends approval request showing task count and layer structure *(ref: DESIGN, Module 6/Approval Gates)*

#### Real-Time Intervention
- **GOAL-6.11** [P1]: User can stop execution mid-run via CLI `gid stop` or Telegram command *(ref: DESIGN, Module 6/Real-time Intervention)*
- **GOAL-6.12** [P1]: User can modify graph during execution; adaptive re-planning detects changes and adjusts plan *(ref: DESIGN, Module 6/Real-time Intervention)*

#### Failure Escalation
- **GOAL-6.13** [P0]: Failures are handled in three tiers: auto-retry (simple failures) → re-plan (structural issues) → escalate to human (unresolvable) *(ref: DESIGN, Module 6/Failure Escalation)*

#### gidterm as UI Layer
- **GOAL-6.14** [P1]: gidterm reads execution state from `.gid/execution-log.jsonl` via file watch (no backend integration) *(ref: DESIGN, Module 6/gidterm Relationship)*
- **GOAL-6.15** [P2]: gidterm renders TUI visualizations: DAG view, task status, live sub-agent logs *(ref: DESIGN, Module 6/gidterm Relationship)*
- **GOAL-6.16** [P1]: gidterm user actions (approve/stop/steer) write commands to `.gid/execution-state.json` for harness to pick up *(ref: DESIGN, Module 6/gidterm Relationship)*

#### MCP & CLI
- **GOAL-6.17** [P1]: `gid_plan` MCP tool returns execution plan as JSON or text *(ref: DESIGN, Interfaces/MCP Tool)*
- **GOAL-6.18** [P0]: CLI provides commands: `gid plan`, `gid execute`, `gid stats`, `gid approve`, `gid stop` *(ref: DESIGN, Interfaces/CLI)*
- **GOAL-6.19** [P0]: RustClaw calls gid-core API directly without MCP overhead *(ref: DESIGN, Interfaces/RustClaw Integration)*

### Skills & Templates (Module 7)

#### Skill Files
- **GOAL-7.1** [P0]: `skills/requirements/SKILL.md` provides template and guidelines for requirements.md generation (GOAL/GUARD format) *(ref: DESIGN, Module 7)*
- **GOAL-7.2** [P0]: `skills/design-doc/SKILL.md` provides template for DESIGN.md with numbered sections and interface signatures *(ref: DESIGN, Module 7)*
- **GOAL-7.3** [P1]: `skills/idea-intake/SKILL.md` provides Phase 1 workflow for capturing IDEAS.md *(ref: DESIGN, Module 7)*

#### Graph Generation Prompt
- **GOAL-7.4** [P0]: `gid_design` prompt instructs LLM to generate feature nodes with `metadata.goals` arrays *(ref: DESIGN, Module 7)*
- **GOAL-7.5** [P0]: `gid_design` prompt instructs LLM to generate task nodes with `satisfies`, `design_ref`, `verify`, `estimated_turns` metadata *(ref: DESIGN, Module 7)*
- **GOAL-7.6** [P0]: `gid_design` prompt instructs LLM to write rich self-contained task descriptions including interface signatures, data structures, constraints, and connection points *(ref: DESIGN, Context Assembly/Strategy 2)*
- **GOAL-7.7** [P1]: `gid_design` prompt instructs LLM to size tasks for 5-25 estimated_turns each *(ref: DESIGN, Task Sizing Guidance)*

#### Agent Orchestration
- **GOAL-7.8** [P1]: RustClaw system prompt includes guidance to recall engram memories before `gid_design` for task sizing context *(ref: DESIGN, Context Assembly/Engram Integration)*

## Guards

### Determinism & Correctness
- **GUARD-1** [hard]: `create_plan()` is deterministic — same graph always produces same plan (order, layers, critical path) *(ref: DESIGN, Adaptive Re-planning/Key Properties)*
- **GUARD-2** [hard]: Context assembly via `assemble_task_context()` is deterministic — uses graph traversal and file section extraction only, no LLM calls *(ref: DESIGN, Context Assembly/Strategy 1)*
- **GUARD-3** [hard]: Topology sort produces a valid execution order — tasks never execute before their dependencies *(ref: DESIGN, Architecture/Topology Analyzer)*
- **GUARD-4** [hard]: Cyclic graphs are rejected before execution — cycle detection must catch all cycles *(ref: DESIGN, Architecture/Topology Analyzer)*

### State Integrity
- **GUARD-5** [hard]: Graph state in `.gid/graph.yml` is the single source of truth — all surfaces read from this file *(ref: DESIGN, Recovery/Architecture)*
- **GUARD-6** [hard]: Task state transitions are monotonic in normal flow — `done` tasks never revert to `todo` unless explicitly reset by re-planning *(ref: DESIGN, Task State Machine)*
- **GUARD-7** [hard]: Execution is idempotent — running `gid execute` N times on a completed graph produces no side effects *(ref: DESIGN, Recovery)*

### Data Safety
- **GUARD-8** [hard]: No data loss on crash — execution-log.jsonl is append-only, graph state is persisted after each task *(ref: DESIGN, Recovery)*
- **GUARD-9** [hard]: Failed task verification never merges to main — worktrees are discarded on verify failure *(ref: DESIGN, Architecture/Executor)*
- **GUARD-10** [hard]: Merge conflicts pause execution — no automatic conflict resolution that could corrupt code *(ref: DESIGN, Git Worktree Strategy)*
- **GUARD-11** [hard]: Merges within a layer are serialized — no two merges to main happen concurrently *(ref: DESIGN, Merge Ordering/Within a Layer)*

### Sub-agent Isolation
- **GUARD-12** [hard]: Sub-agents cannot access workspace-level agent files (SOUL/AGENTS/USER/MEMORY) *(ref: DESIGN, Sub-agent System Prompt)*
- **GUARD-13** [hard]: Sub-agents work in isolated worktrees — parallel agents never conflict on file writes *(ref: DESIGN, Git Worktree Strategy)*
- **GUARD-14** [hard]: Sub-agents cannot modify `.gid/` directory — graph management is harness-only *(ref: DESIGN, Sub-agent System Prompt/Rules)*

### Project Guards Propagation
- **GUARD-15** [hard]: All project-level guards from `metadata.guards` are injected into every sub-agent's context *(ref: DESIGN, Invariant Verification/Guards in Sub-agent Context)*
- **GUARD-16** [soft]: Guards are never violated by sub-agent implementations — layer checkpoints catch violations *(ref: DESIGN, Invariant Verification)*

### Observability & Auditability
- **GUARD-17** [soft]: Every task execution produces telemetry events (start, done/failed, turns, tokens, duration) *(ref: DESIGN, Observability)*
- **GUARD-18** [soft]: Re-planning events (new tasks, new edges) are logged in execution-log.jsonl *(ref: DESIGN, Adaptive Re-planning/Limits)*

## Out of Scope

- **GUI execution dashboard** — TUI (gidterm) and CLI only, no web UI
- **Distributed execution** — parallel tasks run on same machine, no cluster support
- **Multi-project orchestration** — single project graph per execution (cross-graph dependencies deferred)
- **Human-in-the-loop task execution** — sub-agents run autonomously; human approves at phase/layer gates only
- **Property-based test generation from guards** — guards are constraints; test mapping is manual or future work
- **Cost budgets and limits** — execution runs to completion or failure; no mid-execution cost stopping
- **Dynamic max_concurrent adjustment** — concurrency limit is static per execution
- **Sub-agent model switching mid-task** — each task uses one model for its entire execution
- **Automatic merge conflict resolution** — conflicts always pause for human/agent intervention

## Dependencies

- **gid-core** (Rust crate) — graph data structures, topology analyzer, execution planner
- **Host agent framework** (RustClaw or OpenClaw) — sub-agent spawning, worktree management, LLM API
- **Git** — worktree creation, merge, conflict detection
- **Language-specific tooling** — `cargo` (Rust), `npm` (Node), `pytest` (Python) for default checkpoints
- **File system** — `.gid/graph.yml`, `.gid/execution-log.jsonl`, `.gid/execution-state.json`, requirements.md, DESIGN.md must be readable
- **Engram** (optional) — cross-project learning for task sizing and estimation

## Traceability Notes

This requirements document was extracted from the full design document (`DESIGN-task-harness.md`, 48KB). Each requirement references its source design section for bidirectional traceability.

The design document addresses implementation details (algorithms, data structures, code examples) that satisfy these requirements. During Phase 4 (Graph Generation), task nodes will reference specific requirements via `satisfies` metadata (e.g., `satisfies: ["GOAL-1.5", "GOAL-1.6"]`), completing the traceability chain: **Requirements → Design → Graph → Tasks → Code**.

## Summary

**109 GOALs** (63 P0 / 40 P1 / 6 P2) + **18 GUARDs** (15 hard / 3 soft) = **127 verifiable criteria**
