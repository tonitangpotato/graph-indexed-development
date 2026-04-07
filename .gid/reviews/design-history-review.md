# Review: design-history.md

**Reviewed:** 2026-04-06
**Document:** GID SQLite Migration — Snapshot History
**GOALs covered:** 3.1–3.9

---

## 🔴 Critical (blocks implementation)

### FINDING-1 ✅ Applied [Check #1] Snapshot naming convention contradicts requirements
The design uses `<ISO-8601-compact-UTC>-<8-hex-random>.db` (e.g., `20260406T183420Z-a1b2c3d4.db`). The requirements (GOAL-3.2) specify `<filesystem-safe-ISO-8601>.db` (e.g., `2026-04-06T19-40-12Z.db`). These are different formats — the design uses compact `20260406T183420Z`, the requirements use hyphenated `2026-04-06T19-40-12Z`. The 8-hex random suffix in the design is not mentioned in requirements.

More critically, the requirements say `gid history restore {timestamp}` takes a timestamp as the identifier. But the design uses `snapshot_id` which is `<timestamp>-<random>`. A user can't predict the random suffix — they'd need to list first then copy-paste. This affects the UX model.

**Suggested fix:** Align on one naming scheme. Either: (a) use requirements' format and drop random suffix (handle same-second collision by appending `-1`, `-2`), or (b) update requirements to accept `snapshot_id` instead of raw timestamp. The requirements are the source of truth — design should follow.

### FINDING-2 ✅ Applied [Check #6] `snapshot_diff` uses ATTACH with parameter binding incorrectly
§7 pseudocode does:
```rust
conn.execute_batch("
    ATTACH DATABASE ?1 AS snap_from;
    ATTACH DATABASE ?2 AS snap_to;
")?;
```
`execute_batch` does NOT support parameter binding — it runs raw SQL. This would literally try to attach a database named `?1`. This is a correctness bug that would cause runtime failure.

**Suggested fix:** Use two separate `conn.execute("ATTACH DATABASE ?1 AS snap_from", [&from_path])` calls with proper parameter binding via `execute` (not `execute_batch`).

---

## 🟡 Important (should fix before implementation)

### FINDING-3 ✅ Applied [Check #6] Diff uses `content_hash` column that may not exist
§7 pseudocode compares `f.content_hash != t.content_hash` to detect modifications. The design says "per master `design.md` §3" this column exists. But checking design-storage.md, the `nodes` table schema doesn't include a `content_hash` column. If this column doesn't exist, the diff query fails.

**Suggested fix:** Either: (a) add `content_hash` to the storage schema (design-storage.md), or (b) compare individual columns (title, status, description, etc.) to detect modifications. Option (b) is more robust but slower.

### FINDING-4 ✅ Applied [Check #6] Diff `metadata_hash` field on NodeModification
`NodeModification` has `old_meta_hash` and `new_meta_hash` fields, read from `f.metadata_hash` and `t.metadata_hash` columns. Same problem — this column likely doesn't exist in the schema. Also, if metadata is in a separate `node_metadata` table (per design-storage.md), comparing it requires joining, not a simple column read.

**Suggested fix:** Redesign modification detection. Either use a hash column (add it to schema) or do field-by-field comparison with a helper function.

### FINDING-5 ✅ Applied [Check #5] 50-snapshot limit interaction with restore auto-save (GOAL-3.3/3.5)
Requirements GOAL-3.5 says restore auto-saves current state first, and if the auto-save would cause the 50-limit to delete the target snapshot, the limit is "temporarily exceeded by one." The design's `snapshot_prune` (§9) is a separate operation from `snapshot_save` and doesn't have this protect-the-target logic. There's a race condition: `snapshot_save` (auto-save) → `prune` (might delete the restore target) → `snapshot_restore` (target gone).

**Suggested fix:** Add a `protected_ids: &[&str]` parameter to `snapshot_prune` that prevents specified snapshots from being deleted. Call prune with the restore target ID protected.

### FINDING-6 ✅ Applied [Check #7] `snapshot_restore` replaces via file copy, not Backup API
§6 pseudocode says "Strategy: Use SQLite Backup API in reverse" but the actual code does `fs::copy(&snap_path, &tmp_path)` + `fs::rename`. This is file-level copy, not Backup API. The strategy description is misleading.

File-level copy works but has a subtle issue: if the live `graph.db` is open by the current process (which it likely is for the auto-save step), the copy + rename might conflict with the open file handle, especially on macOS where file descriptors persist through renames.

**Suggested fix:** Either: (a) close the live connection first (drop it), then copy+rename, then reopen; or (b) actually use the Backup API in reverse (from snapshot to live). Document which approach and why.

