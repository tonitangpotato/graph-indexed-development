# ISS-041: `cargo clippy` fails on default settings — `approx_constant` errors

**Status:** closed
**Resolution:** fixed
**Priority:** P1 (CI hygiene)
**Component:** test fixtures (`infer/integration.rs`, `storage/sqlite.rs`)
**Filed:** 2026-04-26
**Closed:** 2026-04-26
**Discovered by:** RustClaw (proactive scan)
**Related:** ISS-040 (same scan run; together they account for all 5 of clippy's errors)

---

## Symptom

`cargo clippy --all-targets --all-features` produces 3 hard **errors** (not warnings) for `clippy::approx_constant`:

```
error: approximate value of `f{32, 64}::consts::PI` found
   --> crates/gid-core/src/infer/integration.rs:629:29
    |
629 |                 codelength: 3.14,
```

Same pattern at `storage/sqlite.rs:1869` and `:1880`.

After ISS-040, these 3 errors are the only remaining clippy errors. Resolving them lets `cargo clippy` exit 0, unblocking any future `-D warnings` or pre-commit hook.

## Root Cause

The literal `3.14` is being used as **arbitrary test data** — a synthetic float for fixture purposes (`codelength` in a `ClusterMetrics`, "float" value in a metadata map). It has nothing to do with π. Clippy can't tell intent from `3.14` and assumes the author meant the constant.

Two ways to satisfy clippy:
1. Use `std::f64::consts::PI` if π is intended → not the case here
2. Use a different non-PI-approximating literal → correct fix

## Fix

Replace `3.14` with `3.5` everywhere in fixtures + update the corresponding string-contains assertion (`summary.contains("3.140")` → `summary.contains("3.500")`).

`3.5` is chosen because:
- It is not an approximation of any well-known constant
- It satisfies `> 3.0` in `test_format_json_schema`
- It formats as `3.500` so the existing precision-3 string assertion form is preserved (just with a different digit)

## Verification

- `cargo clippy --all-targets --all-features 2>&1 | grep "^error"` → **empty** (was 3 lines)
- `cargo test --workspace --all-features` → 1243 passed (unchanged)
- After this commit + ISS-040, `cargo clippy` returns exit 0 with only warnings remaining (179 warnings — see ISS-042)
