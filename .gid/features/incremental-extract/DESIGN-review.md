# Design Review: ISS-006 Incremental Updates for `gid extract`

**Reviewed:** 2026-04-06
**Document:** `.gid/features/incremental-extract/DESIGN.md`
**Reviewer:** RustClaw

---

## 🔴 Critical (blocks implementation)

*None found.*

## 🟡 Important (should fix before implementation)

### FINDING-1: Race condition on mtime check ✅ Applied
**[Concurrency]** Mtime-first check has a TOCTOU window: file could be written between mtime read (during scan) and content hash computation. In practice this is low-risk for a CLI tool (user runs `gid extract` manually), but worth documenting as a known limitation.

**Suggested fix:** Add a note in Edge Cases: "File modified during extract → may miss the change; next extract will catch it."

**Resolution:** Edge Cases table includes this row. Already documented.

### FINDING-2: Dangling edge cleanup is under-specified ✅ Applied
**[Completeness]** Design says "Edges to deleted nodes become dangling → removed in cleanup" but doesn't specify:
- When does cleanup run? After Phase 1 (remove stale) or after Phase 3 (resolve refs)?
- Does it also handle edges FROM unchanged files TO deleted nodes? (e.g., file A calls function in deleted file B — file A is unchanged, so its edges aren't re-resolved)

**Suggested fix:** Add to Phase 1 or between Phase 3 and 4: "Scan ALL edges; remove any edge where source or target node_id no longer exists in the graph."

**Resolution:** Added dangling edge cleanup step to Phase 1 in flow diagram. Edge Cases table also documents this for the "Deleted file was imported by others" scenario.

### FINDING-3: `edge_count` in FileState is lossy ✅ Applied
**[Data Model]** `FileState` stores `edge_count: usize` but Phase 1 needs to remove specific edges from a file. With only a count, you can't identify which edges to remove. You'd need either:
- Store edge source/target pairs, or
- Store edge indices into the graph's edge list, or
- Derive edges at removal time by matching `source` node_ids (since `node_ids` are stored)

The third option works (edges whose source is in `file.node_ids` belong to that file), making `edge_count` redundant for removal — it's only useful for reporting.

**Suggested fix:** Either (a) document that edge removal uses `node_ids` to find edges, making `edge_count` a reporting-only field, or (b) replace `edge_count` with `edge_keys: Vec<(String, String)>` for direct removal.

**Resolution:** Option (a) applied. `edge_count` field comments clarify it is reporting-only; edge removal uses `node_ids` to find edges where source ∈ node_ids.

### FINDING-4: No versioning on extract-meta.json ✅ Applied
**[Robustness]** If the struct changes between gid versions (add fields, rename fields), old metadata files will fail to deserialize. Design should include a schema version.

**Suggested fix:** Add `pub version: u32` to `ExtractMetadata`. On version mismatch → full rebuild (same as corrupted).

**Resolution:** `ExtractMetadata` includes `version: u32` field with doc comment. Edge Cases table includes "Version mismatch in metadata → full rebuild" row.

## 🟢 Minor (can fix during implementation)

### FINDING-5: File rename detection could be noted as future improvement
**[Documentation]** Design correctly notes rename = delete + add, which re-creates node IDs. This means a simple rename causes all edges to/from that file's nodes to be rebuilt. Fine for now, but content-hash-based rename detection (same hash, different path) could be a future optimization.

### FINDING-6: xxHash64 collision probability not discussed
**[Correctness]** xxHash64 collision probability is ~1/2^64, effectively zero for codebases. Worth a one-liner to preempt the question.

### FINDING-7: Performance target "<100ms for no changes" assumes metadata is small
**[Performance]** Loading + deserializing `extract-meta.json` for a 10K-file project could approach the 100ms target. Probably fine for typical projects (80-500 files) but worth noting the scaling assumption.

---

## ✅ Passed Checks

- **Problem statement**: Clear, quantified (80 files, ~10 min current, specific scenarios) ✅
- **Approach**: Well-scoped (file-level delta, not AST-level) with good justification ✅
- **Data structures**: Clean, serializable, minimal ✅ (minor: FINDING-3 on edge_count)
- **Flow diagram**: Complete 5-phase pipeline with clear decision points ✅
- **Design decisions**: All 5 decisions are justified with rationale ✅
- **API surface**: Minimal, clean, backwards-compatible (full rebuild as fallback) ✅
- **Edge cases**: Comprehensive table covering 7 scenarios ✅
- **Performance targets**: Concrete numbers with current vs target comparison ✅
- **Scope boundaries**: Clear "Not In Scope" section prevents scope creep ✅
- **File changes**: Specific file list with nature of change ✅
- **LSP daemon synergy**: Good integration story with existing architecture ✅

## 📊 Summary

| Severity | Count |
|----------|-------|
| 🔴 Critical | 0 |
| 🟡 Important | 4 |
| 🟢 Minor | 3 |

**Overall assessment:** Solid, well-scoped design. The 4 important findings are all fixable with small doc additions — no architectural changes needed. Ready for implementation after fixes.

**Recommendation:** Apply FINDING-1 through FINDING-4, then proceed to implementation.
