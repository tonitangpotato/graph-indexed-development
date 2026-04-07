# Review: requirements-migration.md, requirements-history.md, requirements-context.md

**Reviewed**: 2026-04-06  
**Reviewer**: Claude Code (automated structured review)  
**Documents**: 3 feature requirement files for the GID SQLite migration  
**Total**: 28 GOALs across 3 documents (17 P0 / 9 P1 / 2 P2), plus 10 GUARDs in parent doc

---

## 🔴 Critical (blocks implementation)

### FINDING-1: [Check #5, #7] GOAL-2.5: Knowledge migration — incomplete specification of "knowledge table" structure

**GOAL-2.5** says: "transfers all knowledge graph data (per-node findings, file_cache, tool_history) from the YAML `knowledge` section into the SQLite `knowledge` table."

However, examining the codebase, knowledge is stored **per-node** on the `Node.knowledge` field (a `KnowledgeNode` struct with `findings: HashMap<String, String>`, `file_cache: HashMap<String, String>`, `tool_history: Vec<ToolCallRecord>`). There is no top-level "YAML `knowledge` section" — the data lives inside each node.

Meanwhile, **GOAL-1.6** (in requirements-storage.md) defines the `knowledge` table vaguely as "columns for node-level knowledge data (findings, file_cache, tool_history)" without specifying actual columns, primary key, or data types.

An implementer cannot start coding: How are `findings` (a HashMap), `file_cache` (a HashMap), and `tool_history` (a Vec of structs with tool_name, timestamp, summary) each stored? One row per finding? One JSON blob per node? Three JSON columns?

**Suggested fix for GOAL-2.5**: Replace "from the YAML `knowledge` section" with "from each node's `knowledge` field" and specify: "For each node with non-empty knowledge: findings and file_cache are stored as JSON objects in the knowledge table, tool_history is stored as a JSON array. Each node has at most one row in the knowledge table."

**Suggested fix for GOAL-1.6** (in requirements-storage.md — flagging here for cross-reference): Define explicit columns: `node_id TEXT PK FK → nodes.id`, `findings TEXT (JSON object)`, `file_cache TEXT (JSON object)`, `tool_history TEXT (JSON array)`.

---

### FINDING-2: [Check #11, #15] GOAL-2.4: Edge `confidence` field — data loss during migration

The existing `Edge` struct in `graph.rs` has a `confidence: Option<f64>` field. The SQLite `edges` schema (GOAL-1.5) only defines `weight` and `metadata` columns. GOAL-2.4 says "edges referencing the `weight` or `metadata` fields... have those values migrated" but makes no mention of `confidence`.

This means **migration silently drops `confidence` data**, violating GUARD-3 ("No data is lost during migration").

**Suggested fix**: Either:
(a) Add `confidence REAL` column to the `edges` table in GOAL-1.5 and add `confidence` to the migration field list in GOAL-2.4, OR
(b) Explicitly state in GOAL-2.4 that `confidence` is migrated into the `metadata` JSON field (e.g., `{"confidence": 0.85}`), OR
(c) Add this to Out of Scope with justification that `confidence` is unused in practice.

---

### FINDING-3: [Check #6, #9] GOAL-2.3: Migration — Node field mismatch between current Node struct and SQLite schema

GOAL-2.3 lists the fields to be migrated: "id, title, status, description, assigned_to, tags, priority, node_type, and all metadata key-value pairs."

But the SQLite `nodes` table (GOAL-1.2) has ~20 dedicated columns including `file_path`, `lang`, `start_line`, `end_line`, `signature`, `visibility`, `doc_comment`, `body_hash`, `node_kind`, `owner`, `source`, `repo`, `created_at`, `updated_at`.

In the current codebase, fields like `file_path`, `signature`, `line` are stored in `Node.metadata` HashMap (see `unified.rs` lines 44-53). The migration requirement doesn't specify:
- Do metadata keys matching column names get promoted to columns? (e.g., `metadata["file_path"]` → `nodes.file_path`)
- Or do they stay in `node_metadata`?

