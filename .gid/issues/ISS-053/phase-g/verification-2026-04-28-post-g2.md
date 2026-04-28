# Phase G — Default Layout corpus verification (POST-G2)

**Date**: 2026-04-28 (after G2 fix)
**Corpora**: engram, gid-rs, rustclaw `.gid/` directories
**Tool**: `gid artifact list --project <name> --json` (built from `gid-dev-cli` with patched gid-core)
**gid-core change**: `crates/gid-core/src/artifact/layout.rs::default_patterns()` extended with 7 new patterns covering Gaps 1–4 from the pre-G2 report. Diff: ~80 LOC additions (patterns + comments + 14 regression tests).

## Result: ✅ ROLLOUT UNBLOCKED

§7 "Migration verification (Phase 0 — blocks rollout)" criteria are met for all three corpora. **0 miscategorized notes** anywhere; all residual `note`-kind artifacts are §4.4 acknowledged fallbacks (top-level orphan files / non-canonical design subdirs).

## Numbers

| Corpus  | Total | note (now) | note (before G2) | Δ notes | Miscategorized | Acknowledged-fallback notes |
|---------|------:|-----------:|-----------------:|--------:|---------------:|----------------------------:|
| engram  |   146 |          1 |               17 |    −16  |              0 |                           1 |
| gid-rs  |   118 |          3 |                4 |     −1  |              0 |                           3 |
| rustclaw|    87 |          2 |               10 |     −8  |              0 |                           2 |
| **Total**| **351** | **6** | **31** | **−25** | **0** | **6** |

(Pre-G2 report had Total=356; the −5 diff vs now is unrelated churn in `.gid/` dirs between the two snapshots — files added/removed manually by potato. It is not a layout-behavior change.)

## Residual notes (all §4.4-acknowledged, NOT blocking)

### engram (1)
- `.gid/archive/iss-027-ritual-bug-evidence/README.md` — archived investigation dir, intentionally outside the canonical layout

### gid-rs (3)
- `.gid/README.md` — top-level orphan readme
- `.gid/incremental-extract/DESIGN.md` — internal design subdir at `.gid/<topic>/DESIGN.md` (project-specific; §4.4 says these are accepted as note)
- `.gid/sqlite-migration/design-storage.md` — same shape as above

### rustclaw (2)
- `.gid/ISS-051-graph-update.md` — top-level orphan issue stub (legacy)
- `.gid/_misplaced/2026-04-20-engram-kc-source-refs-bug.md` — explicitly-named misplaced file

## What changed in default Layout

7 new patterns added to `default_patterns()` (in priority order, each before the global noop fallback):

1. `issues/{parent_id}/{slug}/{any}.md` → `issue-doc-nested` *(Gap 4)*
2. `features/{name}.md` → `feature-doc-toplevel` *(Gap 2)*
3. `features/{parent_id}/{slug}/requirements.md` → `nested-feature-requirements` *(Gap 1)*
4. `features/{parent_id}/{slug}/design.md` → `nested-feature-design` *(Gap 1)*
5. `features/{parent_id}/{slug}/reviews/{name}.md` → `nested-feature-review` *(Gap 1)*
6. `features/{parent_id}/{slug}/{any}.md` → `nested-feature-doc` *(Gap 1, fallback within nesting)*
7. `docs/reviews/{name}.md` → `doc-review` *(Gap 3, must precede pattern 8)*
8. `docs/{slug}/{name}.md` → `doc-nested` *(Gap 3)*

**Slot reuse rationale**: The token parser only recognises a fixed alphabet of placeholders (`slug`, `name`, `parent_id`, `any`, `seq`, `id`). For nested-feature patterns the outer feature directory is captured as `parent_id` (semantically the nested sub-feature's parent) and the inner sub-feature directory as `slug`. The `SlotMap` is `BTreeMap<String,String>` so a single `slug` key holds the inner slug — this is the round-trip semantic we want (you `resolve(kind="nested-feature-design", slots={parent_id, slug})` and get back the original 4-segment path).

**Ordering rationale**: `pattern_match` requires equal segment counts between pattern and path (`pat_segs.len() == path_segs.len()`) and stops at the first hit. New patterns that share a segment count with existing patterns were placed so the more-specific pattern wins:

- `docs/reviews/{name}.md` before `docs/{slug}/{name}.md` (literal `reviews` would otherwise be captured as `{slug}`)
- `features/{slug}/{any}.md` (3-seg, `feature-doc`) and `features/{name}.md` (2-seg, `feature-doc-toplevel`) cannot collide due to differing segment counts

## Test coverage

`crates/gid-core/src/artifact/layout.rs` test module gained 14 ISS-053 regression tests, each anchored to a real corpus path:

- `iss053_gap1_*` (4 tests) — engram knowledge-compiler nested feature, including a round-trip
- `iss053_gap2_*` (3 tests) — rustclaw `features/DESIGN-*.md`, including round-trip and slug-dir-precedence
- `iss053_gap3_*` (4 tests) — `docs/reviews/`, `docs/discussion/`, ordering, flat-`docs/` non-regression
- `iss053_gap4_*` (2 tests) — engram `issues/ISS-024/wip/README.md` and review-pattern non-regression
- `iss053_g2_unknown_shape_still_falls_back_to_note` (1 test) — sanity check on the noop + true fallback paths

`cargo test -p gid-core --lib artifact` → **123 passed; 0 failed**.

## Artifact kind table change

The default kind table grows by 7 rows. All new kinds use the `nested-`/`-toplevel`/`-nested` suffix to avoid colliding with existing kind names — `pattern_for_kind` returns the first match, so unique kinds are essential for the resolve direction to be deterministic.

| New kind | Source pattern | Round-trip safe |
|---|---|---|
| `issue-doc-nested` | `issues/{parent_id}/{slug}/{any}.md` | yes |
| `feature-doc-toplevel` | `features/{name}.md` | yes |
| `nested-feature-requirements` | `features/{parent_id}/{slug}/requirements.md` | yes |
| `nested-feature-design` | `features/{parent_id}/{slug}/design.md` | yes |
| `nested-feature-review` | `features/{parent_id}/{slug}/reviews/{name}.md` | yes |
| `nested-feature-doc` | `features/{parent_id}/{slug}/{any}.md` | yes |
| `doc-review` | `docs/reviews/{name}.md` | yes |
| `doc-nested` | `docs/{slug}/{name}.md` | yes |

## Acceptance gate (ISS-053 §7) — verdict

| Check | Status |
|---|---|
| All 3 corpora scanned | ✅ |
| 0 miscategorized notes in any corpus | ✅ |
| Residual notes all in §4.4-acknowledged shapes | ✅ |
| Regression tests against corpus paths | ✅ (14 new tests) |
| All artifact-related lib tests pass | ✅ (123/123) |
| No file moves/renames | ✅ (Layout-only fix) |

**§6 Acceptance criteria for Phase G are now met.** Proceed to Phase H (documentation).
