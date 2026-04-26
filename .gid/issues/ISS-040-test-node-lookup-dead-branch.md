# ISS-040: Causal-analysis test-node lookup has dead fallback branch

**Status:** closed
**Resolution:** fixed
**Priority:** P0 (correctness bug)
**Component:** code_graph (analysis, test_analysis)
**Filed:** 2026-04-26
**Closed:** 2026-04-26
**Discovered by:** clippy `overly_complex_bool_expr` flagging dead boolean term

---

## Symptom

`cargo clippy` produces two **errors** (not warnings):

```
error: this boolean expression contains a logic bug
   --> crates/gid-core/src/code_graph/analysis.rs:436:21
    |
436 | /                     n.name == short_name
437 | |                         || n.name.ends_with(short_name)
438 | |                         || (n.file_path.contains("/test") && n.name == short_name)
```

Same pattern at `crates/gid-core/src/code_graph/test_analysis.rs:41`.

The third disjunct `(n.file_path.contains("/test") && n.name == short_name)` is **strictly subsumed** by the first disjunct `n.name == short_name`. The third branch can never contribute a match — it is dead code.

## Root Cause (intent vs. behavior)

These call sites map a pytest-style test identifier such as `tests/test_foo.py::test_bar` into a node in the code graph. After splitting on `::`, `short_name = "test_bar"`. The lookup then tries:

1. exact name match (`n.name == short_name`)
2. suffix match (`n.name.ends_with(short_name)`) — handles namespaced names like `mod::test_bar`
3. **(intended)** path-based fallback for cases where the graph node's `name` field doesn't carry the test function name at all (e.g. file-level test nodes, or when the extractor stores the file basename in `name`)

The author wrote disjunct (3) as `file_path.contains("/test") && name == short_name`, which is identical to disjunct (1) restricted to test files — not a fallback at all. The path-based fallback was lost in a likely copy-paste.

## Impact

When neither (1) nor (2) match, the lookup returns `None`, and:

- `trace_causal_chains` (the regression-explanation path) silently skips the test and emits no causal chain
- `analyze_test_failures` likewise emits a degraded "❌ {short_name}" header with no analysis below

Concretely: when a P2P regression test's graph node was extracted with a name that doesn't end in the test function name (e.g., a Python class-method test like `TestClass::test_method` where the node's `name` is `TestClass`), the diagnostic that's supposed to walk the dependency graph and explain *why* the test broke prints nothing useful. The user sees only the test name and no causal chain, defeating the entire point of the analysis.

This is a silent correctness bug — no error, just degraded output.

## Acceptance Criteria

- AC1: Both `analysis.rs:436` and `test_analysis.rs:41` no longer trigger `clippy::overly_complex_bool_expr`
- AC2: The lookup correctly resolves test nodes via a path-based fallback when the `name` field doesn't match — covered by a new unit test that constructs a `CodeGraphAnalyzer` with a test node whose `name != short_name` but whose `file_path` ends with the inferred test-file path
- AC3: `cargo test -p gid-core code_graph::` passes
- AC4: `cargo clippy --all-targets --all-features` no longer reports these two errors

## Fix Plan

1. Replace the dead third disjunct with a real path-based fallback. The pytest identifier is structured as `<file_path>::<short_name>`. When the `::` split yields a non-trivial prefix, that prefix is the test file path. A node whose `file_path.ends_with(prefix)` and which is itself a test (function in a test file) is the target.
2. Extract the lookup into a private helper (`find_test_node_by_pytest_id`) shared between `analysis.rs` and `test_analysis.rs` to avoid the same bug recurring.
3. Add a focused unit test that constructs the failure mode (test node with mismatched `name`) and asserts the helper still finds it.

---

## Resolution

Implemented the fix described above in commit on branch `iss-001-002-revive`:

- Added `CodeGraph::resolve_pytest_id()` (private helper in `code_graph/test_analysis.rs`) implementing 5-rule lookup:
  1. exact name + file_path-suffix match
  2. suffix name + file_path-suffix match
  3. exact name match anywhere
  4. suffix name match anywhere
  5. file-level fallback (any `is_test=true` node in the matching file)
- Replaced both buggy call sites (`analysis.rs:436`, `test_analysis.rs:41`) with a single call to the helper.
- Added 5 regression tests in `code_graph/tests.rs::tests`:
  - `test_resolve_pytest_id_disambiguates_by_file_path` — proves the fix: same `short_name` in two files now resolves to the right one
  - `test_resolve_pytest_id_file_level_fallback`
  - `test_resolve_pytest_id_bare_name`
  - `test_resolve_pytest_id_suffix_match_with_file`
  - `test_resolve_pytest_id_no_match`

Verification:
- `cargo test --workspace --all-features` → 1243 passed, 0 failed (was 1238, +5 new tests).
- `cargo clippy --all-targets --all-features 2>&1 | grep "boolean"` → no matches (the two `overly_complex_bool_expr` errors are gone).
- Build clean.