Without this, two engineers would implement differently.

**Suggested fix**: Add to GOAL-2.3: "During migration, metadata keys that match a dedicated column name in the `nodes` table (`file_path`, `lang`, `start_line`, `end_line`, `signature`, `visibility`, `doc_comment`, `body_hash`, `node_kind`, `owner`, `source`, `repo`, `created_at`, `updated_at`) are written to those columns. Remaining metadata keys go to `node_metadata`."

---

### FINDING-4: [Check #1, #5] GOAL-4.4: Relevance ranking — "direct call edges" and "type reference edges" undefined

GOAL-4.4 specifies ranking: "Direct call edges rank highest, followed by type reference edges, then same-file edges, then transitive edges."

The current codebase uses `EdgeRelation` enum with variants: `Imports`, `Calls`, `Contains`, `Inherits`, `Implements`, `Uses`, `TypeReference`, `Decorator`, `Overrides`, `ReExports`. And `Edge.relation` in graph.rs is a free-form `String` (with values like `"depends_on"`, `"blocks"`, `"part_of"`).

Which `relation` values map to "direct call edges"? Just `"calls"`? Or also `"imports"` + `"uses"`? Which are "type reference edges"? Just `"type_reference"`? Also `"inherits"` + `"implements"`?

**Suggested fix**: Add an explicit mapping table in GOAL-4.4:
```
| Rank | Category | edge.relation values |
|------|----------|---------------------|
| 1 | Direct call | calls, imports |
| 2 | Type reference | type_reference, inherits, implements, uses |
| 3 | Same-file | contains (when from_node and to_node share file_path) |
| 4 | Structural | depends_on, part_of, blocks |
| 5 | Transitive | any relation at hop > 1 |
```

---

## 🟡 Important (should fix before implementation)

### FINDING-5: [Check #2, #3] GOAL-3.1: History save — no specification of SQLite backup method

GOAL-3.1 says "creates a complete copy of `.gid/graph.db` at `.gid/history/{timestamp}.db`." The feature overview mentions "SQLite's backup API" but the GOAL itself doesn't specify whether to use:
- `sqlite3_backup_init` / `sqlite3_backup_step` (online backup API — safe during concurrent reads)
- File system copy (`fs::copy`) — only safe if no concurrent writer

This matters because GUARD-8 says "concurrent read operations never block each other" and using `fs::copy` during active writes could produce a corrupt snapshot.

**Suggested fix**: Add to GOAL-3.1: "The copy uses rusqlite's `backup` method (wrapping SQLite's online backup API) to ensure a consistent snapshot even if reads are in progress."

---

### FINDING-6: [Check #10] GOAL-3.5: History restore — race condition with auto-save

GOAL-3.5 says "first auto-saves the current state as a new checkpoint (with message 'auto-save before restore'), then replaces `.gid/graph.db` with the specified snapshot."

What happens if the auto-save would exceed the 50-snapshot limit (GOAL-3.3)? It deletes the oldest snapshot. If the user is restoring an old snapshot that happens to BE the oldest, it gets deleted before restore.

**Suggested fix**: Add: "The auto-save step does not count against the 50-snapshot limit, OR the system verifies the target snapshot is not the one that would be deleted before proceeding."

---

### FINDING-7: [Check #4] GOAL-2.8: Compound requirement — two error cases in one GOAL

GOAL-2.8 specifies two distinct error scenarios: (1) `.gid/graph.db` already exists, (2) `.gid/graph.yml` does not exist. These should be independently testable.

**Suggested fix**: Split into:
- **GOAL-2.8a**: Running `gid migrate` when `.gid/graph.db` already exists → specific error + non-zero exit.
- **GOAL-2.8b**: Running `gid migrate` when `.gid/graph.yml` does not exist → specific error + non-zero exit.

---

### FINDING-8: [Check #1, #3] GOAL-4.2: Token estimation — "character-based heuristic" is underspecified

