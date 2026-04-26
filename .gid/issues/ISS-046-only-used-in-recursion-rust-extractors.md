# ISS-046: Recursive extractors carry unused parameters that should be hoisted to caller-shared state

**Status:** open
**Priority:** P3 (smell — wastes argument slots, masks too_many_arguments lint)
**Component:** `crates/gid-core/src/code_graph/lang/rust_lang.rs` (`extract_rust_node` line 103, `extract_calls_from_token_tree` line 1534)
**Filed:** 2026-04-26
**Discovered by:** RustClaw (clippy `only_used_in_recursion`)
**Related:** ISS-042 (parent cleanup), ISS-044 (similar pattern in python.rs `regex_creation_in_loops`)

---

## Symptom

Clippy emits `only_used_in_recursion` for two parameters in `rust_lang.rs`:

```
warning: parameter is only used in recursion
   --> crates/gid-core/src/code_graph/lang/rust_lang.rs:103:5
    |
103 |     impl_target_map: &mut HashMap<String, String>,
    |     ^^^^^^^^^^^^^^^

warning: parameter is only used in recursion
    --> crates/gid-core/src/code_graph/lang/rust_lang.rs:1534:5
     |
1534 |     struct_field_types: &HashMap<String, HashMap<String, String>>,
```

Both parameters are passed down through recursive calls but **never read** in the recursing function itself — only forwarded to the next recursion level. They're dead weight at this layer.

## Root Cause

The functions `extract_rust_node` and `extract_calls_from_token_tree` are both deeply recursive AST walkers. They accept parameters that are only consumed by deeper levels of the recursion (probably leaf cases that resolve impl targets / struct field types).

The current design carries this state through the entire call stack as explicit arguments, which:
1. Inflates parameter count (both functions are also flagged by `too_many_arguments` — 12/7 and 13/7)
2. Pollutes the function signature for readers who have to ask "why does this layer need impl_target_map?"
3. Indicates the recursion structure is leaking state-management concerns into the API

## Impact

- **Functional:** none. Behavior is correct.
- **Maintainability:** real. These two functions are already at the top of the `too_many_arguments` list (ISS-042 Phase 2 item #1). The unused-in-recursion parameters are a *symptom* of the broader "too many flat parameters" problem.

## Fix

This issue is **best resolved together with ISS-042's Phase 2 item #1** (`too_many_arguments` cleanup), because both have the same root cause: shared state being passed flat instead of grouped.

### Option A: ExtractCtx struct (preferred — solves both lints at once)

Introduce an `ExtractCtx<'a>` (or similar) holding the shared state:

```rust
pub(crate) struct RustExtractCtx<'a> {
    pub source: &'a [u8],
    pub source_str: &'a str,
    pub rel_path: &'a str,
    pub file_id: &'a str,
    pub module_prefix: &'a str,
    pub nodes: &'a mut Vec<CodeNode>,
    pub edges: &'a mut Vec<CodeEdge>,
    pub impl_target_map: &'a mut HashMap<String, String>,
    pub struct_field_types: &'a HashMap<String, HashMap<String, String>>,
    // ... whatever else is currently passed flat
}

pub(crate) fn extract_rust_node(node: tree_sitter::Node, ctx: &mut RustExtractCtx<'_>) {
    // ...
    extract_rust_node(child, ctx);  // recursion just passes ctx
}
```

Result:
- `too_many_arguments` warnings disappear (function takes 2 args instead of 12)
- `only_used_in_recursion` warnings disappear (the field is on the ctx, not in the parameter list — clippy doesn't flag struct fields that are forwarded)
- The "this layer doesn't use impl_target_map" question is silently answered: it's just on the ctx, of course it's available

### Option B: Underscore prefix (silences but doesn't fix)

Rename the parameters `_impl_target_map` and `_struct_field_types`. Clippy stops warning, but the underlying smell (12+ flat parameters) remains. **Not recommended** — fixes the lint, not the design.

### Option C: Thread-local / static cell (over-engineered)

Don't.

## Verification

- After Option A applied: `cargo clippy --all-targets --all-features 2>&1 | grep "only_used_in_recursion"` → empty
- Same run: `grep "too_many_arguments" | grep rust_lang` → reduced from current ~6 to ~0
- All 1243 existing tests pass
- Recursion depth and behavior unchanged (this is a pure signature refactor)

## Sibling Audit

While doing this, do the same for `python.rs`'s extract_* family — they have the **same** structural problem (5 functions flagged `too_many_arguments`, plus the import-loop regex bug from ISS-044). A `PyExtractCtx<'a>` mirror of the Rust one will clean both.

## Note on Sequencing

This issue has the smallest standalone scope but the largest "natural fit" with ISS-042 Phase 2. Recommendation: do **not** schedule ISS-046 alone — bundle it into the same PR as ISS-042 Phase 2 item #1, where it gets resolved as a side effect.
