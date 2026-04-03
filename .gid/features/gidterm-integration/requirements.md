# gidterm Integration — Requirements

## Status: 🔴 TODO (P3)

## Design Decision (from DESIGN-task-harness.md)
gidterm is a **UI layer** — it does NOT use its own executor/scheduler/PTY for harness work.
Communication is via **shared files** (no server process, no API):
- gidterm READS: `.gid/execution-log.jsonl`, `.gid/graph.yml`, `.gid/ritual-state.json`
- gidterm WRITES: `.gid/execution-state.json` (user commands: approve/stop/steer)

gidterm's existing `core/executor.rs`, `core/scheduler.rs`, `core/pty.rs` are legacy from pre-harness design.

## Goals (from requirements.md GOAL-6.14 to GOAL-6.16)

### GIDTERM-1: Read execution state (GOAL-6.14, P1)
- Watch `.gid/execution-log.jsonl` via file watch (inotify/fsevents)
- Parse JSONL events: task_start, task_done, task_failed, phase_complete, etc.
- Update TUI display in real-time as events arrive

### GIDTERM-2: TUI visualizations (GOAL-6.15, P2)
- DAG view: show task graph with dependency edges
- Task status: color-coded by status (todo/progress/done/failed)
- Live sub-agent logs: stream task executor output
- Ritual status: show current phase, progress bar

### GIDTERM-3: User control (GOAL-6.16, P1)
- Approve: write `{"command": "approve", "phase_id": "..."}` to execution-state.json
- Stop/pause: write `{"command": "cancel"}` to execution-state.json
- Steer: write `{"command": "steer", "message": "..."}` to execution-state.json
- Harness picks up commands from execution-state.json on next poll

## What gid-core provides to gidterm
- `gid-core` as dependency for graph types (Graph, Node, NodeStatus)
- `execution_state.rs` types for reading/writing state
- `telemetry.rs` types for parsing execution-log.jsonl
- `log_reader.rs` for JSONL parsing utilities

## Implementation Approach
1. Add `gid-core` as dependency (default features, no harness/ritual)
2. Replace internal `core/graph.rs` with thin wrapper around gid-core Graph
3. Add file watcher for execution-log.jsonl
4. Add execution-state.json writer for user commands
5. Add TUI views: DAG, ritual status, live logs

## NOT in scope
- gidterm does NOT run harness tasks (that's RustClaw/CLI)
- gidterm does NOT implement MCP (that's packages/mcp/)
- gidterm's legacy executor/scheduler/PTY are NOT used
