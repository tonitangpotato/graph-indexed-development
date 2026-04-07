# Design Review: ISS-009 Phase 1 — Cross-Layer Graph Connections

**Reviewed:** 2026-04-07
**Document:** `.gid/features/iss-009-cross-layer/design.md`
**Reviewer:** RustClaw
**Depth:** Full (28 checks)

---

## 🔴 Critical (blocks implementation)

### FINDING-1: `impact()` and `deps()` are in `query.rs` on `Graph`, not `graph.rs` — ✅ Applied
**[Check #27: API compatibility]** Design §3.6 says "Current `impact()` in `graph.rs`" and §4 lists `graph.rs` as the file to change. But actual code has `impact()` and `deps()` in **`query.rs`** (line 16, 40) on `QueryEngine<'a>`. The CodeGraph has a separate `impact_analysis()` in `code_graph/analysis.rs`. The design needs to specify which one it's modifying and update the file list.

Additionally, `QueryEngine` operates on the task `Graph` (which has `Node` with `depends_on: Vec<String>`), while code graph has `CodeGraph` with `CodeEdge` that has `EdgeRelation`. These are **different types with different traversal mechanisms**. The design conflates them. The query enhancement in §3.6 needs to clarify:
- Is it enhancing `QueryEngine::impact()` (task graph) to also traverse code edges?
- Or adding multi-relation traversal to `CodeGraph::impact_analysis()`?
- Or both?

**Suggested fix:** Split §3.6 into two parts: (1) `CodeGraph` analysis enhancement (traverses `CodeEdge.relation`), (2) `QueryEngine` enhancement if needed. Update §4 file list: `query.rs` instead of `graph.rs`, possibly `code_graph/analysis.rs`.

**✅ Applied:** Split §3.6 into §3.6.1 (CodeGraph analysis enhancement) and §3.6.2 (QueryEngine enhancement). Updated §4 file list: replaced `graph.rs` with `query.rs` and added `code_graph/analysis.rs`. Added clarifying note about the two distinct systems. Updated §5.3 test names to distinguish code graph vs query engine tests.

### FINDING-2: `CodeEdge::new()` doesn't set `confidence` — design uses struct literal with `confidence: 0.8` but the constructor sets it to `1.0` — ✅ Applied
**[Check #6: Data flow completeness]** §3.4 TestsFor edges use struct literal:
```rust
edges.push(CodeEdge {
    confidence: 0.8,
    ...
});
```
But `CodeEdge::new()` (types.rs:320) sets `confidence: 1.0`. The design correctly avoids the constructor to set 0.8, but the Module `belongs_to` edges in §3.2 use `CodeEdge::new()` which would get confidence 1.0. Is that intentional? Module→parent belongs_to is deterministic (confidence 1.0 is correct), but file→module belongs_to is also deterministic. Meanwhile TestsFor is heuristic (0.8 correct).

This is internally consistent but worth documenting: **belongs_to edges should be 1.0 (deterministic), TestsFor should be 0.8 (heuristic)**. Add a helper like `CodeEdge::new_with_confidence()` or just document the pattern.

**Suggested fix:** Add a note in §3.3 or §3.4 explicitly stating confidence semantics: belongs_to=1.0 (deterministic), TestsFor=0.8 (naming heuristic). Consider adding `CodeEdge::new_heuristic(from, to, relation, confidence)` constructor.

**✅ Applied:** Added "Confidence semantics" block in §3.3 documenting belongs_to=1.0 (deterministic) and TestsFor=0.8 (heuristic). Included suggestion for `CodeEdge::new_heuristic()` constructor.

---

## 🟡 Important (should fix before implementation)

### FINDING-3: `NodeKind::Module` already exists but may not serialize correctly — ✅ Applied
**[Check #26: Existing code alignment]** The design says `NodeKind::Module` "exists in types.rs". Need to verify it's in serde Serialize/Deserialize derives and that the YAML serialization handles it. If NodeKind uses `#[serde(rename_all = "snake_case")]`, "module" should work. But if it doesn't serialize, the graph.yml output will break.

**Suggested fix:** Add a test: create a Module node, serialize to YAML, deserialize, verify roundtrip.

**✅ Applied:** Added test case `test_nodekind_module_serde_roundtrip` to §5.1 (test #6).

### FINDING-4: Root-level files have no belongs_to — but `test_file_belongs_to_module` doesn't test this case — ✅ Applied
**[Check #7: Error handling]** §6 Edge Cases says "Root directory files → no module node, no belongs_to edge." But §5.1 test 4 (`test_file_belongs_to_module`) says "each file has belongs_to edge to its directory's module." This contradicts root-level files which have no module. Test should explicitly verify root files have NO belongs_to edge.

**Suggested fix:** Add test case: `test_root_file_no_belongs_to` — `main.rs` at root → no belongs_to edge.

**✅ Applied:** Fixed test 4 description to say "each non-root file". Added test 5 `test_root_file_no_belongs_to`. Updated §6 edge case to reference the new test.

### FINDING-5: Rust test stem extraction has a logic bug — ✅ Applied
**[Check #5: State machine trace]** §3.4 Rust TestsFor:
```rust
let test_stem = path
    .strip_prefix("tests/").unwrap_or(path)
    .trim_end_matches(".rs")
    .strip_prefix("test_").unwrap_or(
        path.strip_prefix("tests/").unwrap_or(path).trim_end_matches(".rs")
    );
```
The fallback of `strip_prefix("test_")` re-strips `tests/` from the original `path` instead of using the already-stripped value. This means if path is `tests/auth.rs`:
- First: strip "tests/" → "auth.rs", trim ".rs" → "auth"
- strip_prefix("test_") on "auth" → None
- Fallback: strip "tests/" from original path → "auth.rs", trim ".rs" → "auth" ✓ (correct by accident)

But if path is `tests/test_auth.rs`:
- First: strip "tests/" → "test_auth.rs", trim ".rs" → "test_auth"
- strip_prefix("test_") → Some("auth") ✓

It works but the fallback logic is unnecessarily convoluted. Simplify to:
```rust
let raw = path.strip_prefix("tests/").unwrap_or(path).trim_end_matches(".rs");
let test_stem = raw.strip_prefix("test_").unwrap_or(raw);
```

**Suggested fix:** Simplify the stem extraction as shown above.

**✅ Applied:** Replaced convoluted stem extraction in §3.4 with simplified two-line version.

### FINDING-6: TypeScript source_stem construction is buggy for `.test.ts` → `.ts` replacement — ✅ Applied
**[Check #8: String operations]** §3.4 TypeScript TestsFor:
```rust
let source_stem = path
    .replace(".test.", ".")
    .replace(".spec.", ".")
    ...
```
For `auth.test.ts` → `auth.ts` → trim `.ts` → `auth` ✓
For `__tests__/auth.ts` → `auth.ts` (after replace `__tests__/`) → trim `.ts` → `auth` ✓
But for `components/__tests__/auth.test.ts`:
- replace `__tests__/` → `components/auth.test.ts`... wait, `.replace()` only replaces first occurrence and `__tests__/` is removed → `components/auth.test.ts`
- replace `.test.` → `components/auth.ts`
- trim `.ts` → `components/auth`

This would need to match `components/auth` in source_files, but source files are stored with full relative path stems. The matching should work IF source files include directory paths in their stems. Verify this is consistent.

**Suggested fix:** Add a test for nested `__tests__` directories: `src/components/__tests__/Button.test.tsx` → should match `src/components/Button.tsx`.

**✅ Applied:** Added test case `test_ts_nested_tests_dir` to §5.2 (test #12).

### FINDING-7: `extract_incremental()` doesn't exist yet — design references it as existing — ✅ Applied
**[Check #22: Missing helpers]** §3.7 references `extract_incremental()` but ISS-006 (incremental extract) hasn't been implemented yet. This creates a dependency: ISS-009 GOAL-7 (incremental module handling) depends on ISS-006 being implemented first.

**Suggested fix:** Move GOAL-7 to a separate section marked "depends on ISS-006 completion" or implement it as part of ISS-006.

**✅ Applied:** Marked GOAL-7 in §2 as depending on ISS-006. Added blocking note to §3.7. Added "blocked on ISS-006" annotation to §5.4. Updated §4 to note `extract_incremental()` integration is deferred.

### FINDING-8: No `FromStr` or deserialization for `BelongsTo` — ✅ Applied
**[Check #1: Every type fully defined]** §3.3 adds `BelongsTo` to `EdgeRelation` enum. The `Display` impl is shown in the existing code (each variant has a `write!`). But does the corresponding `FromStr` or serde deserialization handle "belongs_to" → `BelongsTo`? Need to verify the existing pattern handles new variants automatically (if using derive macros) or add manual impl.

**Suggested fix:** Verify serde derive handles it, or add to manual `FromStr`/`Deserialize` impl. Add a roundtrip test.

**✅ Applied:** Added serde/FromStr verification note to §3.3. Added test case `test_edge_relation_belongs_to_roundtrip` to §5.1 (test #7).

---

## 🟢 Minor (can fix during implementation)

### FINDING-9: Module node `file_path` field stores directory path — semantic mismatch — ✅ Applied
**[Check #4: Naming consistency]** `CodeNode::new_module()` sets `file_path: dir_path.to_string()`. For file nodes, `file_path` is a file. For module nodes, it's a directory. This works but is semantically imprecise. Consider using `path` as the field name (in a future refactor) or document the overloading.

**✅ Applied:** Added documentation note in §3.1 about the `file_path` overloading (stores directory path for module nodes). Added inline comment in the `new_module()` code. Noted future refactor to rename to `path`.

### FINDING-10: No mention of `.gidignore` interaction with module generation — ✅ Applied
**[Check #15: Configuration]** Module nodes are generated from directories containing source files. But `.gidignore` can exclude certain directories. Verify that the walkdir filtering (which already respects `.gidignore`) runs before `generate_module_nodes()` — otherwise excluded directories would still get module nodes.

**✅ Applied:** Added note in §3.2 clarifying that `.gidignore` filtering runs before `generate_module_nodes()` and that the function only receives already-filtered `file_entries`.

### FINDING-11: Missing `call_site_line` and `call_site_column` in belongs_to edges — ✅ Applied
**[Check #3: No dead definitions]** The design uses `CodeEdge::new()` for belongs_to, which sets `call_site_line: None` and `call_site_column: None`. This is correct (belongs_to has no call site), but these fields are semantically wrong for structural edges. Not blocking — just a code smell in the CodeEdge type design.

**✅ Applied:** Documented in §3.3 confidence semantics block — `call_site_line`/`call_site_column` are `None` for structural edges, acknowledged as code smell, accepted as non-blocking.

---

## ✅ Passed Checks

- **Problem statement**: Clear, well-scoped with 4 specific problems ✅
- **Goals/Non-goals**: 7 goals, 3 non-goals, properly bounded ✅
- **Trade-offs documented**: §7 explains naming convention vs import analysis trade-off ✅
- **Module generation logic**: Complete with ancestor traversal ✅
- **Testing plan**: 16 tests covering all components ✅
- **Edge cases**: 6 scenarios documented ✅
- **Phase boundary**: Clear separation of Phase 1 (deterministic) vs Phase 2 (unified.rs) ✅
- **Backward compatibility**: CLI defaults to full relation set ✅
- **Confidence field usage**: Appropriate — 1.0 for deterministic, 0.8 for heuristic ✅

## 📊 Summary

| Severity | Count |
|----------|-------|
| 🔴 Critical | 2 |
| 🟡 Important | 6 |
| 🟢 Minor | 3 |

**Overall assessment:** Solid design with good scope control. The two critical issues are: (1) wrong file reference for query functions (query.rs not graph.rs, and task Graph vs CodeGraph conflation), and (2) confidence constructor pattern needs explicit documentation. The important findings are mostly edge cases in string matching logic and a dependency on ISS-006. After fixes, ready for implementation.

**Recommendation:** Fix FINDING-1 (clarify query enhancement scope) and FINDING-2 (confidence semantics) first, then apply FINDING-3~8 during implementation.
