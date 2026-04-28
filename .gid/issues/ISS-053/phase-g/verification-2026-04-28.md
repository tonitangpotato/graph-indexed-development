# Phase G — Default Layout corpus verification

**Date**: 2026-04-28
**Corpora**: engram, gid-rs, rustclaw `.gid/` directories
**Tool**: `gid artifact list --project <name> --json`
**Built-in default Layout**: `crates/gid-core/src/artifact/layout.rs::default_patterns()` (12 patterns)

## Result: 🛑 ROLLOUT BLOCKED

The default Layout fails the §7 "Migration verification (Phase 0 — blocks rollout)" criteria in 2 of 3 corpora (engram, rustclaw). gid-rs passes (residual notes are acknowledged fallback paths from §4.4 of the issue).

## Numbers

| Corpus  | Total | note | Miscategorized | Acknowledged-fallback notes |
|---------|------:|-----:|---------------:|----------------------------:|
| engram  |   152 |   17 |             14 |                           3 |
| gid-rs  |   117 |    4 |              0 |                           4 |
| rustclaw|    87 |   10 |              8 |                           2 |
| **Total**| **356** | **31** | **22** | **9** |

A "miscategorized" note is one that the §7 criteria say must NOT fall through to `kind: note` (i.e. it should be `issue/feature/design/review/requirements`).

## Detailed misses by gap class

### Gap 1 — Nested feature subfolders (engram only)
Pattern needed: `features/{slug}/{subslug}/{requirements|design|reviews}.md`
Engram has a "meta-feature" knowledge-compiler with sub-features (compilation, maintenance, platform), each with its own canonical artifact tree. The existing `features/{slug}/{any}.md` pattern is at the wrong depth.

Affected (12 files):
- `.gid/features/knowledge-compiler/compilation/design.md`
- `.gid/features/knowledge-compiler/compilation/requirements.md`
- `.gid/features/knowledge-compiler/compilation/reviews/design-r1.md`
- `.gid/features/knowledge-compiler/compilation/reviews/requirements-r1.md`
- `.gid/features/knowledge-compiler/maintenance/design.md`
- `.gid/features/knowledge-compiler/maintenance/requirements.md`
- `.gid/features/knowledge-compiler/maintenance/reviews/design-r1.md`
- `.gid/features/knowledge-compiler/maintenance/reviews/requirements-r1.md`
- `.gid/features/knowledge-compiler/platform/design.md`
- `.gid/features/knowledge-compiler/platform/requirements.md`
- `.gid/features/knowledge-compiler/platform/reviews/design-r1.md`
- `.gid/features/knowledge-compiler/platform/reviews/requirements-r1.md`

### Gap 2 — Top-level `features/{name}.md` (rustclaw)
Pattern needed: `features/{name}.md` → `kind: feature-design` (or `feature-doc`)
Rustclaw stores some legacy designs at the features-root level instead of inside a slug dir.

Affected (5 files):
- `.gid/features/BATCH3-DESIGN.md`
- `.gid/features/DESIGN-autocompact.md`
- `.gid/features/DESIGN-claude-proxy.md`
- `.gid/features/DESIGN-session-resilience.md`
- `.gid/features/DESIGN.md`

The current pattern `features/{slug}/{any}.md` requires a directory between `features/` and the file — these files have nothing in between, so they fall through to the global `{slug}/{any}.md` → `note`.

### Gap 3 — `docs/{subdir}/{name}.md` (engram, rustclaw)
Pattern needed: `docs/reviews/{name}.md` → `kind: review` (or `doc-review`)
Pattern needed: `docs/{subdir}/{name}.md` → `kind: doc`
The current `docs/{name}.md` is single-segment-only and doesn't recurse.

Affected (5 files):
- engram: `.gid/docs/reviews/architecture-r1.md`, `.gid/docs/reviews/requirements-v03-r1.md`
- rustclaw: `.gid/docs/discussion/04-16.md`, `.gid/docs/discussion/session-2026-04-06-changes.md`, `.gid/docs/discussion/session-2026-04-06.md`

### Gap 4 — Issue subdirectories (engram)
Pattern needed: `issues/{parent_id}/{subdir}/{any}.md`
The current `issues/{parent_id}/{any}.md` is one segment deep; engram has `issues/ISS-024/wip/README.md` (work-in-progress dir under an issue).

Affected (1 file):
- `.gid/issues/ISS-024/wip/README.md`

## Acknowledged fallbacks (NOT blocking)

The following 9 notes are explicitly accepted by §4.4 of the issue (top-level orphan files / project-specific design subdirs):

- engram: `.gid/archive/iss-027-ritual-bug-evidence/README.md`, `.gid/issues/_audit-2026-04-28.md`, plus `.gid/_misplaced/...` (rustclaw)
- gid-rs: `.gid/README.md`, `.gid/features/STATUS.md`, `.gid/incremental-extract/DESIGN.md`, `.gid/sqlite-migration/design-storage.md`
- rustclaw: `.gid/ISS-051-graph-update.md`, `.gid/_misplaced/2026-04-20-engram-kc-source-refs-bug.md`

## Decision

**Recommended**: extend the gid-core default Layout (`default_patterns()`) before declaring ISS-053 §6 acceptance complete. Specifically add patterns for Gap 1, Gap 2, Gap 3, Gap 4 above.

**Order is additive only** — these are new patterns matched before the global `{slug}/{any}.md → note` fallback. They do not change the kind of any currently-correctly-categorized artifact (verified by inspection: every Gap pattern is strictly a deeper / more specific path than any existing default).

This is a small gid-core change (~30 lines in `default_patterns()` + a regression test against the 3-corpus snapshot).

## Followup

- Open `gid-rs ISS-053-G` (sub-issue) for the Layout extension PR, OR fold into ISS-053 as a Phase G2 amendment.
- Phase H (documentation) MAY proceed in parallel since the doc surface is unchanged — only the default kind table grows.
- Once Layout is extended, re-run this verification; expected outcome is `Miscategorized = 0` for all three corpora.

## Notes on rustclaw `features/DESIGN-*.md`

These predate the per-feature-slug convention; they are legacy. Two options:

1. **Add Layout pattern** (treat them as first-class). Cheap; no file movement.
2. **Migrate** (`mv .gid/features/DESIGN-claude-proxy.md .gid/features/claude-proxy/design.md`). Cleaner but violates ISS-053's "no files moved or renamed" constraint.

Option 1 is consistent with ISS-053's stance. Option 2 can be a follow-up cleanup, separate from the Layout fix.
