---
id: "ISS-008"
title: "max_cluster_size auto-formula too lenient for large projects"
status: closed
priority: P2
created: 2026-04-26
---
# ISS-008: max_cluster_size auto-formula too lenient for large projects

**Status:** closed
**Severity**: Design  
**Discovered**: 2026-04-10, during Claude Code validation (1902 files → 102 components)  
**Fixed**: 2026-04-10  

## Problem

The `max_cluster_size` auto-computation formula `total_files / 5` is linear — it grows proportionally with project size. For large projects, this produces thresholds far above reasonable component size:

| Project Size | Old Formula (N/5) | Actual Large Clusters |
|---|---|---|
| 100 files | 20 | OK |
| 500 files | 100 | too high |
| 1902 files | 380 | commands-93 (113 files), utils-30 (104 files) — both under threshold |
| 5000 files | 1000 | absurd |

Reasonable component size is 5-30 files regardless of project scale.

## Root Cause

Linear formula. Component size should not grow linearly with project size — a 2000-file project doesn't have bigger components, it has *more* components.

## Discussion: sqrt vs log

Two candidates were evaluated:

### Option A: sqrt-based
```rust
let max_size = ((total_files as f64).sqrt().ceil() as usize) * 2;
let max_size = max_size.clamp(20, 100);
```
| 50 | 100 | 500 | 1000 | 2000 | 5000 |
|---|---|---|---|---|---|
| 20 | 20 | 44 | 63 | 89 | 100(clamped) |

### Option B: log-based (chosen)
```rust
let auto = ((total_files as f64).ln() * 6.0).ceil() as usize;
auto.clamp(15, 60)
```
| 50 | 100 | 500 | 1000 | 2000 | 5000 |
|---|---|---|---|---|---|
| 24 | 28 | 38 | 42 | 46 | 52 |

### Decision: log (Option B)

**Rationale**: Component size should be *nearly constant* regardless of project scale. This is a cognitive limit — humans can reason about modules of 5-30 files. Large projects have more modules, not bigger modules.

- **sqrt still grows too fast**: 2000 files → 89-file components is still too big. It only delays the problem from 2000 to 5000 files.
- **log is nearly flat**: the difference between 100 files (→28) and 5000 files (→52) is small, which matches reality.
- **Hard cap at 60**: no scenario where a 60+ file component is a single coherent module.
- **Floor at 15**: for tiny projects, don't over-split.

Parameters: `ln(N) * 6.0`, clamped to `[15, 60]`. The multiplier 6.0 was tuned to hit ~28 at 100 files and ~46 at 2000 files.

## Fix

**File**: `crates/gid-core/src/infer/clustering.rs`

**Before** (line ~1460):
```rust
let max_size = config
    .max_cluster_size
    .unwrap_or_else(|| total_files.max(100) / 5);
let max_size = max_size.max(20);
```

**After**:
```rust
let max_size = config.max_cluster_size.unwrap_or_else(|| {
    let auto = ((total_files as f64).ln() * 6.0).ceil() as usize;
    auto.clamp(15, 60)
});
```

## Verification

- `cargo check -p gid-core` — clean ✅
- `cargo test -p gid-core --lib` — 489 pass, 1 pre-existing failure (unrelated storage test) ✅
- All existing split tests use `max_cluster_size: Some(10)` (explicit override), so they are unaffected by the auto-formula change
- Full validation on Claude Code (1902 files) should now split the 113-file and 104-file clusters (both > threshold of 46)

## Impact

- Only affects projects where `max_cluster_size` is not explicitly set (auto-compute path)
- Explicit `--max-cluster-size N` CLI flag still overrides the formula
- Smaller projects (<100 files) see minimal change (20→24-28)
- Large projects see significantly tighter clustering, producing more but smaller components
