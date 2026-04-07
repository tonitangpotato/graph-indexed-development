# Review: design-migration.md

**Reviewed:** 2026-04-06
**Document:** GID SQLite Migration — Migration Logic
**GOALs covered:** 2.1–2.10, GUARDs 3, 5, 9

---

## 🔴 Critical (blocks implementation)

### FINDING-1 [Check #1] Design types don't match requirements schema
The design defines `RawYamlNode` with fields `{id, node_type, label, body, extra}` and maps them to columns `{id, type, label, body, meta}`. But the requirements (GOAL-2.3) specify the real gid-core schema: nodes have `{id, title, status, description, assigned_to, priority, node_type}` plus 14 dedicated metadata columns (`file_path`, `lang`, `start_line`, `end_line`, `signature`, `visibility`, `doc_comment`, `body_hash`, `node_kind`, `owner`, `source`, `repo`, `created_at`, `updated_at`), plus `node_tags` and `node_metadata` tables. The design's simplified 5-column model is **fundamentally misaligned** with the actual storage schema defined in design-storage.md.

**Impact:** Implementing this design as-is would produce a broken migration that loses most node fields.

**Suggested fix:** Rewrite §4.1 `RawYamlNode` to match the actual `graph.yml` Node struct fields. Rewrite §6 Transform to map YAML fields → correct SQLite columns (including metadata promotion logic from GOAL-2.3). The field mapping table (§6.1) needs complete replacement.

