---
id: "ISS-045"
title: "`infer/mod.rs` re-exports and uses its own deprecated constant `COLOCATION_PAIRWISE_LIMIT`"
status: closed
priority: P2
created: 2026-04-26
closed: 2026-04-26
component: "crates/gid-core/src/infer/mod.rs (line 39, re-export); crates/gid-core/src/infer/clustering.rs (definition + deprecation attribute)"
related: ["ISS-042", "ISS-013"]
---
# ISS-045: `infer/mod.rs` re-exports and uses its own deprecated constant `COLOCATION_PAIRWISE_LIMIT`

**Status:** closed (2026-04-26)
**Priority:** P2 (API hygiene — internal use of a constant we've publicly deprecated)
**Resolution:** Migration was complete (no internal `(c)` uses, no external uses in any tracked project). Hard-removed the constant from `clustering.rs` and the re-export from `infer/mod.rs`. Clippy now clean of the deprecation warning. All 1122 lib tests pass with --all-features.
**Component:** `crates/gid-core/src/infer/mod.rs` (line 39, re-export); `crates/gid-core/src/infer/clustering.rs` (definition + deprecation attribute)
**Filed:** 2026-04-26
**Discovered by:** RustClaw (clippy `deprecated`)
**Related:** ISS-042 (parent cleanup), ISS-013 (feature-infer-llm-integration)

---

## Symptom

Clippy emits:

```
warning: use of deprecated constant `infer::clustering::COLOCATION_PAIRWISE_LIMIT`:
         co-location is now isolation-gated; pairwise limit is unnecessary
  --> crates/gid-core/src/infer/mod.rs:39:5
   |
39 |     COLOCATION_PAIRWISE_LIMIT, CO_CITATION_MIN_SHARED,
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^
```

We deprecated `COLOCATION_PAIRWISE_LIMIT` (with a clear reason: "co-location is now isolation-gated; pairwise limit is unnecessary"), but we **still re-export it** from `infer::mod` and presumably still reference it somewhere else in the crate or its public API surface.

This is the worst kind of deprecation: we tell external users "don't use this", then internally use it. It signals one of three problems:
1. We forgot to clean up after the migration to isolation-gating
2. The migration is incomplete and the constant is still load-bearing somewhere
3. The deprecation was premature

## Root Cause

When the infer module was migrated from pairwise-limit-based co-location to isolation-gated co-location, the constant was deprecated to warn external users, but the internal cleanup was skipped — likely because removing the re-export would have broken downstream tests or examples that hadn't been updated yet.

The deprecation has been in place long enough that clippy is now noisy about it on every build. The warning has been ignored, which means any *new* uses of the deprecated constant by mistake are also invisible.

## Impact

- **API surface:** external users importing `gid_core::infer::COLOCATION_PAIRWISE_LIMIT` still get the constant (it's re-exported), but their compile shows a deprecation warning. They have no migration path documented in the crate.
- **Internal correctness:** if the constant is still **used** somewhere (not just re-exported), the migration to isolation-gating is incomplete and the old pairwise-limit logic is still active in some code path.
- **CI noise:** every clippy run shows this; future deprecations become invisible.

## Investigation Plan

Step 1 — **Find all internal references:**

```bash
cd crates/gid-core
grep -rn "COLOCATION_PAIRWISE_LIMIT" src/ tests/ benches/ examples/
```

Categorize each hit:
- (a) The definition site in `clustering.rs` (with `#[deprecated]` attribute)
- (b) The re-export in `infer/mod.rs:39`
- (c) Actual *uses* (read the value, pass it to a function, compare against it)

If (c) is non-empty, the migration is incomplete — fix that first.

Step 2 — **Find external references** (downstream crates, gid-cli, rustclaw, examples):

```bash
cd /Users/potato/clawd/projects
grep -rn "COLOCATION_PAIRWISE_LIMIT" gid-rs/ rustclaw/ engram/ 2>/dev/null
```

If anything outside `gid-core/src/infer/` uses it, those need migration to the new isolation-gated API.

## Fix

### If migration is complete (no internal `(c)` uses)

1. Remove the constant from the `pub use ...;` re-export in `infer/mod.rs:39`. External users get a hard error instead of a warning, with a one-cycle deprecation message in the changelog.
2. Either:
   - **Hard-remove** the constant from `clustering.rs` if no version-pinned downstream needs it (preferred — we own all consumers)
   - Or **keep the definition** as `#[deprecated]` private to the module for one more version, then remove it next release

### If migration is incomplete (internal `(c)` uses exist)

1. **Do not** silence the warning. The warning is correct.
2. File a follow-up task to actually finish the isolation-gating migration: replace each remaining `COLOCATION_PAIRWISE_LIMIT` use with the isolation-gated equivalent.
3. Once internal uses are 0, return to the "migration complete" path above.

## Verification

- After fix: `cargo clippy --all-targets --all-features 2>&1 | grep COLOCATION_PAIRWISE_LIMIT` → empty
- `cargo test --workspace --all-features` → 1243 passed
- Public API change documented in CHANGELOG (if removing the re-export)

## Why This Matters

We frequently use `#[deprecated]` to communicate API direction to downstream users. If we don't keep our own house clean, the signal is meaningless — every clippy run becomes a wall of "we know, we know" deprecation warnings, which trains everyone (us included) to ignore them. The next *real* deprecation problem will be invisible.
