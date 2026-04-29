# Design — gid-rs#ISS-059: Drift detection in `gid_validate`

> §1 Overview, §2 Requirements coverage, §3 Components (3.1–3.7), §4 Data flow, §5 GUARDs, §6 Test plan, §7 Open questions / deferred, §8 References.

---

## §1 Overview

`gid_validate` today catches **structural** problems in the graph (cycles, orphan nodes, broken edge references). It is silent about **drift** — divergence between the three sources of truth that an issue/feature/task lives in: the on-disk artifact (e.g. `.gid/issues/ISS-NNN/issue.md`), the corresponding graph node in `graph.db`, and the central ledger that catalogs which artifacts the project knows about. ISS-059 extends `gid_validate` with a new **drift** check class that compares these three views and reports mismatches: artifacts that exist on disk but are absent from the graph, nodes that point at missing files, status fields that disagree across views, ledger entries with no backing artifact, and so on. This work is scoped to **Layer A** (artifact ↔ graph) and **Layer B** (graph ↔ ledger / config); both layers are read-only by default — drift is *reported*, never silently repaired, and a change is only applied when the operator explicitly passes `--fix-drift` (GUARD-59.1). The check depends on **gid-rs#ISS-058** (`Node.doc_path` schema column + migration + backfill CLI), which provides the canonical pointer from a graph node to its on-disk artifact — without `doc_path`, Layer A drift can only be heuristic and is therefore unreliable. ISS-058 is fully landed on `main` (commits `6a3201d` design, `3615a02` impl/migration, `0ecdad8` backfill+tests), so this design assumes `Node.doc_path` is populated and queryable. **Layer C** (commit ↔ artifact linkage — detecting unrecorded resolutions, stale closures, dangling commit refs) is intentionally **deferred to gid-rs#ISS-060**, because it requires a separate commit-indexing subsystem (`gid commits scan`) that does not yet exist; ISS-060 builds that subsystem and then wires its output back into `gid_validate --check-drift` as a third drift layer. Until ISS-060 lands, drift detection is artifact-and-graph-shaped, not commit-shaped.

---

## §2 Requirements coverage

> **Source.** No master `.gid/docs/requirements.md` exists in gid-rs (verified 2026-04-29) and no per-feature requirements doc; gid-rs uses issue bodies as the requirements source for issue-mode rituals (ISS-057, commit `18e78ab`). GOALs below are inlined from `.gid/issues/ISS-059/issue.md` Acceptance criteria + drift-class definitions. If a master requirements doc is later created, these should be lifted into it and §2 reduced to a `satisfies:` link list.

Each GOAL is numbered `GOAL-59.N` so downstream task nodes and the §6 test plan can reference them via `satisfies` edges.

### Functional GOALs