### FINDING-2 [Check #1] Edge schema mismatch
Same problem for edges. Design maps `{from, to, edge_type, weight, extra}` to `{source_id, target_id, type, weight, meta}`. But requirements (GOAL-2.4) specify edges need `{from, to, relation}` mapped to the edges table, plus `weight` (default 1.0), `confidence` (from Edge struct's `Option<f64>`), and `metadata`. The design uses `edge_type` where the actual YAML schema uses `relation`, and completely omits the `confidence` column.

**Suggested fix:** Update `RawYamlEdge` to use `relation` field. Add `confidence: Option<f64>` field. Update the transform and field mapping table.

### FINDING-3 [Check #1] Knowledge migration entirely missing
GOAL-2.5 requires migrating `KnowledgeNode` data (`findings`, `file_cache`, `tool_history`) from each node into the `knowledge` table. The design document has **zero mention** of knowledge migration. No parsing, no transform, no insertion, no verification.

**Suggested fix:** Add a new section covering knowledge extraction from YAML nodes, serialization of findings/file_cache as JSON objects and tool_history as JSON array, and insertion into the `knowledge` table. Add knowledge row count to the verification phase (§8) and to `MigrationReport`.

---

## 🟡 Important (should fix before implementation)

### FINDING-4 [Check #6] StorageOp mismatch with master design
The design uses `StorageOp::UpsertNode` and `StorageOp::UpsertEdge` (§6.2, §7.1), but the master design.md §3 defines `StorageOp::PutNode(Node)`, `StorageOp::AddEdge(Edge)`, etc. These are different variants with different field structures. The transform phase generates ops that don't match the actual enum.

**Suggested fix:** Use the canonical `StorageOp` variants from master design or explain why migration needs different ops.

### FINDING-5 [Check #14] Tags migration not addressed
The requirements (GOAL-2.3) specify that tags from YAML nodes go to the `node_tags` table. The design doesn't mention tags at all — neither in parsing, transform, nor insertion.

**Suggested fix:** Add tag extraction from YAML nodes and insertion into `node_tags` table during the transform/insert phases.

### FINDING-6 [Check #14] Metadata promotion logic missing
GOAL-2.3 specifies that metadata keys matching dedicated column names (`file_path`, `lang`, `start_line`, etc.) are "promoted" to those columns, with remaining keys going to `node_metadata`. The design lumps all extra fields into a single `meta TEXT` JSON blob, which doesn't match this promotion requirement.

**Suggested fix:** Add metadata promotion logic in the transform phase. Enumerate the 14 promoted keys, extract them from the YAML metadata HashMap, map to correct columns, and route the rest to `node_metadata`.

### FINDING-7 [Check #7] Verify phase checks wrong table structure
§8.1 verification checks `meta IS NOT NULL AND json_valid(meta) = 0` on the `nodes` table. But the actual schema (per design-storage.md) doesn't have a `meta` column on `nodes` — metadata is in a separate `node_metadata` table.

**Suggested fix:** Update verification queries to match the actual schema. Check `node_metadata`, `node_tags`, and `knowledge` tables.

### FINDING-8 [Check #7] Idempotency check error case (GOAL-2.8a)
Requirements GOAL-2.8a says: if `graph.db` already exists, migration should error with "SQLite database already exists." But the design's idempotency mechanism (§11) uses a fingerprint-based skip (`MigrationStatus::Skipped`), not an error. These are different behaviors — the requirement says error + non-zero exit, the design says silent skip.

**Suggested fix:** Align with requirements. When `graph.db` exists, error out (GOAL-2.8a). Remove the fingerprint-based idempotency or make it a separate `--force` flag behavior.

### FINDING-9 [Check #15] Duplicate node handling inconsistency
Requirements GOAL-2.9 says: "If duplicate node IDs exist in the YAML, the last occurrence takes precedence and a warning is emitted." The design's validation (§5.1) flags `DuplicateNodeId` as a diagnostic, which in `Strict` mode causes the entire migration to fail. The requirement says warn and continue, not fail.

**Suggested fix:** In migration context, duplicate node IDs should always use "last wins" + warning behavior, regardless of `ValidationLevel`. Either hardcode this or add a special case in validation.

### FINDING-10 [Check #7] FK enforcement during migration
Requirements GOAL-2.9 says: "edges reference non-existent node IDs, they are still migrated (the edges table does not enforce foreign keys during migration)." The design's validation phase (§5.2) rejects dangling edge references as errors. These directly contradict.

**Suggested fix:** During migration, dangling edge references should be warnings (migrated anyway), not errors. Disable FK enforcement during the migration transaction or handle in validation level.

---

## 🟢 Minor (can fix during implementation)

### FINDING-11 [Check #15] `batch_size` in config but chunking adds no value
§7.1 iterates `ops.chunks(batch_size)` but all chunks are within the same transaction. SQLite doesn't benefit from chunking within a single transaction — it's all one atomic write. The batching only matters if you're committing per-batch, which the design explicitly doesn't do.

**Suggested fix:** Remove `batch_size` from config, or document that it's for future use (e.g., progress reporting per chunk). Currently misleading.

### FINDING-12 [Check #4] Naming inconsistency: `MigrationError` vs `StorageError`
The design defines its own `MigrationError` enum with 7 variants. The master design defines `StorageError`. The relationship between them isn't specified — does `MigrationError` wrap `StorageError`? Are they separate?

**Suggested fix:** Clarify: `MigrationError` should be a separate error type (migration is a one-time operation, not storage CRUD). Add `From<StorageError> for MigrationError` conversion.

### FINDING-13 [Check #23] `GraphId::parse` dependency
§6.2 calls `GraphId::parse(&node.id)` but `GraphId` isn't defined in the master design — it uses plain `String` for node IDs. This is an undocumented dependency.

**Suggested fix:** Use `String` IDs directly (matching master design) or define `GraphId` in the shared types section.

### FINDING-14 [Check #19] No cross-cutting concern: what if YAML is very large?
The design loads the entire YAML into memory (§4.1: "graph sizes are bounded by practical use; streaming is unnecessary"). For production use this is probably fine, but there's no explicit bound or error if someone feeds a 1GB YAML file.

**Suggested fix:** Add a file size check (e.g., max 100MB) with a clear error message. Document the practical bound.

---

## ✅ Passed Checks

- Check #2: References resolve ✅ (all §N references exist, GOAL refs match requirements)
- Check #3: No dead definitions ✅ (all types are used in pipeline)
- Check #5: State machine — N/A (pipeline is linear, not a state machine)
- Check #8: No string slicing on user input ✅
- Check #9: Integer overflow — counters are u64 ✅
- Check #10: Option handling — `unwrap_or_default()` used consistently ✅
- Check #11: Match exhaustiveness — `_ => {}` in insert is safe (migration only produces Upsert ops) ✅
- Check #12: Ordering — pipeline is sequential, no guard ordering issues ✅
- Check #13: Separation of concerns ✅ (parse/validate/transform/insert/verify are distinct)
- Check #16: API surface — minimal public types ✅
- Check #17: Goals explicit ✅ (GOAL traceability table in §12)
- Check #20: Appropriate abstraction level ✅ (pseudocode clarifies intent)
- Check #21: Ambiguous prose — mostly clear ✅
- Check #24: Migration path — self-contained Phase 1 ✅
- Check #25: Testability — pure functions for parse/validate/transform ✅
- Check #26: Existing code alignment — uses `serde_yaml` already in project ✅
- Check #27: API compatibility — new code, no existing callers ✅
- Check #28: Feature flag — behind `sqlite` feature ✅

---

## Summary

- **Critical: 3** (schema mismatch for nodes, edges, and missing knowledge migration)
- **Important: 7** (StorageOp mismatch, tags, metadata promotion, verify queries, idempotency semantics, duplicate handling, FK enforcement)
- **Minor: 4** (batch_size, error type relationship, GraphId, file size bound)
- **Recommendation:** ❌ **Needs major revision** — the core type definitions and field mappings are misaligned with both the requirements and the master design. The design was written against a simplified schema, not the actual gid-core data model.
- **Estimated implementation confidence:** Low — an implementer following this design would produce code that doesn't compile against the real types.

---
## ✅ All Findings Applied (2026-04-06 19:56)

All 14 findings applied to design-migration.md:
- FINDING-1 ✅ RawYamlNode rewritten with correct fields (id, title, status, description, node_type, assigned_to, priority, tags, knowledge, extra)
- FINDING-2 ✅ RawYamlEdge uses relation (not edge_type), added confidence: Option<f64>
- FINDING-3 ✅ Knowledge migration added (§6.4): RawYamlKnowledge struct, transform_knowledge function, SetKnowledge StorageOp
- FINDING-4 ✅ StorageOp uses canonical variants (PutNode, AddEdge, SetTags, SetMetadata, SetKnowledge)
- FINDING-5 ✅ Tags migration added in §6.3 (node.tags → SetTags StorageOp)
- FINDING-6 ✅ Metadata promotion logic added in §6.3 (PROMOTED_KEYS list, dedicated columns vs node_metadata)
- FINDING-7 ✅ Verify checks node_metadata, node_tags, knowledge tables (not old meta column)
- FINDING-8 ✅ Idempotency rewritten: graph.db exists → error (GOAL-2.8a), --force flag for override
- FINDING-9 ✅ Duplicate IDs always use last-wins + warning, never error (GOAL-2.9)
- FINDING-10 ✅ FK enforcement disabled during migration (PRAGMA foreign_keys = OFF), dangling edges migrated with warnings
- FINDING-11 ✅ batch_size removed from config (no benefit within single transaction)
- FINDING-12 ✅ MigrationError documented as separate from StorageError with From<StorageError> conversion
- FINDING-13 ✅ GraphId::parse removed, plain String IDs used directly
- FINDING-14 ✅ 100MB file size limit added to §4.1