GOAL-4.2 says "1 token ≈ 4 characters" but doesn't specify:
- Is this counting the raw content bytes, or the formatted output bytes?
- Does this include section headers, formatting delimiters, and structural overhead?
- What character encoding? UTF-8 byte count or Unicode codepoint count?
- "unless a more precise tokenizer is configured" — how? Via config file? CLI flag? API parameter?

**Suggested fix**: Clarify: "Token estimation counts UTF-8 bytes of the final formatted output divided by 4. The `--max-tokens` budget applies to the entire output including headers and formatting. The token estimation method is not configurable in v1."

---

### FINDING-9: [Check #9] GOAL-3.3: History retention — boundary behavior unspecified

GOAL-3.3 says "retains a maximum of 50 snapshots" and "oldest snapshot is deleted before the new one is created." But:
- What if there are already >50 snapshots (e.g., from manual copying)? Delete until ≤50? Delete only one?
- Is the 50-limit configurable? (The current code has `const MAX_HISTORY_ENTRIES: usize = 50`)
- Does the count include auto-save snapshots from restore operations (GOAL-3.5)?

**Suggested fix**: Add: "If more than 50 snapshots exist when saving, delete the oldest snapshots until exactly 50 remain. The limit is hardcoded at 50 in v1. Auto-save snapshots from restore operations count against this limit."

---

### FINDING-10: [Check #2] GOAL-4.3: Truncation — "furthest hops removed first" has ambiguous tie-breaking

GOAL-4.3 says truncation removes "furthest hops first." But at the same hop distance, which nodes get removed? By relevance score (GOAL-4.4)? Alphabetical? Arbitrary?

**Suggested fix**: Add: "Within the same hop distance, nodes are removed in ascending relevance score order (lowest relevance removed first)."

---

### FINDING-11: [Check #7] GOAL-2.3/2.4: Migration — no handling of corrupt or partial YAML

GOAL-2.3 and 2.4 describe reading from YAML, but what happens if:
- The YAML file is malformed (parse error)?
- The YAML has nodes with duplicate IDs?
- An edge references a node ID that doesn't exist in the nodes list?

GUARD-3 says "no data is lost" but doesn't cover "data was already broken in YAML."

**Suggested fix**: Add a GOAL: "If the YAML file fails to parse, `gid migrate` exits with an error describing the parse failure. If edges reference non-existent nodes, they are still migrated (since SQLite FK enforcement may reject them — document the behavior). If duplicate node IDs exist, the last one in the YAML takes precedence and a warning is emitted."

---

### FINDING-12: [Check #18] GOAL-4.1: Context assembly — where does node content come from?

GOAL-4.1 says output includes "full details of each target node (title, file_path, signature, doc_comment, description)." But for an AI agent to have useful context, it typically needs the **actual source code**, not just metadata. The requirement doesn't mention:
- Is actual source code included in context output?
- If so, where does it come from? Read from disk using `file_path` + `start_line`/`end_line`?
- Or is only graph metadata included?

This is the **core value proposition** of `gid context` — if it only outputs metadata, it's far less useful than providing source code.

**Suggested fix**: Either (a) add a field specifying that source code is included by reading the file at `file_path` between `start_line` and `end_line`, or (b) explicitly state "source code retrieval is not in scope for v1; only graph metadata is included."

---

### FINDING-13: [Check #5] GOAL-3.2: Metadata storage — "either in the snapshot itself or in a `.gid/history/index.json`"

GOAL-3.2 uses "either/or" language, giving the implementer a design choice. But GOAL-3.4 (listing history) needs to read this metadata. If the implementation stores metadata inside each snapshot DB, listing requires opening every snapshot DB. If stored in `index.json`, listing is fast but risks index/snapshot desync.

**Suggested fix**: Choose one: "Metadata is stored in `.gid/history/index.json` as a JSON array. The file is the source of truth for listing. When a snapshot is deleted, its entry is removed from the index."

---

### FINDING-14: [Check #16] GOAL-4.7: Depth parameter — interaction with token budget undefined

