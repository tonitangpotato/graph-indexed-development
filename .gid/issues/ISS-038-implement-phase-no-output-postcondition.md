# ISS-038 — Ritual `implement` phase reports success despite zero file changes

**Status:** closed
**Type:** bug / root fix
**Severity:** high (false success masks broken rituals; downstream verify trivially passes)
**Discovered:** 2026-04-25 — RustClaw ritual `r-af714a` produced 0 file diffs after burning 19 026 tokens in `Implementing` + 3 963 in `Reviewing`, finished `Done` with `failed_phase = None`.
**Closed:** 2026-04-25
**Cross-repo:** rustclaw ISS-025 (problem statement, reproducer, hypotheses)
**Related:** ISS-029 (work-unit binding / `target_root`), ISS-016 (LSP dangling edges — adjacent surface), rustclaw ISS-016 (main-agent ritual awareness — should also catch false success)

## Resolution

Filesystem snapshot before/after the LLM invocation in any phase that requires file changes (currently `implement`). Diff is the authoritative artifact list; the LLM-reported list is ignored. Empty diff on `implement` → `SkillFailed` with diagnostic message naming tokens consumed and tool calls made, so the failure mode is debuggable from logs alone.

**Tests added (10, all passing):**
- 7 `file_snapshot` unit tests: added/modified/deleted detection, empty-diff identity, large-file head+tail+size fallback, gidignore filtering, cross-platform path handling.
- 3 `v2_executor` end-to-end tests:
  - `implement_phase_with_zero_changes_emits_skill_failed`
  - `implement_phase_with_file_writes_emits_skill_completed_with_artifacts`
  - `non_implement_phase_does_not_enforce_changes`

**Verification:** 187/187 ritual tests pass. No regressions. (One pre-existing unrelated failure in `storage::tests::test_load_graph_auto_empty_dir_returns_default` — sqlite feature-gate issue already on origin/main.)

**Commit:** `52a84ee fix(ritual): enforce implement-phase post-condition via fs snapshot`

---

## Summary

The `implement` phase of the V2 ritual could complete with `SkillCompleted { artifacts: [] }` when the LLM produced only commentary and made zero `Write`/`Edit` tool calls. The downstream `verify` phase then ran against an unchanged tree and trivially passed, so the state machine reached `Done` with `failed_phase = None` — i.e. **false success**.

Concrete failure mode (rustclaw `r-af714a`):
- 19 026 tokens spent in `Implementing`.
- 3 963 tokens spent in `Reviewing`.
- 0 files modified anywhere under either project root.
- State machine reported `Done`, no error context, nothing to retry.

False success is more damaging than failure: a failed phase tells the operator to investigate; a successful phase with no diff tells them they're done.

---

## Root cause

`crates/gid-core/src/ritual/api_llm_client.rs` returned `SkillResult { artifacts_created: vec![], ... }` regardless of whether the underlying LLM made `Write`/`Edit` tool calls. The intent was that the **executor layer** would own artifact accounting (the comment mentions deferring this) — but the executor never did. The contract was half-implemented:

- `api_llm_client` correctly punted artifact tracking to the executor.
- `v2_executor` never picked it up. It just propagated whatever the skill returned.
- Net effect: artifacts were always `[]`, no post-condition checked the filesystem, the phase always "succeeded".

This is a **specification / contract bug**, not a missing-feature bug. The data flow existed end-to-end; nothing was checking it against ground truth.

---

## Fix (root, not patch)

**File-system diff is ground truth.** Wrap `run_skill` in the executor with a snapshot before and after, for any phase declared `phase_requires_file_changes`. The diff replaces the LLM-reported artifact list as the path-of-record. Empty diff on a phase that requires changes → `SkillFailed`.

### Changes

