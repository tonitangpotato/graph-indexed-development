---
id: "ISS-030"
title: "rustclaw `start_ritual` tool calls `gid project resolve`"
status: closed
priority: P2
created: 2026-04-26
closed: 2026-04-23
---
# ISS-030: rustclaw `start_ritual` tool calls `gid project resolve`

**Status:** closed (2026-04-23 — implemented as rustclaw ISS-022)
**Created**: 2026-04-23
**Closed**: 2026-04-23
**Reporter**: potato
**Severity**: low (consumer-layer adaptation)
**Parent feature**: feature-project-registry

## Resolution

Implemented in rustclaw repo, tracked as **rustclaw ISS-022** (same work, per-repo issue number — see `/Users/potato/rustclaw/.gid/issues/ISS-022-migrate-start-ritual-to-workunit.md`).

**Chosen approach**: Option B variant — structured object, not stringly-typed.

Rather than `start_ritual(work_unit: "engram:ISS-022")` (string parsing layer), the tool schema accepts gid-core's `WorkUnit` enum directly via `#[serde(tag="kind")]`:

```json
{"task": "...", "work_unit": {"kind": "issue", "project": "engram", "id": "ISS-022"}}
```

Why: `WorkUnit` is already a tagged enum in gid-core (ISS-029). Passing JSON objects through `serde_json::from_value::<WorkUnit>()` means no custom parser, no string grammar to keep in sync, no silent ambiguity between `:` and `/` delimiters. The LLM sees the exact structure the library expects.

**What was delivered** (all acceptance criteria met):
- ✅ `work_unit` is required, `workspace` parameter removed from schema (no deprecation window — root fix, not patch)
- ✅ Uses gid-core Rust API directly (`RegistryResolver::load_default` + `resolve_and_validate`) — no shell-out to `gid project resolve`, since rustclaw links gid-core as a library
- ✅ Registry-miss error surfaces `~/.config/gid/projects.yml` path in the error message
- ✅ 284/284 tests pass, 0 regressions
- ✅ `RitualRunner::start_with_work_unit(unit, task)` is the new API; legacy `start(task)` kept only for `/ritual` Telegram command where user has already picked project interactively

**Not done in this issue** (deferred):
- AGENTS.md / TOOLS.md documentation updates — can fold into the next rustclaw docs sweep
- Unifying `/ritual` Telegram command path with the tool path — separate UX question

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
