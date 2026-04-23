# ISS-030: rustclaw `start_ritual` tool calls `gid project resolve`

**Status**: open
**Created**: 2026-04-23
**Reporter**: potato
**Severity**: low (consumer-layer adaptation)
**Parent feature**: feature-project-registry

---

## Problem

rustclaw's `start_ritual` tool currently extracts workspace from task text or falls back to parent workspace. After ISS-028 + ISS-029, it must resolve via `gid project resolve` and pass a `work_unit`.

## Deliverable

### 1. Tool signature change

Option A (preferred — single param):
```
start_ritual(work_unit: "engram:ISS-022")
```

Option B (separate fields):
```
start_ritual(project: "engram", issue: "ISS-022")
# or feature: "..." / task: "..."
```

Remove the `workspace` parameter (or deprecate with warning for one release).

### 2. Implementation

- Shell out to `gid project resolve <project>` to get canonical path (or use gid-core Rust API if rustclaw links it)
- Build `WorkUnit` enum value and call gid-core ritual API (ISS-029)
- Error handling: resolver failure surfaces `Run 'gid project add <name> <path>' to register this project.`

### 3. Location

rustclaw repo, likely `src/tools/ritual.rs` (or wherever `start_ritual` is defined).

## Acceptance

- `start_ritual(work_unit="engram:ISS-022")` launches ritual in correct workspace
- Invalid project name surfaces a clear registration hint
- Unit test (if tool layer is mockable) or integration test via CLI
- Documentation updated (rustclaw AGENTS.md / TOOLS.md)

## Dependencies

- **Blocks on ISS-028** (needs `gid project resolve`)
- **Blocks on ISS-029** (needs new ritual API)

## Scope

- **In**: rustclaw tool API change + caller migration within rustclaw
- **Out**: agentctl / other consumers (separate issues when they land)
