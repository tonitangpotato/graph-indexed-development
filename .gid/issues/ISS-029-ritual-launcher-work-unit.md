# ISS-029: gid-core ritual launcher accepts `work_unit` only, rejects `target_root`

**Status**: open
**Created**: 2026-04-23
**Reporter**: potato
**Severity**: medium
**Parent feature**: feature-project-registry
**Resolves**: ISS-027 (ritual workspace guard — main body)

---

## Problem

Ritual launcher currently accepts `target_root=PATH` from the caller and relies on implicit working-directory inheritance. Root cause of ISS-027 and blocks ISS-020 cleanup.

## Deliverable

### 1. API change

**Old**:
```rust
start_ritual(target_root: PathBuf, task: String)
```

**New**:
```rust
start_ritual(work_unit: WorkUnit)

enum WorkUnit {
    Issue    { project: String, id: String },      // "engram", "ISS-022"
    Feature  { project: String, name: String },    // "engram", "consolidation"
    Task     { project: String, task_id: String }, // "engram", "T-042"
}
```

Workspace path is auto-resolved from `work_unit.project` via `gid project resolve` (ISS-028).

### 2. Explicit rejection

If any caller passes `target_root`, return:
```
Error: target_root is no longer supported. Pass work_unit instead.
```

### 3. Startup validation

- Resolved path exists and contains `.gid/`
- Git tree state check (flag dirty, refuse to start on anomalies — per ISS-027 design)
- No `DEPRECATED_DO_NOT_RITUAL` sentinel in target

## Acceptance

- Unit test: `start_ritual(WorkUnit::Issue { project: "engram", id: "ISS-022" })` resolves correct path
- Unit test: non-existent project name errors cleanly
- Unit test: passing both `work_unit` and `target_root` (via legacy API) → error
- Integration test: CLI ritual invocation uses new API
- All existing callers of `start_ritual` migrated in the same PR (no legacy shim)

## Dependencies

- **Depends on ISS-028** for the real resolver. For dev/test, a hardcoded-map stub resolver is acceptable so ISS-029 can progress in parallel.

## Scope

- **In**: gid-core ritual API, validation, caller migration
- **Out**: registry CLI (ISS-028), rustclaw tool layer (ISS-030)