### FINDING-7 ✅ Applied [Check #14] WAL/SHM cleanup after restore is best-effort
§6 does `let _ = fs::remove_file(live_db_path.with_extension("db-wal"))`. If the WAL file can't be removed (permissions, in-use), the restore "succeeds" but the database might have inconsistent state — the old WAL could be replayed on next open.

**Suggested fix:** Make WAL/SHM removal a hard requirement. If they can't be removed, error out with a message explaining the user needs to close other gid processes.

### FINDING-8 ✅ Applied [Check #18] No trade-off discussion for index.json vs metadata-in-DB
The design says "per master design.md §9 trade-offs, we maintain index.json" but doesn't discuss the trade-offs within this document. What are the failure modes? What if index.json and actual snapshot files get out of sync (user manually deletes a .db file)?

**Suggested fix:** Add a brief trade-offs section. Document the self-healing behavior more explicitly — currently §3 mentions it but doesn't specify what happens when a .db file exists but isn't in index.json, or when index.json lists a .db that's been deleted.

### FINDING-9 ✅ Applied [Check #7] `compute_sha256` used but never defined
Both `snapshot_save` (§4) and `snapshot_restore` (§6) call `compute_sha256(&path)` but this function is never defined in the document.

**Suggested fix:** Add a brief definition or reference to where this utility lives. It's trivial but should be specified for completeness.

---

## 🟢 Minor (can fix during implementation)

### FINDING-10 ✅ Applied [Check #15] Pruning dedup logic is more complex than needed
§9 `snapshot_prune` builds `to_remove`, then deduplicates by sorting and `dedup_by`. But the three pruning strategies can overlap (a snapshot might be selected by both keep-last and max-age). A simpler approach: use a HashSet of IDs to remove.

**Suggested fix:** Use `HashSet<String>` for `to_remove` IDs instead of `Vec` + dedup.

### FINDING-11 ✅ Applied [Check #4] Inconsistent types: `NodeId` vs `String`
§7 uses `Vec<NodeId>` for added/removed nodes and `EdgeId` for edges, but these types aren't defined in this document or clearly imported. The master design uses `String` for node IDs.

**Suggested fix:** Use `String` consistently or import/define `NodeId`/`EdgeId` type aliases.

### FINDING-12 ✅ Applied [Check #10] `load_index` error handling unspecified
`load_index(snapshots_dir)` is called in multiple places but behavior when `index.json` doesn't exist vs. is corrupt isn't specified inline. §3 mentions self-healing but doesn't tie it to `load_index`.

**Suggested fix:** Specify: `load_index` returns empty vec if file doesn't exist, triggers rebuild if file exists but fails to parse.

### FINDING-13 ✅ Applied [Check #19] No disk space mention for restore
§9 has disk space checks for save, but restore also copies a file (the snapshot to live DB path). If the snapshot is large and disk is low, restore could fail partway through.

**Suggested fix:** Add disk space check before restore as well.

---

## ✅ Passed Checks

- Check #2: References resolve ✅ (GOAL refs match requirements, §N refs exist)
- Check #3: No dead definitions ✅ (all types used)
- Check #8: No string slicing on user input ✅
- Check #9: Integer overflow — u64 for sizes and counts ✅
- Check #12: Ordering — prune strategies are combinable, order doesn't affect final set ✅
- Check #13: Separation of concerns ✅ (save/list/restore/diff/prune are distinct functions)
- Check #16: API surface — minimal, each function is a clear entry point ✅
- Check #17: Goals explicit ✅ (GOAL traceability in §11)
- Check #20: Appropriate abstraction ✅ (pseudocode level, not line-by-line)
- Check #21: Ambiguous prose — mostly clear ✅
- Check #24: No migration needed (new feature) ✅
- Check #25: Testability — each function is independently testable with tempdir ✅
- Check #26: No existing snapshot code to conflict with ✅
- Check #27: New API, no existing callers ✅
- Check #28: Feature flag — behind `sqlite` feature ✅

---

## Summary

- **Critical: 2** (naming convention mismatch, ATTACH parameter binding bug)
- **Important: 7** (content_hash missing, metadata_hash missing, prune-protect interaction, restore file handle issue, WAL cleanup, trade-offs missing, compute_sha256 undefined)
- **Minor: 4** (dedup logic, NodeId type, load_index error handling, disk space for restore)
- **Recommendation:** ✅ **All 13 findings applied** (2026-04-06). Naming convention aligned with requirements (FINDING-1), ATTACH bug fixed (FINDING-2), content_hash/metadata_hash replaced with field-by-field comparison (FINDING-3/4), all other findings addressed.