GOAL-4.7 says `--depth` controls max traversal depth (default: 3). GOAL-4.3 says truncation removes furthest hops first. But:
- If `--depth 5` with `--max-tokens 1000`: does depth cap the traversal BEFORE budget truncation, or does budget truncation FURTHER reduce depth?
- Answering: depth is a hard cap (never traverse beyond N), budget is a soft cap (include as much as fits). But this should be explicit.

**Suggested fix**: Add: "`--depth` is a hard traversal limit applied before token budget truncation. The system never traverses beyond `depth` hops regardless of remaining budget. Token budget truncation then further reduces included nodes within the traversed set."

---

### FINDING-15: [Check #13] GOAL-4.12: Priority inversion — P2 GOAL is architectural foundation

GOAL-4.12 [P2] says "The context assembly logic is implemented in `gid-core` as a library function... The CLI is a thin wrapper over this library function." But GOAL-4.1 [P0] describes the CLI command. If GOAL-4.12 is deprioritized, the P0 implementation might bake logic into the CLI, making the later refactor expensive.

**Suggested fix**: Either promote GOAL-4.12 to P0 (since it's an architectural decision, not extra work — it's actually less work to do it right from the start), or add a note: "Implementation of GOAL-4.1 through GOAL-4.11 MUST structure the code as a library function in gid-core with a thin CLI wrapper, even though GOAL-4.12 is P2."

---

### FINDING-16: [Check #8] All three documents — no observability requirements

None of the three documents specify:
- What log output does `gid migrate` produce? Progress bar? Per-node logging? Silent?
- What log output does `gid history save` produce?
- What log output does `gid context` produce for debugging context assembly decisions?
- Are there timing/performance metrics exposed?

The parent doc has no observability GUARDs either.

**Suggested fix**: Add at minimum: "All commands log progress and timing to stderr. `gid migrate` logs node/edge counts during transfer. `gid context` logs traversal statistics (nodes visited, nodes included, token budget used) to stderr at default verbosity."

---

### FINDING-17: [Check #19] GOAL-2.1/2.2: Migration — `graph.yaml` (alternate extension) not mentioned

Current `find_graph_file` in `parser.rs` searches for both `.gid/graph.yml` AND `.gid/graph.yaml`. GOALs 2.1 and 2.2 only mention `.gid/graph.yml`. What about users with `.gid/graph.yaml`?

**Suggested fix**: GOAL-2.1 and 2.3 should say "`.gid/graph.yml` (or `.gid/graph.yaml`)" or specify that the migration searches the same candidate list as `find_graph_file`.

---

## 🟢 Minor (can fix during implementation)

### FINDING-18: [Check #21] GOAL-2.8: Single GOAL covering two cases — numbering

GOAL-2.8 is a compound requirement (see FINDING-7). Even if not split, the P1 priority is questionable — error handling for "already migrated" seems P0 since users will accidentally run `gid migrate` twice. Without this, a second migration could overwrite a SQLite DB that has newer data than the YAML.

**Suggested fix**: Consider promoting to P0. (Less critical if GOAL-2.2 already prevents SQLite creation when it exists, but the UX still needs a clear error.)

---

### FINDING-19: [Check #12] Terminology — "checkpoint" vs "snapshot" vs "history entry"

The history requirements use "checkpoint" (headings, GOAL-3.5 "auto-saves the current state as a new checkpoint"), "snapshot" (GOAL-3.1 "creates a complete copy", GOAL-3.2 "filesystem-safe"), and "history entry" (code: `HistoryEntry`). These three terms are used interchangeably.

**Suggested fix**: Pick one term and use it consistently. Recommendation: "snapshot" (matches what it actually is — a copy of the DB). Update headings and GOAL text.

---

### FINDING-20: [Check #12] Terminology — "node_type" vs "node_kind" vs "kind"

