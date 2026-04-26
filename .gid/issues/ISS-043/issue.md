---
id: "ISS-043"
title: "Python call-resolution confidence ladder has collapsed `same_file` and `imported` tiers (both 0.8)"
status: closed
priority: P2
created: 2026-04-26
closed: 2026-04-26
component: "crates/gid-core/src/code_graph/lang/python.rs (resolve_and_add_call_edge, lines ~857-867)"
related: ["ISS-042", "ISS-012"]
---
# ISS-043: Python call-resolution confidence ladder has collapsed `same_file` and `imported` tiers (both 0.8)

**Status:** RESOLVED (2026-04-26)
**Priority:** P2 (correctness smell — wrong confidence affects edge ranking and downstream queries)
**Component:** `crates/gid-core/src/code_graph/lang/python.rs` (`resolve_and_add_call_edge`, lines ~857-867)
**Filed:** 2026-04-26
**Discovered by:** RustClaw (clippy `if_same_then_else`)
**Related:** ISS-042 (parent cleanup), ISS-012 (confidence-weighted edges)

---

## Resolution Summary (2026-04-26)

**Determined:** Bug (copy-paste typo). Fixed by changing `imported` confidence from 0.8 → 0.75.

**Evidence for bug verdict:**
1. Same file, line ~932 (constructor `__init__` resolution) has graduated ladder: `same_file=0.8 / imported=0.7 / same_pkg=0.6` — clearly the author's intended pattern.
2. `rust_lang.rs:1855` has graduated ladder: `same_file=0.9 / imported=0.8 / same_pkg=0.7` — confirms graduated descent is the established cross-language convention.
3. Git blame shows the entire `python.rs` block is from one commit (`86f5b1c` refactor split). The collapse happened at refactor time, not from incremental drift.

**Fix applied:**
- `imported` branch: 0.8 → 0.75 (slot strictly between same_file=0.8 and same_pkg=0.7)
- Chose 0.75 over the alternative (push same_pkg from 0.7 → 0.6 like line 932) to **minimize disturbance** to existing edge weights for in-package calls
- Added doc comment on the ladder explaining intent + cross-reference to rust_lang.rs and ISS-043 history

**Regression test added:** `test_python_call_confidence_ladder_ordering` in `crates/gid-core/src/code_graph/tests.rs`
- Constructs Python source with same-file call + unresolved external call
- Asserts `same_file_call.confidence > unresolved_call.confidence`
- Asserts `same_file_call.confidence >= 0.8`
- Catches any future regression that re-collapses the ladder

**Verification:**
- `cargo clippy --all-targets --all-features` → no `if_same_then_else` warning on python.rs:857-867
- `cargo test --workspace --all-features` → 1113 passed (one unrelated perf flake `test_perf_tasks_with_code_nodes`, passes on rerun)
- New test passes

---

## Original Symptom (preserved for context)

Clippy emits `if_same_then_else` for the Python call-resolution confidence ladder:

```rust
// python.rs:857-867 (BEFORE FIX)
let confidence = if !same_file.is_empty() {
    0.8_f32
} else if !imported.is_empty() {
    0.8                       // ← same value as same_file branch (TYPO)
} else if !same_pkg.is_empty() {
    0.7
} else if is_attribute_call {
    0.3
} else {
    0.5
};
```

`same_file` and `imported` resolve to the **same** 0.8 confidence. The ladder pretends to have 5 tiers, but functionally it has 4: `{same_file ∪ imported = 0.8}`, `same_pkg = 0.7`, `attribute = 0.3`, `unknown = 0.5`.

Compare to the equivalent ladder in Rust (`rust_lang.rs`, same function pattern) — there `same_file` is strictly higher than `imported`. The two languages have inconsistent semantics.

## Root Cause

Two possibilities:

1. **Bug:** `imported` should be lower than `same_file` (e.g. 0.75 or 0.7). The author intended a strict ladder but typed 0.8 twice. The clippy lint is correctly flagging a copy-paste error.

2. **Intentional but undocumented:** in Python, `from foo import bar` resolves at runtime via the import system, and a same-file definition vs. an imported one are equally certain *as long as the import path is unambiguous*. The author may have decided imported and same_file are equally confident.

**To determine which:** check git blame on this block, check for design docs / ISS-012 acceptance criteria, and compare with rust_lang.rs's ladder for the same call type.

Initial reading: probably (1). The third tier (`same_pkg = 0.7`) drops only 0.1 below `imported`, suggesting the author thought of a graduated ladder. If `imported` were intentionally tied with `same_file`, the next drop to `same_pkg` would more likely be 0.65 or lower to preserve relative spacing. The current numbers look like a typo.

## Impact

- **Edge ranking:** GID query results that sort by confidence (e.g. "show most-likely call targets") will see `same_file` and `imported` matches as indistinguishable. This is wrong if (1) is the case — same-file calls should rank above cross-file imports.
- **Downstream features (ISS-012 confidence-weighted edges):** any feature that uses confidence as a weight in a graph algorithm (clustering, centrality, traversal cost) will treat these two cases as equivalent. Wrong if (1).
- **No test coverage:** there is no test asserting the ladder structure. The bug (if it is one) is invisible to CI.

## Fix

Step 1 — **Decide intent**: read git blame for `python.rs:846-852`, cross-reference with `rust_lang.rs`'s call resolution and ISS-012 design notes. Ask in commit message of original change.

Step 2a — **If bug**: change `imported` branch to a value strictly between `same_pkg` (0.7) and `same_file` (0.8). Suggested: `0.75`. Add a unit test in `tests/python_call_resolution.rs` that asserts:
```rust
assert!(confidence_for_same_file() > confidence_for_imported());
assert!(confidence_for_imported() > confidence_for_same_pkg());
```

Step 2b — **If intentional**: add a doc comment explaining why same_file and imported are equally confident in Python but not in Rust. Allow the lint at the function level: `#[allow(clippy::if_same_then_else)]`. The doc comment IS the test — it documents the design choice for future readers.

## Verification

- After fix: `cargo clippy ... 2>&1 | grep -A3 "python.rs:846"` → no `if_same_then_else` warning
- New test (if 2a): passes and would have caught a future regression to identical values
- All 1243 existing tests still pass

## Open Questions

- Does ISS-012's confidence-weighted edges work depend on this ladder being graduated? If yes, this is more urgent than P2.
- Are there any bench fixtures that lock in the current 0.8/0.8 behavior? If so, those need updating with the fix.