- **GOAL-59.1 — `--check-drift` opt-in flag.** `gid validate` accepts a `--check-drift` flag (off by default until stable; on by default thereafter). Tool form: `gid_validate(check_drift: bool)`. *(maps to AC#1)*

- **GOAL-59.2 — Layer A: artifact ↔ node existence (forward).** For every artifact in `.gid/issues/` and `.gid/features/`, the graph contains a node with the artifact's `id`. Missing → finding `missing-node`. *(maps to AC#2, drift-class A.1)*

- **GOAL-59.3 — Layer A: dangling doc pointer (reverse).** For every node with `doc_path` populated (from `gid-rs#ISS-058`), the file at `doc_path` exists on disk. Missing → finding `dangling-doc-pointer`. **Requires ISS-058 `Node.doc_path`** (landed on `main` `0ecdad8`). *(drift-class A.2)*

- **GOAL-59.4 — Layer A: status sync.** Artifact `frontmatter.status` and node `status` map per the canonical table (open↔todo, in_progress↔in_progress, resolved/closed/done↔done, blocked↔blocked, cancelled/wontfix↔cancelled). Divergence → finding `status-drift`. *(drift-class A.3)*

- **GOAL-59.5 — Layer A: relations sync.** Artifact frontmatter relations (`relates_to`, `depends_on`, `blocks`, `resolved_by`, `superseded_by`) match graph edges of the corresponding kind. Missing graph edge → finding `missing-edge`. *(drift-class A.4)*

- **GOAL-59.6 — Layer B: ledger consistency.** Projects declare central ledgers in `.gid/config.yml` (path + entry pattern + trigger conditions). Closed/resolved artifacts that match a ledger's trigger but have no referencing entry → finding `ledger-not-updated`. Driven entirely by config (no hard-coded ledgers). *(drift-class B, AC#3)*

- **GOAL-59.7 — Lookup precedence.** Drift checks use `Node.doc_path` (ISS-058) as the **primary** artifact↔node lookup; only fall back to ID-convention matching (`.gid/issues/<id>/issue.md`) when `doc_path` is null. Convention fallback is a reportable warning, not an error. *(AC#2 phrasing: "uses `doc_path` from ISS-058 as primary lookup, falls back to ID convention")*

- **GOAL-59.8 — Structured drift report.** Each finding carries `{category, severity, location, suggested_fix, auto_fixable: bool}`. JSON output for tool consumption; human-readable summary for CLI. *(AC#4)*

- **GOAL-59.9 — `--fix-drift` (auto-apply).** With `--fix-drift`, the validator applies fixes for `status-drift`, `missing-edge`, and `missing-node` (when artifact exists but node doesn't). All other findings remain dry-run + diff (require human authorship). Default conflict resolution for `status-drift`: artifact wins (markdown is human-edited); overrideable via `--prefer node`. *(AC#5)*

- **GOAL-59.10 — `--strict` exit code.** With `--strict`, any drift finding causes exit code ≠ 0 (for CI use). Without `--strict`, drift is reported but exit is 0. *(AC#6)*

- **GOAL-59.11 — Test fixtures per drift type.** Each drift category (A.1-A.4 + B) has at least one fixture project under `tests/fixtures/drift/` covering: detection, auto-fix correctness (where applicable), `--strict` exit code, and `--prefer` flag behavior. *(AC#7)*

- **GOAL-59.12 — Documentation.** `gid validate --help` documents the new flags; `.gid/config.yml` schema reference documents the `ledgers:` section. *(AC#8)*

### Non-functional GOALs

- **GOAL-59.13 — Layer C deferral.** Layer C (commit ↔ artifact linkage) is **explicitly out of scope** for ISS-059. The drift report MAY include a placeholder section labeled "Layer C (commit linkage) — pending ISS-060", but no commit-scanning logic ships with this design. Tracked by `gid-rs#ISS-060`. *(issue body: "Why ISS-058 first" + "Out of scope")*

- **GOAL-59.14 — Read-only by default.** No drift check writes to disk or graph unless `--fix-drift` is passed. Enforced as **GUARD-59.1** in §4. *(autopilot doc B2 acceptance criteria)*

- **GOAL-59.15 — No network, no shell-out.** Drift detection operates on local files + graph DB + config only. No git invocations, no HTTP calls. (Layer C in ISS-060 will introduce git plumbing; ISS-059 stays local.)

- **GOAL-59.16 — Skill sunset.** Once ISS-058 + ISS-059 are released, the rustclaw stop-gap skill `skills/sync-graph-on-task-complete/` is deleted (sunset condition recorded in that skill's SKILL.md). This is a tracking obligation for the release PR, not a runtime requirement.

### GOAL → drift-class mapping (cross-reference)

- **Drift-class A (artifact ↔ node):** GOAL-59.2, 59.3, 59.4, 59.5, 59.7
- **Drift-class B (graph ↔ ledger):** GOAL-59.6
- **Drift-class C (commit ↔ artifact):** *deferred — GOAL-59.13 → ISS-060*
- **CLI surface:** GOAL-59.1, 59.9, 59.10, 59.12
- **Output contract:** GOAL-59.8
- **Tests:** GOAL-59.11
- **Safety/scope:** GOAL-59.14, 59.15, 59.16

## §3 Components

> Seven components. §3.1–§3.6 are **required**; §3.7 is **optional polish** but documented here because exit-code semantics belong in design, not as folklore. Each component states **purpose, inputs, outputs, behavior, edge cases, file location, and which GOAL(s) it satisfies.**

### §3.1 — Layer A.1 / A.2: existence + reverse checks

**Purpose.** Detect the two simplest drift cases: (A.1) an artifact lives on disk but the graph has no node for it, and (A.2) a graph node has a `doc_path` (from `gid-rs#ISS-058`) that points at a file that no longer exists.

**Inputs.**
- Filesystem walk of `.gid/issues/` and `.gid/features/` (recursive, depth ≤ 2 — issue/feature dirs contain `issue.md` / `feature.md` at known paths).
- Graph DB: `SELECT id, doc_path FROM nodes` for all nodes whose `id` matches issue/feature ID conventions (`ISS-NNN`, feature slug).

**Outputs.**
- `DriftFinding { category: "missing-node", artifact_path: <path>, suggested_fix: "create graph node for <id>", auto_fixable: true, severity: "warn" }` per orphan artifact.
- `DriftFinding { category: "dangling-doc-pointer", node_id: <id>, artifact_path: <stale path>, suggested_fix: "clear doc_path or restore file", auto_fixable: false, severity: "error" }` per dead pointer.

**Lookup precedence (GOAL-59.7).** For every artifact, the canonical lookup is:
1. `SELECT id FROM nodes WHERE doc_path = ?` (primary — uses ISS-058 column)
2. If no row, fall back to `SELECT id FROM nodes WHERE id = <derived_id>` where `derived_id` comes from the directory name (`ISS-058` from `.gid/issues/ISS-058/`)
3. If step 2 is the only way a match is found, emit a `severity: "info"` warning suggesting `gid backfill-doc-path` to repair the pointer (this is the fallback warning required by GOAL-59.7, not a separate drift class).

**Edge cases.**
- Symlinked directories: follow once, deduplicate by canonical path; second hit is ignored.
- Non-`.md` files in artifact dirs (e.g., attachments, `reviews/`, sub-issue scaffolds): ignored — only `issue.md` / `feature.md` / `requirements*.md` / `design*.md` count as artifacts; reviews are scoped to their parent.
- Empty `.gid/issues/` directory (new project): zero findings, not an error.
- Node with `doc_path = NULL`: skipped from A.2 entirely (NULL is not "stale", it's "unknown" — covered by the GOAL-59.7 fallback warning, not by A.2).
- Two artifacts with the same derived ID (e.g., `ISS-059` appearing twice because someone manually copied a directory): both are reported as `severity: "error"` duplicates with `category: "missing-node"` and `suggested_fix: "deduplicate artifact dirs"`. The graph lookup is performed once; both artifacts get the same finding so the operator sees the duplication.
- Walking performance: for a project with 10,000 issues the walk should stay under 100ms (no recursive `read_to_string`; only `read_dir` + a single frontmatter parse per artifact). Caching is not in scope for v1 — re-walking is fast enough.

**Location.** `gid-core/src/validate/drift/layer_a.rs` (new file under existing `validate/` module). Pure function `check_existence(graph: &Graph, root: &Path) -> Vec<DriftFinding>`.

**Satisfies.** GOAL-59.2, GOAL-59.3, GOAL-59.7.


### §3.2 — Layer A.3: status sync

**Purpose.** Catch the most common silent drift: an issue is marked `closed` in `issue.md` frontmatter but its graph node still says `todo` (or vice versa).

**Canonical mapping table** (reproduced verbatim from issue body — this is the contract, do not invent new mappings):

| Artifact `frontmatter.status` | Graph node `status` |
|---|---|
| `open` | `todo` |
| `in_progress` | `in_progress` |
| `resolved`, `closed`, `done` | `done` |
| `blocked` | `blocked` |
| `cancelled`, `wontfix` | `cancelled` |

**Inputs.** Pairs of `(artifact_status, node_status)` from §3.1's lookup. Statuses outside the table → `DriftFinding { category: "status-drift", severity: "error", suggested_fix: "rename status to one of the canonical values", auto_fixable: false }`.

**Behavior.**
- For each `(artifact, node)` pair, compute canonical form on each side and compare.
- Mismatch → `DriftFinding { category: "status-drift", node_id, artifact_path, suggested_fix: <which side to update>, auto_fixable: true, severity: "warn" }`.

**Conflict resolution (`--prefer`).**
- Default: `--prefer artifact`. Rationale: markdown frontmatter is human-edited (someone went out of their way to type `closed`); graph status is often agent-written and easier to silently corrupt.
- `--prefer node` overrides for cases where the operator knows the graph is the source of truth (e.g., bulk import).
- The flag affects **only** the rendered `suggested_fix` and the action `--fix-drift` takes; the *finding itself* is emitted regardless.

**Edge cases.**
- Empty status (frontmatter or DB NULL): treated as `open`/`todo` respectively (GOAL-59.4 default).
- Case sensitivity: status values are lowercased before comparison; mixed-case in artifact → fix suggestion includes the canonical lowercase form.
- A status not in the table on either side: emit `severity: "error"`, `auto_fixable: false`, with `suggested_fix` listing the canonical values.

**Location.** `gid-core/src/validate/drift/status.rs`. Function `check_status_sync(pairs: &[(Artifact, Node)], prefer: Prefer) -> Vec<DriftFinding>`.

**Satisfies.** GOAL-59.4, GOAL-59.9 (auto-fix path).


### §3.3 — Layer A.4: relations sync

**Purpose.** Catch divergence between artifact frontmatter relations (`relates_to`, `depends_on`, `blocks`, `resolved_by`, `superseded_by`) and the corresponding graph edges.

**Mapping** (relation field → edge `kind`):
- `depends_on` → edge kind `depends_on`
- `blocks` → edge kind `blocks`
- `relates_to` → edge kind `relates_to`
- `resolved_by` → edge kind `resolved_by`
- `superseded_by` → edge kind `superseded_by`

(These are the names already used by `gid_artifact_relate` per AGENTS.md, so no remapping is needed.)

**Inputs.**
- For each artifact, its frontmatter relation lists (each value is a target ID, possibly cross-project as `<proj>:<id>`).
- For each corresponding node, all outgoing edges grouped by kind: `SELECT to_id, kind FROM edges WHERE from_id = ?`.

**Behavior.**
- For each (artifact_relation, node_edge_kind) pair: compute set difference.
- Frontmatter has `X` but graph doesn't → `DriftFinding { category: "missing-edge", node_id, suggested_fix: "add edge from:<id> to:<X> kind:<rel>", auto_fixable: true }`.
- Graph has edge but frontmatter doesn't → `DriftFinding { category: "missing-edge", severity: "warn", suggested_fix: "add `<rel>: <target>` to frontmatter", auto_fixable: false }` (writing markdown frontmatter is a human-author concern; auto-fix only writes the graph side).

**Edge cases.**
- Cross-project refs (`engram:ISS-022`): the target may not exist in the local graph — that's not drift, that's a deliberate cross-project edge. Skip checking the target's existence; just verify the edge is present.
- Self-references: `ISS-059` listing itself as `relates_to` is degenerate; skip and emit `severity: "info"` cleanup hint.
- Empty relation list vs missing key: treated identically (both = no relation).

**Location.** `gid-core/src/validate/drift/relations.rs`. Function `check_relations(pairs: &[(Artifact, Node)], graph: &Graph) -> Vec<DriftFinding>`.

**Satisfies.** GOAL-59.5, GOAL-59.9 (auto-fix path for graph side only).


### §3.4 — Layer B: ledger consistency

**Purpose.** Detect "forgot to update the ledger" (issue body story #2: PR merged, code shipped, `CHANGELOG.md`/`RELEASES.md` never references the issue).

**`.gid/config.yml` schema** (committed by this design — implementations must accept exactly this shape):

```yaml
ledgers:
  - id: changelog                # required, unique within project
    path: CHANGELOG.md           # required, relative to project root
    pattern: '\[ISS-\d+\]'       # optional, regex used to find references; default = match the rendered require_reference
    triggers:
      - artifact_kind: issue     # required: issue | feature
        when_status: [closed, resolved, done]   # required, list (any-of)
        when_labels_any: []      # optional, list (any-of); empty = no label filter
        require_reference: '[{id}]'   # required, template; {id} is substituted with artifact id
```

`triggers:` is a list — multiple per ledger allowed (e.g., one for issues, one for features).

**Inputs.** Parsed `.gid/config.yml` (absent → Layer B silently skipped); all artifacts; file contents of each declared ledger path.

**Behavior.**
1. For each ledger entry, for each trigger, filter artifacts where: `kind == trigger.artifact_kind` AND `status ∈ trigger.when_status` AND (`when_labels_any` empty OR labels intersect).
2. For each filtered artifact, render `require_reference` with `id` substitution.
3. Search the ledger file for the rendered string (or the ledger's `pattern` regex if set, then verify the rendered string is one of the matches).
4. If absent → `DriftFinding { category: "ledger-not-updated", node_id, artifact_path, suggested_fix: "add reference '<rendered>' to <ledger.path>", auto_fixable: false, severity: "warn" }`.

**Edge cases.** Ledger file missing → one `error` finding `{category: "ledger-not-updated", suggested_fix: "create file <ledger.path>"}` per ledger (not per artifact). Multi-line `require_reference` not supported in v1. No `ledgers:` block → never flagged (GOAL-59.6, GUARD-59.4).

**Location.** `gid-core/src/validate/drift/ledger.rs`. Reuses existing `gid_core::config::load_project_config()`.

**Satisfies.** GOAL-59.6.


### §3.5 — Drift report data model

**Purpose.** Single, stable shape for every drift finding, so downstream tooling (CI, dashboards, `--json` consumers) can rely on it.

**Type definition** (Rust, in `gid-core/src/validate/drift/mod.rs`):

```rust
pub struct DriftFinding {
    pub category: DriftCategory,
    pub severity: Severity,           // info | warn | error
    pub node_id: Option<String>,      // None for missing-node
    pub artifact_path: Option<PathBuf>, // None for orphan node cases
    pub message: String,              // human-readable one-liner
    pub suggested_fix: String,        // imperative ("add edge ...", "set status to ...")
    pub auto_fixable: bool,           // matches the §3.6 fix engine's capability
    pub fix_target: FixTarget,        // Graph | Artifact | Ledger | None
}

pub enum DriftCategory {
    MissingNode,
    DanglingDocPointer,
    StatusDrift,
    MissingEdge,
    LedgerNotUpdated,
    // Layer C variants intentionally omitted — added by ISS-060
}
```

**Output formats.**
- **Human:** grouped by category, then severity, then node_id. One line per finding plus an indented `suggested_fix:` line.
- **JSON (`--json`):** `{"findings": [DriftFinding...], "summary": {"by_category": {...}, "by_severity": {...}, "auto_fixable": <count>}}`. Schema is stable — adding new categories is additive (clients tolerate unknown enum values per the existing `gid_validate` JSON contract).

**Edge cases.**
- A finding with `auto_fixable: true` but `fix_target: None` is a bug — assert in debug builds.
- Multi-finding for the same `(node_id, category)` pair: deduplicate before rendering, keep the highest severity.

**Location.** `gid-core/src/validate/drift/mod.rs` (the type lives here so all four check modules can return it).

**Satisfies.** GOAL-59.8.


### §3.6 — `--fix-drift` auto-fix engine

**Purpose.** When the operator opts in, apply mechanical fixes for the three safe categories.

**Auto-fixable categories** (and only these — GOAL-59.9):
- `status-drift` → write the canonical status to whichever side `--prefer` lost (default: write to graph node).
- `missing-edge` (graph side missing) → `INSERT INTO edges (from_id, to_id, kind, ...)`.
- `missing-node` (artifact-without-node) → `INSERT INTO nodes (id, status, doc_path, ...)` using artifact frontmatter to populate fields.

**Not auto-fixable** (always require human):
- `dangling-doc-pointer` (could mean "rename" or "wrong file deleted" — too risky)
- `ledger-not-updated` (changelog text is editorial, not mechanical)
- `missing-edge` (artifact side) — writing user-facing markdown frontmatter is left to humans
- Any `severity: "error"` outside the three safe categories

**Workflow.**
1. Collect findings, filter `auto_fixable && severity != "error"`.
2. **Always print proposed diff first** (GUARD-59.2 dry-run): unified diff for files, structured `INSERT/UPDATE` preview for graph rows.
3. `--apply` → execute. Without `--apply` → exit after diff (so `--fix-drift` alone never writes).
4. After apply, re-run drift detection in-process; remaining drift becomes `severity: "warn"` lines in the post-apply summary.

**Idempotency.** Re-running `--fix-drift --apply` on a clean DB must produce zero changes and zero findings. Tested by §6 fixture `tests/fixtures/drift/idempotent/`.

**Atomicity.** All graph mutations for one drift run happen in a single SQL transaction; if any fail, the whole batch rolls back. Frontmatter writes are not part of the v1 fix engine (see "not auto-fixable").

**Location.** `gid-core/src/validate/drift/fix.rs`. Function `apply_fixes(graph: &mut Graph, findings: &[DriftFinding], opts: FixOpts) -> FixReport`.

**Satisfies.** GOAL-59.9, GUARD-59.1, GUARD-59.2.


### §3.7 — `--strict` exit code semantics (optional polish)

**Purpose.** Make CI integration trivial — a single flag turns drift into a hard build failure.

**Exit code matrix** (committed contract, do not change without a major-version bump):

| Condition | Exit code | Notes |
|---|---|---|
| No findings, validation OK | `0` | Default success |
| Drift findings, no `--strict` | `0` | Drift is reported but non-fatal |
| Drift findings + `--strict` | `1` | New behavior introduced by ISS-059 |
| Pre-existing structural validation failure (cycle, broken edge, orphan) | `2` | Unchanged from current `gid_validate` |
| Internal error (DB read failure, malformed YAML, panic) | `3` | Unchanged |

**Precedence rule.** If both drift and structural failures are present, the higher exit code wins (`2` > `1`). This preserves the invariant that "exit ≠ 0 means something is structurally broken" for legacy CI scripts that don't yet know about drift.

**`--strict` interaction with `--fix-drift`.**
- `--fix-drift --strict` (no `--apply`): findings are listed, exit `1` if any remain.
- `--fix-drift --apply --strict`: post-apply re-run determines exit code; if all auto-fixable findings cleared, exit `0` (even if non-auto-fixable findings remain — `--strict` only escalates **drift**, but non-auto-fixable findings *are* drift, so this exits `1`).
  - **Resolved**: `--strict` always exits `1` if any finding remains, regardless of auto-fixability. The fix engine is not a way to silence `--strict`.

**Location.** `crates/gid-cli/src/main.rs` — extends the existing `cmd_validate_ctx` (currently at L1434) by wiring the new exit-code precedence (drift `1` < structural `2`) into its return path. ⚠️ Not `gid-core/src/bin/gid/cmd_validate.rs` — gid-rs has no bin module under `gid-core`; the CLI entry lives in the separate `gid-cli` crate (verified 2026-04-29).

**CI usage examples.**
```bash
# Fail fast on any drift, schedule weekly
gid validate --check-drift --strict || exit $?

# Auto-fix what's safe, then fail if residue
gid validate --fix-drift --apply --strict
```

**Edge cases.**
- Empty graph (no nodes): exit `0` (no drift possible, no structural checks fire).
- Missing `doc_path` (GUARD-59.5): NOT a finding, only a warning to stderr; does not affect exit code.
- Layer C placeholder (GOAL-59.13): emits a `<deferred>` note in the report but contributes no findings; never affects exit code under ISS-059.
- `--quiet` flag: suppresses the human-readable report but **does not** change exit codes (machine-readability is the contract).

**Satisfies.** GOAL-59.10.

## §4 Data flow

> Single command, single pass, deterministic. No daemon, no background indexing, no caching. The sequence below is the contract for `gid validate --check-drift`; `--fix-drift` adds steps after step 7 (see §3.6).

### §4.1 Pipeline (happy path)

```
1. parse CLI flags
       │
       ▼
2. resolve project root  ──────────► .gid/ exists? no → exit 3
       │
       ▼
3. load graph (SQLite)   ──────────► open .gid/graph.db read-only (BEGIN DEFERRED)
       │
       ▼
4. walk artifacts        ──────────► .gid/issues/**, .gid/features/**
       │                              parse frontmatter only (no body)
       │
       ▼
5. load ledger config    ──────────► .gid/config.yml :: ledgers[]
       │                              absent → skip Layer B silently
       │
       ▼
6. run checks (parallel) ──────────► §3.1 existence  ┐
                                     §3.2 status     ├─► Vec<DriftFinding>
                                     §3.3 relations  ┤      (per-check)
                                     §3.4 ledger     ┘
       │
       ▼
7. aggregate + dedup     ──────────► merge, dedup by (node_id, category),
       │                              sort by (severity desc, category, node_id)
       ▼
8. render                ──────────► text (default) | json (--json)
       │
       ▼
9. compute exit code     ──────────► §3.7 matrix
```

### §4.2 Step details

**1. CLI parse.** `--check-drift` is the trigger; companions: `--json`, `--strict`, `--fix-drift`, `--apply`, `--prefer {artifact|node}`. `--apply` without `--fix-drift` → exit 3.

**2. Project root.** Reuse `gid_core::project::find_project_root()`. Missing `.gid/` → exit 3.

**3. Graph load.** Open `.gid/graph.db` with `BEGIN DEFERRED` (GUARD-59.1). Two queries: nodes (`id, status, doc_path` filtered to issue/feature IDs), edges (`from_id, to_id, kind` grouped in-memory by `from_id`). Schema older than v2 (no `doc_path`) → fall back to ID-convention lookup, emit one `info` finding suggesting `gid migrate`.

**4. Artifact walk.** Two phases: (4a) `read_dir` of `.gid/issues/` and `.gid/features/`, filter to ID/slug patterns; (4b) per entry, `read_to_string` of `issue.md`/`feature.md` and **frontmatter-only parse** (stop at closing `---`, never read the body — it would dominate runtime on 10k-issue projects). Result: `Artifact { id, kind, status, labels, relations, doc_path }`. Malformed YAML → `error` finding "fix YAML in <path>", artifact skipped from subsequent checks.

**5. Ledger config.** `gid_core::config::load_project_config()`. Missing/empty `ledgers:` → Layer B skipped (`Ok(vec![])`). Invalid schema → exit 3 (not a drift finding — config errors block validation entirely).

**6. Run checks.** Each layer returns `Result<Vec<DriftFinding>, CheckError>`. Errors aggregate alongside findings, do not abort other layers. Parallelism is **bounded** (rayon `par_iter` over 4 layers, NOT over artifacts within a layer — per-artifact parallelism would race on the SQLite read connection).

**7. Aggregate + dedup.** Concatenate, then dedup per §3.5 (highest severity per `(node_id, category)`). Sort: `(severity_rank desc, category asc, node_id asc)` — errors lead.

**8. Render.** Text (default): group by category → icon + summary + indented `suggested_fix`; trailing `N findings (X auto-fixable)`. JSON (`--json`): schema from §3.5; clients **must** tolerate unknown `category` enum values (forward-compat).

**9. Exit code.** §3.7 matrix. Pre-existing structural checks (cycle/orphan/broken-edge from current `gid_validate`) run alongside drift in the same command; merged under `category: "structural"`. Exit 2 (structural) outranks exit 1 (drift) per §3.7.

### §4.3 Error and edge handling

- **DB locked** (another writer holds the lock): retry with backoff (3 attempts, 100/300/900ms), then exit 3 with message `"graph.db is locked; another gid process may be writing"`.
- **Permission denied on artifact file:** `severity: "error"`, `category: "missing-node"`, `suggested_fix: "fix file permissions on <path>"`. The walk continues.
- **Symlink loops in `.gid/`:** detected by canonical-path dedup in §3.1; never causes infinite recursion.
- **Encoding errors** (non-UTF8 in markdown): treated as malformed frontmatter (see Step 4).
- **Massive output** (>1000 findings): no pagination in v1; the JSON contract is stable, so consumers can pipe to `jq`. A `--limit N` flag is explicitly out of scope (deferred).

### §4.4 Performance budget

- **Target:** end-to-end < 500ms for a project with 1000 issues + 50 features on a warm filesystem (SSD).
- **Budget allocation:** graph load ~50ms, artifact walk ~150ms (dominant), checks ~100ms (parallel, mostly hashmap ops), render ~50ms, overhead ~150ms.
- **No caching in v1.** A future `--watch` mode could cache the artifact walk between runs, but that's an explicit non-goal here (defer to a follow-up issue if needed).
- **`--fix-drift --apply`** adds one transactional write phase between steps 7 and 8 plus a re-run; budget doubles to ~1s, still acceptable for an interactive command.

### §4.5 Concurrency invariants

- The validator opens **one** SQLite connection in deferred-read mode for the entire run. No prepared statements are reused across threads; each layer's check function takes `&Graph` (which holds the connection) and is `Send + Sync`.
- The artifact walk is single-threaded by design — directory I/O on macOS/Linux gets minimal benefit from threading at this scale, and predictable ordering helps debug output.
- The fix engine (§3.6) runs strictly *after* all checks return; checks never observe a half-applied fix.

## §5 GUARDs

> Invariants the implementation MUST enforce. Violations are bugs, not config errors.

### GUARD-59.1 — Drift detection is read-only by default

**Invariant.** `gid validate --check-drift` (without `--fix-drift`) MUST NOT write to disk, the graph DB, or any artifact file.

**Why.** A validator that mutates state is a validator nobody can trust. Pre-commit hooks especially: silent writes race the user's staging step and create non-deterministic commits. Also a hard prerequisite for safe parallel use.

**Enforcement.** Graph connection opened with `BEGIN DEFERRED`, wrapped in a `ReadOnlyGraph` newtype exposing only `&self` query methods. Artifact files opened `read(true)` only; the `fix` module is private and reachable only from the `--fix-drift` path. Test `read_only_invariant` snapshots `mtime` of every `.gid/` file pre/post and asserts equality.

### GUARD-59.2 — `--fix-drift` without `--apply` is dry-run

**Invariant.** `--fix-drift` alone prints the diff and exits without modifying anything. Only `--fix-drift --apply` writes.

**Why.** Mirrors `terraform plan/apply` and `git apply --check`. Lets users inspect proposed changes before committing — especially important for status overrides where the "right" direction depends on `--prefer`.

**Enforcement.** `--apply` without `--fix-drift` exits 3 at CLI parse. Fix engine takes `mode: FixMode { Plan, Apply }`; `Plan` builds the same `Vec<FixOp>` Apply does and drops it. Test `dry_run_no_writes` asserts mtimes/DB unchanged when `--apply` is absent.

### GUARD-59.3 — Layer C (commit drift) is deferred

**Invariant.** This design covers Layers A and B only. Layer C is explicitly out of scope for v1. Implementation MUST NOT add Layer C checks under a hidden flag, and the JSON `category` enum MUST NOT pre-allocate Layer C variants (forward-compat handled by clients tolerating unknown values per §4.2).

**Why.** Layer C reads git history (slow, fragile across rebase/squash) with the highest false-positive rate. Shipping A+B first builds confidence before adding the noisier layer. See §7.1.

**Enforcement.** Code-review boundary. Reviewers reject any PR adding `Category::CommitDrift` or importing `git2`/`git log` from the validator path. `category` enum is `#[non_exhaustive]` so Layer C can land later without a breaking change.

### GUARD-59.4 — Ledger config schema is forward-compatible

**Invariant.** Older `gid` reading a `config.yml` written by a newer version MUST NOT fail. Unknown fields under `ledgers[]` are ignored with a one-shot WARN log. Top-level `ledgers:` may be absent (Layer B disabled).

**Why.** `gid` is a binary distributed across machines on different update cadences (laptop / CI / server). Hard-failing on unknown fields creates flag-day upgrades, which are unworkable.

**Enforcement.** `LedgerConfig` does NOT use `#[serde(deny_unknown_fields)]`. Deserialize via `serde_yaml::Value`, structurally extract known fields, log unknowns at WARN once per load. Test `forward_compat_unknown_fields` adds a fictitious `ledgers[0].future_feature: foo`, asserts load succeeds + WARN emitted + known fields parse.

### GUARD-59.5 — `doc_path NULL` falls back to ID convention with explicit warning

**Invariant.** If a node has `doc_path = NULL`, the checker falls back to `.gid/issues/{ID}/issue.md` AND emits a `severity: "warning"`, `category: "missing-doc-path"` finding with `suggested_fix: "run gid backfill --doc-path"`.

**Why.** `doc_path` is a B1 (`gid-rs#ISS-058`) deliverable; it may be incomplete during rollout. Silent fallback would hide migration debt. Per-node warnings (vs one global) are the right granularity — a single global warning gets ignored.

**Enforcement.** Lookup returns `LookupResult { artifact, used_fallback }`. When `used_fallback`, every check producing a finding attaches a sibling `missing-doc-path` warning, routed through a `FindingBuilder` that emits both in one call. Test `doc_path_null_fallback` asserts both the primary drift finding AND the `missing-doc-path` warning are present.

## §6 Test plan

> Specs only — directory layouts, file contents in prose, assertion shapes. Implementation is deferred to the execute-tasks ritual phase. All tests live under `crates/gid-cli/tests/drift/` (CLI integration) and `crates/gid-core/src/drift/*.rs` (unit, inline `#[cfg(test)]` modules).

### §6.0 Fixture directory convention

Every fixture is a self-contained mini-project under `crates/gid-cli/tests/fixtures/drift/<scenario>/`:

```
<scenario>/
├── .gid/
│   ├── graph.db              ← pre-seeded SQLite, committed to repo (binary, ~16KB each)
│   ├── config.yml            ← (optional, only fixtures exercising Layer B)
│   └── issues/ISS-NNN/issue.md
├── expected/
│   ├── check.json            ← `--check-drift --format json` snapshot (canonical, sorted)
│   ├── check.txt             ← `--check-drift` human format snapshot
│   ├── fix-plan.txt          ← `--fix-drift` (no --apply) plan snapshot
│   └── after-fix-graph.sql   ← `sqlite3 .gid/graph.db .dump` post-`--fix-drift --apply`
└── README.md                 ← one-paragraph: seeded drift + expected detection
```

**Pre-seeded `graph.db`** (not generated at test time): reproducibility (byte-identical inputs), speed (~50 fixtures × seed time would dominate), debug-ability (`sqlite3 fixture/.gid/graph.db`). Cost: an `xtask seed-fixtures` helper for rebuilds — acceptable.

**Snapshot policy:** `insta` for diffing; `INSTA_UPDATE=1 cargo test drift` regenerates. JSON normalized via `drift::testing::canonicalize_report()` (sort findings by `(category, node_id, artifact_path)`, scrub timestamps) before comparison.

### §6.1 Required fixtures (one per drift category)

#### §6.1.1 `missing-node/` — Layer A.1
**Seeded state:**
- `.gid/issues/ISS-101/issue.md` exists with frontmatter `id: ISS-101`, `status: open`, `title: "Test issue"`.
- `graph.db` has zero nodes (or has unrelated nodes but no `ISS-101`).

**Assertions:**
- `gid validate --check-drift --format json` exits 0, output contains exactly one finding with `category: "missing-node"`, `artifact_path: ".gid/issues/ISS-101/issue.md"`, `node_id: null`, `severity: "error"`, `auto_fixable: true`, `suggested_fix.kind: "create-node"`.
- No findings with any other category (no false positives from unrelated checks).
- `gid validate --check-drift --strict` exits 1.
- `gid validate --fix-drift` (dry-run) exits 0, plan output contains literal string `+ create node ISS-101 (status: open)`.
- `gid validate --fix-drift --apply` exits 0, post-state graph contains node `ISS-101` with `status='open'`, `kind='issue'`, `doc_path='.gid/issues/ISS-101/issue.md'`. Re-running `--check-drift` exits 0 with zero findings.

#### §6.1.2 `missing-artifact/` — Layer A.2 (reverse of A.1)
**Seeded state:**
- `graph.db` has node `ISS-202`, `kind='issue'`, `status='open'`, `doc_path='.gid/issues/ISS-202/issue.md'`.
- `.gid/issues/ISS-202/` does not exist on disk.

**Assertions:**
- One finding, `category: "missing-artifact"`, `node_id: "ISS-202"`, `artifact_path: ".gid/issues/ISS-202/issue.md"`, `severity: "error"`, `auto_fixable: false` (we don't auto-create artifacts; humans write issues).
- `suggested_fix.kind: "manual"`, `suggested_fix.hint: "create artifact at .gid/issues/ISS-202/issue.md or remove node from graph"`.
- `--fix-drift --apply` does NOT mutate the graph for this finding (auto_fixable=false). Re-running `--check-drift` after fix attempt still shows the same finding.

#### §6.1.3 `status-drift-artifact-newer/` — Layer A.3, `--prefer artifact`
**Seeded state:**
- Artifact `ISS-303/issue.md` frontmatter: `status: resolved`. File mtime: now.
- Graph node `ISS-303`: `status='todo'`. Node `updated_at`: 1 hour ago.

**Assertions (default `--prefer artifact`):**
- One finding, `category: "status-drift"`, `severity: "warning"`, `auto_fixable: true`.
- Finding's `details` map contains `{"artifact_status": "resolved", "node_status": "todo", "winner": "artifact"}`.
- Plan output: `~ update node ISS-303 status: todo → resolved (source: artifact, mtime newer)`.
- After `--apply`: node `ISS-303` status is `resolved`. Re-check exits 0.

**Variant `status-drift-node-newer/`:** swap mtimes (node updated_at newer). With `--prefer node`, same plan but `winner: "node"` and update target is the artifact frontmatter (file write), not the graph. Use `tempfile`-based fixture clone to avoid mutating the committed-to-repo fixture during `--apply` test.

#### §6.1.4 `missing-edge/` — Layer A.4
**Seeded state:**
- Artifact `ISS-404/issue.md` frontmatter: `relates_to: [ISS-405]`.
- Artifact `ISS-405/issue.md` exists.
- Graph: both nodes present. **No edge** between them.

**Assertions:**
- One finding, `category: "missing-edge"`, `severity: "warning"`, `auto_fixable: true`.
- `details: {"from": "ISS-404", "to": "ISS-405", "relation": "relates_to", "source": "artifact"}`.
- Plan: `+ add edge ISS-404 --[relates_to]--> ISS-405 (source: artifact frontmatter)`.
- After `--apply`: edge present with `relation='relates_to'`. Re-check zero findings.

**Sibling fixture `extra-edge/`:** edge present in graph, NOT in any artifact's frontmatter. `category: "extra-edge"`, `auto_fixable: true` only with `--prefer artifact` (default), proposes deleting the edge. With `--prefer node`, finding is `severity: "info"` and `auto_fixable: false` (we don't invent frontmatter relations from edges automatically — too risky).

#### §6.1.5 `ledger-not-updated/` — Layer B
**Seeded state:**
- `.gid/config.yml` declares one ledger: `ledgers: [{name: "release-notes", path: ".gid/release-notes/CHANGELOG.md", trigger: {on_status: "resolved", scope: "issues"}}]`.
- Artifact `ISS-505/issue.md` status `resolved`, frontmatter date `2026-04-29`.
- `.gid/release-notes/CHANGELOG.md` exists but does not contain the string `ISS-505`.

**Assertions:**
- One finding, `category: "ledger-stale"`, `severity: "warning"`, `auto_fixable: false` (ledger-update logic varies per project; we don't auto-edit CHANGELOGs).
- `details: {"ledger": "release-notes", "missing_artifact": "ISS-505", "expected_in": ".gid/release-notes/CHANGELOG.md"}`.
- `suggested_fix.hint: "add entry for ISS-505 in .gid/release-notes/CHANGELOG.md and re-run"`.

**Sibling fixture `ledger-forward-compat/`:** config declares a ledger with an unknown field `ledgers[0].future_feature: "x"`. Asserts (a) load succeeds, (b) WARN log emitted (captured via `tracing-test` subscriber), (c) the known ledger still functions. Validates GUARD-59.4.

#### §6.1.6 `doc-path-null-fallback/` — GUARD-59.5
**Seeded state:**
- Graph node `ISS-606` with `doc_path = NULL`, `status='todo'`.
- Artifact at conventional location `.gid/issues/ISS-606/issue.md` with `status: resolved` (status drift seeded too).

**Assertions:**
- TWO findings emitted (not one):
  1. Primary: `category: "status-drift"` for the resolved/todo mismatch.
  2. Sibling: `category: "missing-doc-path"`, `severity: "warning"`, `node_id: "ISS-606"`, `suggested_fix.hint: "run gid backfill --doc-path"`.
- Both findings reference the same `node_id`. Validates that `LookupResult.used_fallback=true` triggers the dual-emission via `FindingBuilder` (per §5 GUARD-59.5).

### §6.2 Unit tests (in-crate, fast)

Located in `crates/gid-core/src/drift/*.rs` `#[cfg(test)] mod tests` blocks. No fixtures, build state in-memory.

| Module | Test name | What it asserts |
|---|---|---|
| `drift::layer_a` | `missing_node_detected_when_artifact_orphaned` | Given `Vec<Artifact>` with one orphan, `Vec<Node>` empty → returns one `Finding { category: MissingNode, .. }`. |
| `drift::layer_a` | `status_sync_prefer_artifact_picks_newer_mtime` | Two `prefer` configs × {artifact_newer, node_newer} matrix → 4 cases, asserts `winner` field. |
| `drift::layer_a` | `relations_sync_handles_bidirectional_dedup` | Artifact A has `relates_to: [B]`, B has `relates_to: [A]` → emits ONE missing-edge finding (relation is symmetric for `relates_to`), not two. |
| `drift::layer_b` | `ledger_unknown_fields_warn_not_fail` | Parse config with `future_feature: x` → returns Ok, captures one WARN log. |
| `drift::layer_b` | `ledger_missing_top_level_key_disables_layer_b` | Config without `ledgers:` → Layer B emits zero findings, no error. |
| `drift::report` | `canonicalize_sorts_findings_deterministically` | Shuffle-input findings → canonical output bytes-identical regardless of input order. |
| `drift::fix_engine` | `plan_mode_does_not_call_commit` | Mock `Graph` with assertion-on-write → run fix engine in `Plan` mode → assert no write methods called. Validates GUARD-59.2. |
| `drift::fix_engine` | `apply_mode_atomicity` | Plan with 3 fixes, second fix simulated to fail → assert all 3 rolled back, graph state unchanged. (Requires `BEGIN IMMEDIATE` tx for fix-apply, see §3.6.) |

### §6.3 Integration tests (CLI surface, slower)

Located in `crates/gid-cli/tests/drift_integration.rs`. Each test invokes the `gid` binary via `assert_cmd`, points at a fixture directory.

| Test name | Setup | Assertions |
|---|---|---|
| `read_only_invariant` | Pristine fixture from §6.1.1 | Snapshot `mtime` of every file under `.gid/` (recursive walk). Run `gid validate --check-drift`. Re-snapshot. Assert all mtimes equal AND `graph.db` SHA256 equal. Validates GUARD-59.1. |
| `dry_run_no_writes` | Fixture from §6.1.3 (status-drift) | Snapshot `.gid/`. Run `gid validate --fix-drift` (no `--apply`). Re-snapshot. Assert equal. Stdout contains `~ update node`. Validates GUARD-59.2. |
| `apply_requires_fix_drift` | Any fixture | Run `gid validate --apply` (without `--fix-drift`). Assert exit code 3, stderr contains `--apply requires --fix-drift`. |
| `strict_exit_code` | Fixture with one warning + one error | `--check-drift --strict` → exit 1. `--check-drift` (no strict) → exit 0. |
| `json_output_schema_stable` | Fixture from §6.1.1 | Run `--check-drift --format json`. Assert output parses as `DriftReport` (the public type). Snapshot via `insta` against `expected/check.json`. |
| `fixture_roundtrip_<name>` (one per fixture) | Each fixture in §6.1 | Generated via `macro_rules! drift_fixture_test` — runs the standard 4-step (check → snapshot, plan → snapshot, apply → snapshot, re-check → assert clean). Failure points at which step diverged. |

### §6.4 Regression / cross-cutting tests

| Test name | What it guards against |
|---|---|
| `pre_b1_nodes_handled_gracefully` | Graph created before ISS-058/B1 (no `doc_path` column). Validate runs without panic, emits `missing-doc-path` warnings as designed (GUARD-59.5). Uses a fixture with the older schema explicitly (separate `graph-pre-b1.db`). |
| `large_project_perf` | 1000-issue fixture (generated, not committed; built by `xtask seed-large-fixture`). Asserts `--check-drift` completes in <2s on CI hardware. Performance budget; fails the suite if regressed >50%. |
| `concurrent_validate_safe` | Spawn 4 threads each running `--check-drift` against the same fixture. Assert all 4 exit 0, no DB lock errors, no spurious findings. Validates `BEGIN DEFERRED` actually allows concurrent reads (GUARD-59.1 corollary). |
| `unicode_paths_and_ids` | Fixture with `ISS-777` containing non-ASCII title and a path with a space and a CJK char. Asserts no encoding panics, JSON output is valid UTF-8, snapshot matches. |

### §6.5 Coverage matrix (test → GOAL/GUARD)

| GOAL/GUARD | Covered by |
|---|---|
| GOAL-59.1 (detect missing-node) | §6.1.1, unit `missing_node_detected_when_artifact_orphaned` |
| GOAL-59.2 (detect missing-artifact) | §6.1.2 |
| GOAL-59.3 (status drift) | §6.1.3, unit `status_sync_prefer_artifact_picks_newer_mtime` |
| GOAL-59.4 (relations drift) | §6.1.4, unit `relations_sync_handles_bidirectional_dedup` |
| GOAL-59.5 (Layer B ledger) | §6.1.5 |
| GOAL-59.6 (`--fix-drift` engine) | §6.1.1 apply step, unit `apply_mode_atomicity` |
| GOAL-59.7 (`--strict` semantics) | integration `strict_exit_code` |
| GOAL-59.8 (JSON output) | integration `json_output_schema_stable` |
| GOAL-59.9..16 (drift class definitions) | covered transitively by §6.1 fixtures (one per class) |
| GUARD-59.1 (read-only) | integration `read_only_invariant`, regression `concurrent_validate_safe` |
| GUARD-59.2 (dry-run) | integration `dry_run_no_writes`, unit `plan_mode_does_not_call_commit` |
| GUARD-59.3 (Layer C deferred) | code review only — no runtime test (this guard is a scope boundary, not a behavior) |
| GUARD-59.4 (forward-compat) | §6.1.5 `ledger-forward-compat`, unit `ledger_unknown_fields_warn_not_fail` |
| GUARD-59.5 (doc_path NULL fallback) | §6.1.6, regression `pre_b1_nodes_handled_gracefully` |

Any GOAL or GUARD without a row above is a coverage gap and blocks merge. CI computes this matrix mechanically by parsing test names and asserting full coverage.

### §6.6 Out of scope for this test plan

- Property-based / fuzz testing of the fix engine (deferred to a follow-up; would catch interactions between concurrent fixes).
- End-to-end test that includes git operations (Layer C). Excluded per GUARD-59.3.
- Performance benchmarks beyond the single `large_project_perf` budget gate. A criterion-based bench suite is a separate issue.

## §7 Open questions / deferred

Explicit v1 boundary: anything listed here is NOT in scope for this issue.

### §7.1 Deferred — Layer C: commit drift → owned by gid-rs#ISS-060

**Deferred:** Checks involving `git log` (artifact `resolved` but no commit, commit cites missing ISS, frontmatter `closed_on` older than HEAD touching the file).

**Why:** Layer C requires (1) a commit index `sha → [ISS-NNN]` (ISS-060 Stage 1), (2) graph integration of commit refs (ISS-060 Stage 2), and (3) configurable extraction patterns. Building any of those here would duplicate ISS-060 or block on its undecided design.

**Owner:** [`gid-rs#ISS-060`](../iss-060/issue.md) — verified open, P2, `relates_to: [ISS-058, ISS-059]`; AC includes "Drift checks per Stage 3 wired into `gid_validate --check-drift` (ISS-059)".

**Sunset / forward-compat:** ISS-060 Stage 3 extends `DriftReport.findings` with `UnrecordedResolution`, `StaleClosure`, `DanglingCommitRef`. The `Finding` enum is `#[non_exhaustive]` (§3.5, GUARD-59.3) precisely so this addition is non-breaking.

**Open for ISS-060:** Run by default or behind `--with-commits`? Recommend default-on with graceful degradation when index is missing/stale (warn once, skip Layer C, exit 0). Decision deferred.

### §7.2 Out of scope — cross-project drift

**Excluded:** Drift between artifacts and graphs in **different repositories** (e.g., engram artifact `relates_to: [rustclaw:ISS-052]` going stale).

**Why:** No multi-repo graph model exists; cross-project refs are opaque strings resolved at read time. Federated query or centralized index is a much larger architectural commitment. Volume of cross-project refs is currently small enough that manual `gid_artifact_show <project>:<id>` is acceptable. If this becomes a real problem, file a fresh issue with its own design — retrofitting would be worse than starting clean.

**Boundary clarification:** Same-project cross-*kind* refs (feature → issue) ARE in scope (Layer A.4, §3.3). Only cross-*repository* refs are excluded.

### §7.3 Out of scope — real-time drift watching (filesystem events)

**Excluded:** A `gid validate --watch` daemon using inotify/FSEvents to push notifications on every artifact save or graph mutation.

**Why:** No identified consumer (CLI is on-demand, CI is scheduled, no editor extension exists). Filesystem watching is platform-specific with known reliability gotchas. On-demand validate runs in <2s on a 1000-issue project (§6.4) — `watch -n 60 gid validate --check-drift` is a sufficient composable workaround. If a real consumer emerges (e.g., VS Code extension), they own the design.

### §7.4 Open question — `--prefer` default for status drift

**Question:** Artifact-as-truth or node-as-truth by default?

**Decision:** `--prefer artifact` by default. Frontmatter is source-controlled, human-readable, PR-reviewed; the graph is a derived index. `--prefer node` exists for tooling-driven workflows.

**Revisit trigger:** Add instrumentation in §3.6 counting `winner: node` vs `winner: artifact`; if `winner: node` becomes meaningful (>10%) at quarterly retrospective, reconsider.

### §7.5 Open question — ledger schema discovery vs explicit declaration

**Decision:** Explicit `ledgers:` declarations in `.gid/config.yml` only. Auto-discovery is rejected — it leaks layout opinions and makes "why is this file being checked?" hard to answer.

**Polish (post-v1):** Ship a `gid ledger init` scaffolding helper that writes a sensible default `ledgers:` block based on which conventional directories exist. Not auto-discovery at runtime; one-time scaffold. File as polish issue post-merge.

### §7.6 Deferred — fuzz testing of the fix engine

See §6.6. Most likely latent-bug surface once Layer A.4 lands — concurrent fixes on overlapping edges have non-trivial interaction. Owned by a future issue once real flakes appear; not pre-emptively scoped.

### §7.7 Non-question — static checks vs live invariants

For posterity (so this isn't relitigated): ISS-059 is a *static check tool*. Live invariant enforcement would require intercepting every artifact save and graph mutation — invasive, tightly couples gid to every tool touching `.gid/`. Static checks at human/CI cadence solve the actual problem (silent drift accumulation) at much lower cost. Future arguments for live enforcement must justify why static checks failed in practice.

## §8 References

External dependencies, prerequisites, and sunset targets, with status verified at design time.

### §8.1 Prerequisite — `gid-rs#ISS-058` (`Node.doc_path` schema + backfill)

- **Artifact:** [`.gid/issues/ISS-058/issue.md`](../../issues/ISS-058/issue.md); design [`.gid/features/iss-058-doc-path/design.md`](../iss-058-doc-path/design.md) (87 KB, 8 sections).
- **Status (2026-04-29):** ✅ landed on `main`. Commits: `6a3201d` (design), `3615a02` (impl + migration), `0ecdad8` (backfill CLI).
- **Provides:** `nodes.doc_path TEXT` column (nullable), backfill migration, `gid backfill --doc-path` CLI, layout convention `.gid/<kind>s/<id>/<kind>.md`.
- **ISS-059 cites it at:** §1 (motivation), §2 GOAL-59.3 / 59.7, §3.1 (Layer A SQL), §5 GUARD-59.5, §6.1.6 fixture.
- **Regression failure mode:** Layer A degrades to ID-convention-only matching with `missing-doc-path` warnings (GUARD-59.5). Output gets noisier, build does not break.

### §8.2 Sibling deferral — `gid-rs#ISS-060` (commit-issue linkage)

- **Artifact:** [`.gid/issues/ISS-060/issue.md`](../../issues/ISS-060/issue.md); status open, P2, severity major, `relates_to: [ISS-058, ISS-059]`. Stage 3 explicitly: "Drift checks per Stage 3 wired into `gid_validate --check-drift` (ISS-059)."
- **Owns Layer C of drift detection.** ISS-059 stops at Layers A+B; commit-shaped drift inherited by ISS-060 once Stages 1+2 land.
- **ISS-059 cites it at:** §1 (scope), §2 GOAL-59.13, §3.5 (`Finding` is `non_exhaustive` for forward-compat), §7.1 (handoff).
- **Integration contract:** the `non_exhaustive` enum + `DriftReport.layers: Vec<LayerSection>` are the stable seams ISS-060 extends.

### §8.3 Sunset target — `rustclaw/skills/sync-graph-on-task-complete/`

- **Artifact:** [`/Users/potato/rustclaw/skills/sync-graph-on-task-complete/SKILL.md`](/Users/potato/rustclaw/skills/sync-graph-on-task-complete/SKILL.md), 7.2 KB, `priority: 90`, `always_load: false`.
- **Self-declared sunset condition:** SKILL.md `description` reads *"Stop-gap until gid-rs ISS-058 + ISS-059 ship."*
- **Trigger:** the gid-rs release bundling ISS-059 + CI wiring of `gid validate --check-drift`.
- **Action:** delete `skills/sync-graph-on-task-complete/` on the rustclaw side; the skill is directory-registered, so deletion is sufficient.
- **Replacement:** rustclaw agent loop calls `gid validate --check-drift` at the same trigger points; the CLI replaces what the prompt-level skill tried to enforce.
- **Tracking:** GOAL-59.16. Release PR must include the skill-deletion commit OR a rustclaw follow-up issue with deadline. Leaving both active = two enforcement layers, worse than either alone.

### §8.4 Indirect dependency — `gid-rs#ISS-057` (issue-mode ritual requirements source)

- **Artifact:** [`.gid/issues/ISS-057/issue.md`](../../issues/ISS-057/issue.md); landed `main` at `18e78ab`.
- **Why:** ISS-057 made issue bodies (not a master `requirements.md`) the requirements source for issue-mode rituals. ISS-059's §2 inlined GOALs follow that convention. If reverted, §2 should reduce to a `satisfies:` link list.

### §8.5 Implementation context — `gid-core` crate

- **No new external deps.** Reused: `rusqlite`, `serde`/`serde_json`, `tracing`. Dev-only: `insta` (snapshots), `assert_cmd` (CLI tests), `tracing-test` (may need adding for §6.2 log capture).
- **Crate boundary:** drift logic in new `gid-core::drift::*` module; CLI wiring in `gid-cli::commands::validate`. No changes to `gid-graph` or `gid-extract`.

### §8.6 Documentation to update on ship

- `gid-rs/README.md` — add `--check-drift` / `--fix-drift` to flag table.
- `gid-rs/.gid/docs/issues-index.md` — mark ISS-059 resolved.
- `gid-rs/CHANGELOG.md` — paired ISS-058 + ISS-059 release entry.
- `gid validate --help` — auto-generated from `clap`, just verify it reads sensibly.

### §8.7 Out-of-scope cross-references (do not link from this design)

Listed here so readers don't hunt for connections that aren't there: cross-project refs like `engram:ISS-022` (see §7.2); rustclaw ritual gating (orthogonal); `gid_extract` code-graph layer (artifact-shaped vs code-shaped).
