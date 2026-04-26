---
id: "ISS-037"
title: "`BatchOp::DeleteNode` orphans edges (FK-off context)"
status: closed
priority: P2
created: 2026-04-25
closed: 2026-04-25
severity: high
related: ["ISS-015", "ISS-033", "ISS-016"]
---
# ISS-037 — `BatchOp::DeleteNode` orphans edges (FK-off context)

**Status:** closed
**Type:** bug / root fix
**Severity:** high (data integrity)
**Discovered:** 2026-04-25 via RustClaw `gid_refactor` (delete) producing orphaned edges in `.gid/graph.db`
**Closed:** 2026-04-25
**Related:** ISS-015 (PRAGMA fk in transaction), ISS-033 (FK cascade not enforced), ISS-016 (LSP dangling edges)

## Resolution

Fixed by explicit edge deletion before node deletion in both `execute_batch` and `execute_migration_batch`. The migration path (FK-disabled) cannot rely on CASCADE; the normal path was relying on FK CASCADE that ISS-033 proved unreliable. Both paths now do the cleanup at the op level — no dependency on SQLite engine FK behavior.

**Tests added (3, all passing under `--features sqlite`):**
- `test_iss037_delete_node_in_execute_batch_removes_incident_edges` — normal path: A→B→C, delete B, both edges gone, 0 orphans.
- `test_iss037_delete_node_in_migration_batch_removes_incident_edges` — FK-off path same scenario.
- `test_iss037_delete_node_self_loop_removed` — self-loop edge cleaned up.

**Verification:** 690 gid-core lib tests pass with sqlite feature enabled.

---

## Summary

`BatchOp::DeleteNode(id)` in `crates/gid-core/src/storage/sqlite.rs` only deletes the node row, never the edges that reference it. Both call sites are affected:

1. `SqliteStorage::execute_batch` (line ~918) — normal application batches.
2. `SqliteStorage::execute_migration_batch` (line ~254) — migration / FK-disabled context (`FkGuard::new` / `FkGuard::disable`).

In `execute_migration_batch`, foreign-key enforcement is **deliberately disabled** via `FkGuard`, so the SQLite engine cannot cascade-delete edges for us. The `execute_batch` path relies on `PRAGMA foreign_keys = ON` + `ON DELETE CASCADE`, but as ISS-033 documented, that cascade is not always enforced (depends on schema + connection state). Either way, **the batch op itself must guarantee edge cleanup** — relying on the storage engine's FK behavior is fragile and was already proven incorrect by ISS-015 / ISS-033.

This bug surfaced when RustClaw used `gid_refactor delete` to remove planned code nodes from `.gid/graph.db`. The nodes were gone, but their edges remained — visible via:

```sql
SELECT COUNT(*) FROM edges
WHERE from_node NOT IN (SELECT id FROM nodes)
   OR to_node   NOT IN (SELECT id FROM nodes);
-- > 0  (orphan edges)
```

`gid_validate` reported broken references. `gid_visual` and impact queries returned phantom edges pointing into the void.

---

## Root cause

The semantic contract of `BatchOp::DeleteNode(id)` is "remove this node from the graph." A graph is `(V, E)` — removing a vertex implies removing every incident edge. The current implementation only honors `V`, leaving `E` inconsistent.

This is a **specification bug**, not a SQL pragma issue. The fix is at the operation level, not the schema level.

---

## Fix (root, not patch)

In **both** call sites of `BatchOp::DeleteNode`, delete edges referencing the node **before** deleting the node row:

```rust
BatchOp::DeleteNode(id) => {
    // ISS-037: edges must be removed at the op level — do not rely on
    // FK cascade (disabled by FkGuard in migration_batch; unreliable in
    // execute_batch per ISS-033). DeleteNode is responsible for V *and* E.
    tx.execute(
        "DELETE FROM edges WHERE from_node = ? OR to_node = ?",
        params![id, id],
    )?;
    tx.execute("DELETE FROM nodes WHERE id = ?", params![id])?;
}
```

Why both sites:
- `execute_batch`: even with FK ON + CASCADE, ISS-033 showed cascade is not guaranteed across all schema versions / connection setups. Belt-and-suspenders is correct here because the cost (one extra DELETE) is negligible and the cost of an orphan edge is data-integrity rot.
- `execute_migration_batch`: FK is explicitly disabled via `FkGuard`. Without explicit edge deletion, every migration that deletes a node leaves orphans. Non-negotiable.

This is **not** a patch — `DeleteNode` is being corrected to honor its semantic contract. The previous behavior was a bug, not a feature.

---

## Regression tests

Add to `sqlite.rs` test module:

1. `delete_node_in_execute_batch_removes_incident_edges` — put 3 nodes (A, B, C) + 2 edges (A→B, B→C), DeleteNode(B), assert no edges remain referencing B.
2. `delete_node_in_migration_batch_removes_incident_edges` — same fixture, route through `execute_migration_batch` (FK-disabled path), assert orphan-edge count is 0.
3. `delete_node_self_loop_removed` — node A with self-edge A→A, DeleteNode(A), assert 0 edges.

Each test must end with a SELECT that asserts:
```sql
SELECT COUNT(*) FROM edges
WHERE from_node NOT IN (SELECT id FROM nodes)
   OR to_node   NOT IN (SELECT id FROM nodes);
-- expected: 0
```

---

## Impact / blast radius

- Any caller that issues `BatchOp::DeleteNode` (CLI `gid refactor delete`, RustClaw `gid_refactor` tool, migrations, infer rollback batches).
- Existing graphs may already contain orphan edges from past deletes. Suggest running a one-time cleanup query on user graphs:
  ```sql
  DELETE FROM edges
  WHERE from_node NOT IN (SELECT id FROM nodes)
     OR to_node   NOT IN (SELECT id FROM nodes);
  ```
  (Out of scope for this issue — file as ISS-038 if needed.)

---

## Acceptance criteria

- [ ] Both `execute_batch` and `execute_migration_batch` delete incident edges before deleting node.
- [ ] 3 regression tests added, all pass.
- [ ] `cargo test -p gid-core` passes (full suite).
- [ ] No new clippy warnings.
- [ ] RustClaw rebuilds against local gid-core, restarts cleanly, can re-run the delete-then-verify cycle without producing orphan edges.