The SQLite schema (GOAL-1.2) has both `node_type` and `node_kind` columns. `CodeNode` has `kind: NodeKind`. `Node` has `node_type: Option<String>`. GOAL-4.8 uses `"type:function"` as a filter pattern. The distinction between `node_type` and `node_kind` is not defined anywhere in the requirements.

**Suggested fix**: Add a glossary or note: "`node_type` is the high-level category (task, file, function, class, module, feature, component, layer, knowledge). `node_kind` is a sub-category from code extraction (e.g., NodeKind::Interface, NodeKind::Trait, NodeKind::Enum within node_type 'class')."

---

### FINDING-21: [Check #22] GOAL-3.7: Diff output — "up to 10 example IDs per category" matches code but is arbitrary

The existing `GraphDiff::fmt()` already uses `.take(10)`. This is fine, but the requirement should note whether the 10-limit applies to the library function output or only the CLI display. For machine consumers (GOAL-4.12 pattern), the full list may be needed.

**Suggested fix**: Clarify: "The CLI displays up to 10 example IDs per category. The library function returns the full lists."

---

### FINDING-22: [Check #14] GOAL-4.1: Cross-references — "sorted by relevance" references GOAL-4.4 implicitly

GOAL-4.1(b) says "direct dependencies sorted by relevance" but doesn't reference GOAL-4.4 which defines relevance. Adding an explicit reference improves traceability.

**Suggested fix**: GOAL-4.1(b): "direct dependencies sorted by relevance (per GOAL-4.4)"

---

### FINDING-23: [Check #25] GOAL-4.8: Include patterns — user perspective unclear

GOAL-4.8 says `--include "type:function"` but from the user's perspective, what are the valid type values? The user needs to know the vocabulary. This is implementation guidance that's incomplete.

**Suggested fix**: Add: "Valid type values match `node_type` column values: `task`, `file`, `function`, `class`, `module`, `feature`, `component`, `layer`, `knowledge`."

---

---

## 📊 Coverage Matrix

