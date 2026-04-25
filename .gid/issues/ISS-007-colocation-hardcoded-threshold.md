# ISS-007: Hardcoded co-location threshold skips large directories

**Status:** likely-superseded
**Severity**: Important  
**Component**: `crates/gid-core/src/infer/clustering.rs`  
**Reported**: 2026-04-10  
**Note**: Original number reused — see issues-index.md ISS-007 (closed 2026-04-06, ghost nodes) for earlier issue with same ID. The hardcoded `MAX_DIR_SIZE_FOR_COLOCATION = 50` constant referenced below appears to have been replaced/removed during co-location refactor (see `WEIGHT_DIR_COLOCATION` + isolation gating). Needs verification before close.

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
