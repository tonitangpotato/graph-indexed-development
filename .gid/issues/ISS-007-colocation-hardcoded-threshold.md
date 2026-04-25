# ISS-007: Hardcoded co-location threshold skips large directories

**Status:** closed (2026-04-25 — superseded by isolation-gated co-location)
**Severity**: Important  
**Component**: `crates/gid-core/src/infer/clustering.rs`  
**Reported**: 2026-04-10  
**Note**: Verified 2026-04-25 — hardcoded `MAX_DIR_SIZE_FOR_COLOCATION = 50` is gone. Replaced by `add_dir_colocation_edges` (clustering.rs:531) which only fires for files that are otherwise isolated (no other clustering signal). The threshold-skip problem described below no longer applies because:
- Large directories with sub-structure get clustering signal from co-citation, symbol-similarity, and import edges (no co-location needed).
- Truly isolated files in large flat directories still get co-location applied.
- The "skip all >50 files" cliff is gone.

## Problem

`MAX_DIR_SIZE_FOR_COLOCATION = 50` is a hardcoded constant. Directories with >50 files are entirely skipped for co-location edge injection.

For large projects (e.g., 1900 files), common directories like `utils/` with 298 files get zero co-location signal, breaking clustering quality for those files.

### Why this is wrong

Co-location is a real signal — files in the same directory ARE related. Skipping it entirely for large dirs throws away valuable information. The threshold should not be a fixed number.

### Location

- `clustering.rs` line 31: `pub const MAX_DIR_SIZE_FOR_COLOCATION: usize = 50;`
- `clustering.rs` line 275: `if files.len() > MAX_DIR_SIZE_FOR_COLOCATION { continue; }`

## Fix

Replace the skip-all strategy with hierarchical sub-directory grouping:

1. For directories with >N files, check if they have sub-directories
2. If yes: add co-location edges within each sub-directory group (recursive)
3. If no sub-dirs (flat dir with many files): add co-location edges with reduced weight (e.g., `base_weight / log2(file_count)`) to avoid over-coupling
4. Remove the hardcoded constant entirely — the weight decay handles scaling naturally