| Category | Covered | Missing |
|---|---|---|
| **Happy path — Migration** | GOAL-2.1, 2.2, 2.3, 2.4, 2.5, 2.6, 2.7 | — |
| **Happy path — History** | GOAL-3.1, 3.2, 3.4, 3.5, 3.7 | — |
| **Happy path — Context** | GOAL-4.1, 4.2, 4.3, 4.4, 4.6, 4.10 | — |
| **Error handling — Migration** | GOAL-2.8 (already exists, no YAML) | ⚠️ Corrupt YAML (FINDING-11), duplicate node IDs, FK violations during migration, disk full during migration, `.gid/graph.yml.bak` already exists |
| **Error handling — History** | GOAL-3.6 (invalid timestamp) | ⚠️ Disk full during snapshot, corrupt snapshot DB, history directory missing, index.json corrupt/missing |
| **Error handling — Context** | GOAL-4.6 (no targets) | ⚠️ Target node doesn't exist, graph.db doesn't exist, graph is empty, circular dependencies during traversal |
| **Performance** | GUARD-6 (10ms single op), GUARD-7 (3× size) | ⚠️ No performance target for `gid context` (how fast should context assembly be for 50k-node graph?), no performance target for `gid migrate` (acceptable time for 50k nodes?), no performance target for `gid history save`/`restore` |
| **Security** | — | ⚠️ No security requirements at all. SQLite DB file permissions? Sensitive data in context output? Path traversal in `--include` patterns? |
| **Reliability** | GUARD-1 (atomic), GUARD-3 (no data loss), GOAL-2.6 (validation) | ⚠️ No retry behavior specified. What if `gid context` hits a corrupt node? What if backup API fails mid-copy? |
| **Observability** | — | ⚠️ No logging/metrics/progress requirements for any of the 3 features (FINDING-16) |
| **Scalability** | GUARD-6 mentions 50k+ nodes | ⚠️ No scalability target for `gid context` (what's max graph size for context assembly?), no target for history snapshot count × snapshot size |
| **Concurrency** | GUARD-8 (readers don't block) | ⚠️ What happens if `gid migrate` runs while another `gid` command is using YAML? What if `gid history save` runs during `gid context`? |
| **Backward compat** | GUARD-4, GUARD-5, GOAL-2.1, 2.2 | — |
| **Data integrity** | GUARD-1, 2, 3, GOAL-2.6 | — |

---

## ✅ Passed Checks

- **Check #0: Document size** ✅ — Migration: 8 GOALs, History: 8 GOALs, Context: 12 GOALs. All ≤15. Parent doc has 0 GOALs (only GUARDs). Proper split by feature.
- **Check #1: Specificity** — 23/28 GOALs are specific enough (see FINDING-4 for GOAL-4.4, FINDING-8 for GOAL-4.2, FINDING-1 for GOAL-2.5, FINDING-3 for GOAL-2.3, FINDING-12 for GOAL-4.1). Score: 82%.
- **Check #2: Testability** — 24/28 GOALs have clear pass/fail conditions. Flagged: GOAL-4.2 (FINDING-8), GOAL-4.3 (FINDING-10), GOAL-2.5 (FINDING-1), GOAL-3.1 (FINDING-5). Score: 86%.
- **Check #3: Measurability** ✅ — Quantitative requirements (GOAL-3.3: 50 snapshots, GOAL-4.2: token budget, GOAL-3.7: 10 examples) have concrete numbers. GUARD-6 has 10ms target. Pass.
- **Check #4: Atomicity** — 27/28 GOALs are atomic. GOAL-2.8 is compound (FINDING-7). Score: 96%.
- **Check #5: Completeness** — 23/28 GOALs specify actor/trigger/behavior/outcome. Flagged: GOAL-2.5 (no clear outcome structure — FINDING-1), GOAL-1.6 (vague — referenced in FINDING-1), GOAL-4.1 (incomplete on content — FINDING-12), GOAL-4.4 (incomplete mapping — FINDING-4), GOAL-3.2 (either/or — FINDING-13). Score: 82%.
- **Check #6: Happy path coverage** ✅ — All main user flows covered: migrate, save/list/restore/diff history, assemble context.
- **Check #7: Error/edge case coverage** — Partial. Migration covers "already done" and "no YAML" but misses corrupt YAML (FINDING-11). History covers invalid timestamp but misses disk full / corrupt snapshot. Context covers no-targets but misses non-existent targets.
- **Check #8: Non-functional requirements** — Security and observability entirely missing (FINDING-16). Performance partially covered via GUARDs.
- **Check #9: Boundary conditions** — GOAL-3.3 has ≤50 limit but boundary edge case flagged (FINDING-9). GOAL-4.2 token budget at 0 unspecified. GOAL-4.7 depth=0 unspecified.
- **Check #10: State transitions** — Migration has clear states (YAML-only → both → SQLite-preferred). History has clear states. No unreachable or exit-less states. ✅
- **Check #11: Internal consistency** — One contradiction found: Edge `confidence` field vs. schema (FINDING-2). Otherwise consistent.
- **Check #12: Terminology consistency** — "checkpoint"/"snapshot" inconsistency (FINDING-19). "node_type"/"node_kind" ambiguity (FINDING-20). Otherwise consistent.
- **Check #13: Priority consistency** — One priority inversion: GOAL-4.12 P2 is architectural prerequisite for GOAL-4.1 P0 (FINDING-15). Otherwise sound.
- **Check #14: Numbering/referencing** ✅ — All GOAL IDs are unique across documents (2.x, 3.x, 4.x scheme). No gaps. Cross-references to parent doc resolve correctly.
- **Check #15: GUARDs vs GOALs alignment** — GUARD-3 conflicts with GOAL-2.4 on `confidence` field (FINDING-2). All other GUARDs are compatible with GOALs.
- **Check #16: Technology assumptions** ✅ — SQLite + rusqlite + WAL mode explicitly chosen and documented. Backup API mentioned in overview. Character-based tokenizer is explicit (though underspecified per FINDING-8).
- **Check #17: External dependencies** ✅ — rusqlite (bundled), serde_json, chrono all documented in parent requirements. No external service dependencies.
- **Check #18: Data requirements** — Context output data sources partially unclear (FINDING-12). Migration data mapping partially unclear (FINDING-3).
- **Check #19: Migration/compatibility** — `.gid/graph.yaml` alternate extension not handled (FINDING-17). Otherwise well-covered by GUARD-4, GUARD-5.
- **Check #20: Scope boundaries** ✅ — Parent doc has explicit "Out of Scope" section covering remote storage, GUI, multi-user writes, schema migrations, code extraction changes, YAML write path. Well-defined.
- **Check #21: Unique identifiers** ✅ — All 28 GOALs have unique IDs. No duplicates. No gaps within feature ranges.
- **Check #22: Grouping/categorization** ✅ — Requirements are organized by feature (3 docs) and by sub-feature within each doc (headings: Auto-Detection, Migration Execution, Backup, Error Cases, etc.).
- **Check #23: Dependency graph** — Dependencies are mostly implicit. GOAL-3.1 depends on storage layer existing. GOAL-4.1 depends on graph being in SQLite. GOAL-2.3 depends on schema from GOAL-1.1–1.5. No circular dependencies. But explicit dependency annotations would help implementation ordering.
- **Check #24: Acceptance criteria** — GOALs are their own acceptance criteria (they specify behavior). No separate acceptance criteria section, but each GOAL is specific enough to serve as one (with exceptions noted above).
- **Check #25: User perspective** ✅ — All three documents write requirements from the user's perspective: "Running `gid migrate`...", "Running `gid history save`...", "Running `gid context`...". Excellent.
- **Check #26: Success metrics** — No production observability metrics defined beyond "tests pass." How would you know `gid context` is actually useful in production? (Minor concern for v1.)
- **Check #27: Risk identification** — No explicit risk flagging. High-risk items: context assembly relevance ranking (novel algorithm), SQLite backup during concurrent access, token estimation accuracy. These should be marked for spike/prototype.

---

## Summary

| Metric | Value |
|--------|-------|
| **Total requirements reviewed** | 28 GOALs (8 migration + 8 history + 12 context) |
| **Cross-cutting GUARDs referenced** | 10 (6 hard / 4 soft) |
| **Critical findings** | 4 (FINDING-1, 2, 3, 4) |
| **Important findings** | 13 (FINDING-5 through 17) |
| **Minor findings** | 6 (FINDING-18 through 23) |
| **Total findings** | 23 |
| **Coverage gaps** | Security (none), Observability (none), Error handling (partial), Performance targets for new features (none) |
| **Recommendation** | **Needs fixes before implementation** — 4 critical findings involve data loss risk (FINDING-2), schema ambiguity (FINDING-1), field mapping gaps (FINDING-3), and undefined ranking categories (FINDING-4). These would cause implementation rework. |
| **Estimated implementation clarity** | **Medium** — Migration and History are close to implementation-ready after critical fixes. Context assembly needs more specification work (relevance ranking, source code inclusion, truncation tie-breaking). |

### Priority fix order:
1. **FINDING-2** (Critical) — Edge confidence data loss, contradicts GUARD-3
2. **FINDING-1** (Critical) — Knowledge table schema undefined, blocks implementation
3. **FINDING-3** (Critical) — Node field mapping ambiguous, blocks migration implementation
4. **FINDING-4** (Critical) — Relevance ranking undefined, blocks context implementation
5. **FINDING-12** (Important) — Source code in context output, core value proposition unclear
6. **FINDING-5** (Important) — Backup method unspecified, correctness risk
7. **FINDING-13** (Important) — Metadata storage location, blocks history list implementation
8. Remaining findings in ID order

---

## ✅ All Findings Applied (2026-04-06)

All 23 findings (FINDING-1 through FINDING-23) have been applied to `requirements-migration.md`, `requirements-history.md`, `requirements-context.md`, and `requirements.md`.