- **New module `crates/gid-core/src/ritual/file_snapshot.rs` (390 lines):**
  - `snapshot_dir(root, ignore) -> Snapshot` — walks the directory respecting `.gidignore`, computes per-file fingerprint.
  - Fingerprint = full sha256 for files ≤ 1 MiB; 64-byte head + 64-byte tail + size for larger files (cheap, robust against real edits, only adversarial same-size middle-byte edits slip through).
  - `diff_snapshots(before, after) -> FsDiff { added, modified, deleted }`.
  - Cross-platform path handling (Windows `\\?\` prefix stripping).

- **`v2_executor` (281 lines added):**
  - `phase_requires_file_changes(phase) -> bool` — currently `phase == "implement"`.
  - `resolve_mutation_root(state, config) -> PathBuf` — prefers `state.target_root` over `config.project_root`. This is the ISS-029 work-unit binding: post-conditions evaluated against where the code lives, not where the ritual was invoked. Side-effect: addresses cross-workspace correctness silently.
  - `run_skill_with_postcondition` wraps `run_skill`:
    - Snapshot before invoking LLM.
    - Snapshot after.
    - If `phase_requires_file_changes` and diff is empty → emit `SkillFailed { reason: "implement phase produced no file changes (tokens=N, tool_calls=M)" }`.
    - If diff non-empty → emit `SkillCompleted { artifacts: <diff paths> }`. **The LLM-reported artifact list is now explicitly ignored.**
  - Cross-workspace warning: `target_root != project_root` → one-line warn log so the discrepancy surfaces.

- **`api_llm_client.rs`:** kept `artifacts_created: vec![]`, added a comment documenting the contract: empty vec is intentional, the executor owns artifact accounting via fs-diff. Removing the field would have been more invasive (it's part of the public `SkillResult` shape) and pointless once the executor ignores it.

---

## Design choices

1. **fs-diff over LLM self-reporting.** The LLM's claim is one signal; the filesystem is ground truth. We always trust ground truth.

2. **Hash with large-file fallback.** Full sha256 on every snapshot would be wasteful for binary blobs (model weights, generated assets, compressed datasets). 64-byte head + tail + size catches every real edit and is O(1) regardless of file size. Adversarial same-size middle-byte edits slip through, but ritual targets aren't adversarial.

3. **Reuse `.gidignore`, not a separate ignore list.** Diverging ignore rules between graph extraction and ritual snapshot would cause silent mismatches (e.g. snapshot detects a build artifact change and fails the phase, when the graph would correctly ignore it). One source of truth.

4. **Per-phase opt-in via `phase_requires_file_changes`.** Some phases (triage, planning, graph-only) legitimately produce no file changes. Hard-coding "implement requires changes" is correct for now. If `verify` later wants self-correcting edits, we add it to the predicate. Don't generalize prematurely.

5. **`target_root` preference over `project_root`.** ISS-029 work-unit binding made `target_root` distinct from `project_root`. Post-conditions must be evaluated against the actual code root. This also incidentally fixes the cross-workspace case where the ritual was invoked from project A but the target code lives in project B — the snapshot now follows the work-unit, not the invocation site.

6. **`SkillFailed` not `SkillError`.** The phase itself didn't error — the LLM ran to completion. The skill *result* failed a post-condition. Emitting `SkillFailed` keeps the distinction and lets the state machine apply normal retry/escalation policy. Also: the diagnostic message embeds `tokens=N, tool_calls=M` so an operator can tell from logs alone whether the LLM was confused (zero tool calls), got stuck (high tokens, low tool calls), or hit a refusal pattern.

---

## Follow-up (deferred)

- **F1: Mid-stream enforcement.** Detect that the LLM hasn't called `Write`/`Edit` after N turns and inject a corrective system message before burning the full budget. Cheaper than post-hoc detection. Tradeoff: more invasive, harder to reason about. Probably its own issue.
- **F2: `Δfiles=N` in completion notifications.** Even with the post-condition fix, the operator skimming a Telegram completion message can't tell at a glance whether the diff was substantial vs. one-line. Surface it. Ties into rustclaw ISS-016 (main-agent ritual awareness).
- **F3: Doc-only / investigation rituals.** A ritual whose work is "write a design doc" produces zero code changes and would fail the current post-condition. Either tag the work-unit `kind: doc` and skip enforcement, or route to a different phase predicate. Low priority — current `implement` phase is consistently code-producing.
- **F4: Sandbox/worktree merge protocol.** Not needed for this fix — the no-op rituals weren't running sandboxed, they were running directly and just not writing. But if execution ever moves to sandboxes, the snapshot must be taken at the merge point, not inside the sandbox.
- **F5: Cross-workspace warning escalation.** Currently a log line. Could become a structured field on `RitualState` so the dashboard/notification layer can surface it without log scraping.

---

## References

- rustclaw ISS-025 — full problem statement, reproducer (`r-af714a`), and hypothesis matrix.
- ISS-029 — work-unit binding (`target_root` vs `project_root`).
- Commit `52a84ee` — the fix.
- Commit `fe36267` — `RitualV2Status` terminal disposition field (orthogonal but landed adjacent; addresses the `status: null` symptom in rustclaw ISS-019).
