# Design: ISS-058 — `doc_path` field on graph nodes

> **Source issue:** `gid-rs#ISS-058` (`.gid/issues/ISS-058/issue.md` in this repo). All `ISS-NNN` references below are repo-local to gid-rs unless explicitly prefixed (e.g. `engram#ISS-NNN`).

## 1 Overview

`gid-rs#ISS-058` adds a single `doc_path TEXT` column to the SQLite `nodes` table so every graph node can carry an explicit, structured pointer to its canonical artifact (`.gid/issues/ISS-NNN/issue.md`, `.gid/features/<slug>/design.md`, etc.) instead of relying on convention-derived path inference scattered across `gid_read`, `gid_tasks`, and downstream consumers. The field is populated automatically by `gid_artifact_new` when an artifact is filed, accepted explicitly by `gid_add_task` / `gid_add_feature`, falls back to a documented convention table for legacy nodes (displayed as `<inferred>` until a one-shot `gid migrate doc-paths` back-fill runs), and is validated by `gid_validate` to catch dangling pointers. This is a deliberately minimal schema change — no `doc_anchor`, no JSON `metadata`, no `documented_in` edges — chosen to keep the migration trivial and the data model honest. The primary consumer it unblocks is **ISS-059 (drift detection)**, which needs a structural artifact↔node linkage to compare graph state against artifact state without re-deriving paths from regex; secondarily it lets the `sync-graph-on-task-complete` rustclaw skill be retired (its sunset condition is "ISS-058 + ISS-059 shipped") and removes the silent-drop bug where the SQLite backend currently discards `metadata={"issue_doc": ...}` passed to `gid_add_task`.

<!-- §3-§8 to be filled by subsequent autopilot tasks: schema migration, API surface, back-fill tool, validation, tests, rollout -->

## 2 Requirements coverage

No master `requirements.md` exists at `.gid/docs/requirements.md` for gid-rs as of 2026-04-28, so goals are written inline below as `GOAL-58.N` and guards as `GUARD-58.N`. Each maps directly to an acceptance criterion in `.gid/issues/ISS-058/issue.md`. Subsequent design sections (§3 schema, §4 API, §5 back-fill, §6 validation, §7 tests) will reference these IDs via `satisfies: GOAL-58.N` annotations.

### Goals (functional — what the change must do)

- **GOAL-58.1** — Schema: add `doc_path TEXT NULL` column to the SQLite `nodes` table via a forward-only migration; existing nodes retain all data, the new column defaults to `NULL`. *(maps AC #1)*
- **GOAL-58.2** — Write API: `gid_add_task`, `gid_add_feature`, and any sibling node-creating tools accept an optional `doc_path: String` parameter and persist it verbatim (no normalization beyond stripping a leading `./`). *(maps AC #2)*
- **GOAL-58.3** — Artifact integration: `gid_artifact_new` for `kind ∈ {issue, feature, design, requirements, review}` creates or updates the corresponding node and sets `doc_path` to the freshly allocated artifact path, relative to project root. *(maps AC #3)*
- **GOAL-58.4** — Read API: `gid_read` and `gid_tasks` include a `doc` field per node — the explicit `doc_path` when set, or the convention-derived path tagged `<inferred>` when not. The convention table from the issue body is the canonical source for inference. *(maps AC #4)*
- **GOAL-58.5** — Validation: `gid_validate` flags any node whose `doc_path` is set but whose target file does not exist on disk, under a new finding kind `dangling-doc-pointer`. Inferred paths are NOT validated (they are display-only). *(maps AC #5)*
- **GOAL-58.6** — Back-fill tool: a one-shot subcommand `gid migrate doc-paths` walks all nodes with `doc_path = NULL`, computes the convention-inferred path, and writes it to the column **only when the file actually exists on disk**. Reports counts (filled / skipped-missing / skipped-already-set). *(maps AC #6)*
- **GOAL-58.7** — Test coverage: schema round-trip (insert → read → assert), explicit-set-overrides-convention, back-fill on a fixture project with mixed existing/missing files, validate detects a fabricated dangling pointer. *(maps AC #7)*

### Guards (non-functional — invariants the change must not violate)

- **GUARD-58.1** — Migration is forward-only and idempotent: re-running it on an already-migrated DB is a no-op (no error, no duplicate column). Achieved via `PRAGMA user_version` bump or `IF NOT EXISTS`-style guard.
- **GUARD-58.2** — No silent path mutation: stored `doc_path` round-trips byte-for-byte through write→read. The only allowed normalization is stripping one leading `./`; absolute paths are stored as-is and flagged by validate as `absolute-doc-pointer` (informational, not error).
- **GUARD-58.3** — Convention inference is read-only and pure: it never writes to the DB, never touches the filesystem, and depends only on `(node_type, id)`. The back-fill tool is the *only* code path that converts inferred → explicit.
- **GUARD-58.4** — No regression on the SQLite-silently-drops-metadata bug: after this change, `gid_add_task(metadata={"issue_doc": "..."})` either (a) maps the legacy key to `doc_path` with a deprecation warning, or (b) rejects with a clear error pointing to `doc_path`. Decision deferred to §4 API surface, but silent drop is forbidden.
- **GUARD-58.5** — Out-of-scope features stay out: this design must not introduce `doc_anchor`, a `metadata` JSON column, `documented_in` edges, or any git-aware path tracking. If §3-§8 trend toward those, stop and re-scope.
- **GUARD-58.6** — Cross-project artifact refs (`engram:ISS-022` style, per `gid_artifact_relate`) are out of scope for `doc_path` storage; `doc_path` is always project-local. Cross-project linkage stays in frontmatter relation fields.

## 3 Components

This section breaks the change into four concrete components. Each maps to a specific code surface in `gid-rs/crates/gid-core/`. Implementation lives in §5 (sequence) and §8 (test plan); §3 only defines what each component is responsible for and where its boundary sits.

### 3.1 Schema change & migration runner

**Surface:** `crates/gid-core/src/storage/schema.rs` (verified exists), plus a new `apply_migrations()` helper invoked from `crates/gid-core/src/storage/sqlite.rs` (verified line 187 calls `conn.execute_batch(SCHEMA_SQL)`).

**Responsibility.** Add `doc_path TEXT NULL` to the `nodes` table and bump `schema_version` from `1` to `2`. The change must work on three DB states: (a) brand-new DB created post-change, (b) existing DB at `schema_version = 1` (upgrades in place), (c) existing DB already at `schema_version = 2` (no-op).

**Mechanism.** `SCHEMA_SQL` in `schema.rs` is a single `&str` constant executed once via `execute_batch`. Today there is no schema-evolution framework — only one-shot init. Rather than invent a full migration DSL, the change is delivered in two parts:

1. Update `SCHEMA_SQL` so a freshly-initialized DB has the column from line one. The `CREATE TABLE IF NOT EXISTS nodes` statement gains the column; the trailing `INSERT OR IGNORE INTO config ('schema_version', '1')` becomes `'2'`. New DBs land at v2 directly.
2. Add a tiny `apply_migrations(conn) -> Result<(), StorageError>` function called immediately after `execute_batch(SCHEMA_SQL)` in the SQLite open path. It reads `schema_version` from `config`, and if the value is `'1'`, it executes `ALTER TABLE nodes ADD COLUMN doc_path TEXT` followed by `UPDATE config SET value = '2' WHERE key = 'schema_version'`, in a single transaction.

The two paths converge: existing v1 DBs upgrade via `ALTER TABLE`; new DBs are born at v2; v2 DBs hit the runner and exit immediately because the version check fails. SQLite's `ALTER TABLE ADD COLUMN` populates existing rows with `NULL` automatically (no `UPDATE ... SET doc_path = NULL` sweep needed) — this is a documented SQLite guarantee, cheap, and atomic.

**Why a dedicated column over `metadata.doc_path`.** The `node_metadata` table (verified `schema.rs:56`) is a generic key-value store keyed by `node_id`. Storing `doc_path` there would technically work but is wrong on four counts:

- **typed storage** — `doc_path` is always `Option<String>`, not arbitrary JSON
- **indexable** — a `CREATE INDEX idx_nodes_doc_path` enables O(log n) reverse lookup ("which node owns this artifact?"), critical for ISS-059 drift detection
- **query-friendly** — `SELECT id, doc_path FROM nodes WHERE doc_path IS NOT NULL` is one statement vs. a JOIN on `node_metadata` per row
- **schema discipline** — graph-defining attributes belong in the primary table, not the side bag

The `node_metadata` table stays for genuinely free-form per-node annotations.

**FTS5 interaction.** The `nodes_fts` virtual table (`schema.rs:114`) indexes only `id, title, description, signature, doc_comment` and is sync'd via `nodes_fts_insert/update/delete` triggers. `doc_path` is NOT a search target — users don't full-text-search artifact paths — so the FTS triggers stay untouched. This is a deliberate non-change worth flagging because it's the kind of thing a careless migration would break.

**Out of scope.** No `UNIQUE` constraint on `doc_path` (see GUARD-58.4 and §6); no `NOT NULL` constraint (legacy nodes are pre-doc_path); no foreign key to anything (`doc_path` points to a filesystem artifact, not another graph row).

**Index decision.** Add `CREATE INDEX IF NOT EXISTS idx_nodes_doc_path ON nodes(doc_path) WHERE doc_path IS NOT NULL` as part of the v2 migration. The partial index excludes NULL rows (the majority for legacy DBs and for code-extracted nodes), keeping the index small. ISS-059 drift detection's hot path is "given an artifact path, find the owning node" — without this index that's an O(n) scan of the entire graph. Cost of the index: small write overhead on `put_node` (gid graphs are write-light, query-heavy, so this is the right trade).

**Failure modes considered.**

- **Long-running reader during ALTER:** SQLite serializes via WAL so the writer waits; acceptable.
- **Migration crashes mid-way** (process killed between `ALTER TABLE` and `UPDATE config`): on next open, schema_version is still `1`, the runner re-attempts `ALTER TABLE`, gets a duplicate-column error from SQLite, treats that specific error as "already done" and proceeds to the version bump. This makes the migration crash-safe without explicit transactions across DDL+DML (SQLite DDL is auto-committed and can't be rolled back with the version bump as a unit).
- **Concurrent migration from two processes:** SQLite's `BEGIN IMMEDIATE` on the version-check transaction serializes them; the loser sees `schema_version = 2` already and exits.

**Migration test fixtures.** Three DB fixtures under `crates/gid-core/tests/fixtures/` cover the v1→v2 paths: `v1-empty.db` (schema_version=1, zero nodes — exercises bare ALTER), `v1-populated.db` (schema_version=1, ~50 nodes — exercises ALTER on populated table and verifies row preservation), `v2-fresh.db` (created post-change — verifies new DBs land at v2 directly without invoking the runner). Each fixture is committed as a binary blob in tests/fixtures/ since SQLite DBs aren't text-friendly; alternative — generate them in-test from a SQL script — adds startup cost but eliminates binary-file diff noise. Decision: in-test generation for the populated case (clearer intent in code review), committed blob for the empty/fresh cases (small and stable).

**Schema version constant.** Replace the hardcoded `'1'` literal in `INSERT OR IGNORE INTO config` with a `const CURRENT_SCHEMA_VERSION: u32 = 2` exposed from `schema.rs`. Future migrations bump this constant in one place; the runner reads it instead of hardcoding the target. Trivial today (only one bump) but pays off the moment a v3 lands.

### 3.2 `Node` struct field & serde wiring

**Surface:** `crates/gid-core/src/graph.rs` (the `Node` struct definition and its `Serialize`/`Deserialize` derives), plus the read/write paths in `crates/gid-core/src/storage/sqlite.rs`.

**Responsibility.** Add `pub doc_path: Option<String>` to `Node` and ensure it round-trips byte-for-byte through (a) `put_node` → SQLite → `get_node`, (b) YAML export → YAML import (the legacy `.gid/graph.yml` path, still used by `migration.rs` for one-shot YAML→SQLite conversion), (c) JSON serialization (used by MCP tool responses).

**Field semantics.** `Option<String>` is the right type because three states must be distinguishable:

1. **explicitly set to a real path** — `Some("…/issue.md")`
2. **explicitly absent because the node has no canonical artifact** — `None` (e.g., a code-extracted function node has no human-authored doc)
3. **legacy/never-populated** — `None` (same Rust representation as state 2, distinguished only by historical context)

State 1 vs. {2,3} is the only distinction the system needs to make at runtime; states 2 and 3 are reconciled by the back-fill tool in §3.4.

**serde attributes.** `#[serde(default, skip_serializing_if = "Option::is_none")]`. `default` lets older YAML files (which lack the field) deserialize cleanly into `Node` with `doc_path: None` — this preserves YAML round-trip compatibility, which is non-negotiable until the SQLite migration is universally adopted (`gid migrate` workflow). `skip_serializing_if` keeps emitted YAML/JSON tidy: nodes without a doc_path don't carry an explicit `doc_path: null` line, so existing graph.yml files diff cleanly when only some nodes gain the field.

**SQLite read path.** `get_node` and any `SELECT … FROM nodes` projection that builds a `Node` must include `doc_path` in the column list and bind it to the new field. The column is at the end of the schema, so all positional bindings shift by zero — only the explicit projection list needs updating. There is no "find every SELECT *" hazard; gid-core uses explicit column lists throughout (verified by inspection of `sqlite.rs`).

**SQLite write path.** `put_node` and the bulk `BatchOp::PutNode` path must include `doc_path` in their `INSERT OR REPLACE INTO nodes (…) VALUES (…)` statements. The value is bound from `node.doc_path.as_deref()`, which produces `NULL` for `None` and the borrowed string slice for `Some`.

**No normalization.** The field is stored verbatim except for stripping a single leading `./` (per GUARD-58.2). No path canonicalization, no symlink resolution, no case folding. Reason: `doc_path` is a human-meaningful identifier ("the path you typed when filing this artifact"), not a filesystem handle. Canonicalization would silently mutate paths across machines (different cwd, different symlink layouts) and break diff-based drift detection.

**Backward-compat with `metadata.issue_doc`.** The pre-existing silent-drop bug — `gid_add_task(metadata={"issue_doc": "…"})` discards the key under the SQLite backend — is resolved here in the simplest way that doesn't surprise callers: the `put_node` path checks `metadata` for keys named `issue_doc`, `doc_path`, or `documentation` (case-insensitive); if found and `node.doc_path` is `None`, the value is moved into `doc_path` with a single-line stderr deprecation warning. If both `metadata.issue_doc` and `node.doc_path` are set, `node.doc_path` wins and the metadata key is dropped with a louder warning. This is forward-only: §4 (API surface) makes the explicit field the documented way, and the metadata fallback is removed in a future release.

**Concrete example.** Today, the SQLite backend's `put_node` projection looks like (paraphrased): `INSERT OR REPLACE INTO nodes (id, node_type, title, description, status, priority, file_path, …) VALUES (?, ?, ?, ?, ?, ?, ?, …)`. After this change it becomes `…, file_path, doc_path, …) VALUES (…, ?, ?, …)` with `node.doc_path.as_deref()` bound at the new position. The matching `get_node` projection in `SELECT id, node_type, title, … FROM nodes WHERE id = ?` gains `doc_path` in its column list and the `from_row` constructor reads it via `row.get::<_, Option<String>>("doc_path")?`. There is no schema-driven binding magic in gid-rs — every column is named explicitly in code, which makes this change mechanical but auditable.

**YAML round-trip example.** A pre-change `graph.yml` entry like `- id: ISS-042\n  node_type: issue\n  title: Foo\n  status: open` deserializes cleanly into the new `Node` (with `doc_path: None` via `serde(default)`). Re-serializing it without ever setting `doc_path` produces byte-identical YAML (via `skip_serializing_if`). Setting `doc_path = Some(".gid/issues/ISS-042/issue.md")` and re-serializing produces the same YAML plus one new line `  doc_path: .gid/issues/ISS-042/issue.md`. This matters because the YAML→SQLite migration in `migration.rs` is still the upgrade path for projects on the legacy backend; it must continue to work without modification.

**`Node` constructor changes.** Existing builder-style helpers (e.g., `Node::new_task(id, title)`, `Node::new_feature(id, title)`) gain a `.with_doc_path(path: impl Into<String>)` method on the chain. The base constructors still set `doc_path: None`, preserving binary compatibility for any out-of-tree callers. No constructor variant takes `doc_path` as a required arg — that would force every callsite to know the convention, which is precisely what this design is trying to eliminate.

**Default-derive interaction.** `Node` derives `Default` for test fixtures and YAML deserialization. `Option<String>::default()` is `None`, which is the correct semantic default; no manual `Default` impl needed.

**Equality and hashing.** `Node` derives `PartialEq` and `Eq` for test assertions; `doc_path` participates in equality. This is intentional — two nodes that differ only in `doc_path` ARE different nodes for diff/drift purposes, and tests should fail loudly if a put/get round-trip drops the field. No `Hash` derive on `Node` (large struct, never used as a HashMap key in current code).

### 3.3 Tool integration contract (write & display)

**Surface (contract-only — implementation in rustclaw and gid-rs MCP layer):** `gid_artifact_new`, `gid_artifact_show`, `gid_add_task`, `gid_add_feature`, `gid_read`, `gid_tasks`, plus a future `gid_artifact_move` (not built here).

**Responsibility.** Define **who owns the field at each lifecycle stage** so two tools never race to write inconsistent values, and define **what consumers see** when the field is absent.

**Lifecycle ownership.** Three stages, three owners:

1. **Artifact creation owns the initial write.**
   When `gid_artifact_new` allocates a new artifact (e.g., `ISS-042/issue.md`), it MUST, in the same logical operation, either (a) create a corresponding graph node with `doc_path` set to the new artifact's project-relative path, or (b) update an existing node (matched by ID convention) to set `doc_path` if currently `None`.
   The same-operation rule means atomicity at the API surface, not necessarily at the DB transaction level — gid-rs's `gid_artifact_new` runs its own write, then issues a separate `gid_add_task` / `gid_update_task` call.
   That is acceptable; what is forbidden is leaving the artifact created and the node un-updated for any user-visible duration.

2. **Explicit `gid_add_*` calls own the field for synthetic nodes.**
   When a caller creates a node directly via `gid_add_task` (no artifact involved — e.g., a code-extracted function node, a planned task with no issue file), the caller may pass `doc_path: Some("…")` and the system stores it verbatim.
   The caller is asserting "this path will exist by the time anyone validates"; if they lie, GUARD-58.5's `dangling-doc-pointer` finding will catch it later.

3. **`gid_artifact_move` (future) owns rename propagation.**
   When an artifact is moved (e.g., issue renumbered, feature renamed), the move operation MUST update all nodes whose `doc_path` matches the old path.
   This tool does not exist today; the contract here is forward-looking. Until it lands, renames are a known drift source — manual back-fill via §3.4 covers it.

**Display contract for absent values.** `gid_read`, `gid_tasks`, and the MCP `gid_artifact_show` response include a `doc` field per node. The value is:

- `{ "kind": "explicit", "path": "…" }` when `doc_path IS NOT NULL`
- `{ "kind": "inferred", "path": "…", "exists": true|false }` when `doc_path IS NULL` and convention inference yields a path; the `exists` flag is computed cheaply (single `Path::exists()` call per node) so consumers can distinguish "convention says it's there and it is" from "convention says it should be there but isn't"
- `{ "kind": "none" }` when `doc_path IS NULL` and no convention rule applies (e.g., code-extracted nodes)

The kind tag means downstream consumers (rustclaw skills, ISS-059 drift detection, IDE plugins) can branch on data structure, not on string parsing of "<inferred>" suffixes. This is a small but important shift from the issue body's original "<inferred>" sentinel suggestion — equivalent expressive power, cleaner contract.

**Out of scope for this component.** The actual sync layer that watches artifact filesystem changes and propagates them to graph nodes is **rustclaw ISS-052's job** (the V2Executor / ritual quality gate work). This design.md flags rustclaw#ISS-052 as a downstream consumer that depends on the contract above; it does not specify the watcher's implementation.

**Worked example — issue creation.** Caller invokes `gid_artifact_new(kind="issue", title="Add doc_path field")`. The Layout module allocates `ISS-058` and writes `.gid/issues/ISS-058/issue.md`. The same tool then either (a) creates a fresh `Node { id: "ISS-058", node_type: "issue", title: "Add doc_path field", doc_path: Some(".gid/issues/ISS-058/issue.md"), … }` via `gid_add_task`, or (b) if a node with that ID already exists (someone pre-created the task), updates it via a partial-update path that sets `doc_path` only if it was `None`. Subsequent `gid_artifact_show ISS-058` reads the node, sees `doc_path` populated, and returns the artifact via the explicit pointer — no convention regex involved.

**Worked example — legacy node display.** A code-extracted node `func:auth.rs:login` exists from a pre-doc_path extraction. It has `doc_path: NULL`. When `gid_read` returns it, the convention table yields no rule for `node_type=function`, so the `doc` field is `{ "kind": "none" }`. A consumer rendering this in `gid tasks` output simply omits the doc column for this row.

### 3.4 Back-fill subcommand (`gid migrate doc-paths`)

**Surface:** new subcommand under `gid` CLI, plumbed through `crates/gid-cli/src/main.rs` (the existing single-file clap entry point — subcommand variants are defined inline as enum cases under the top-level `Commands` enum, so the migration command is a new variant alongside `Validate`, `Extract`, etc.). Default invocation: `gid migrate doc-paths` (with `--dry-run` as the default behavior, requiring explicit `--apply` to write).

**Responsibility.** For every node in `.gid/graph.db` where `doc_path IS NULL`, compute the convention-inferred artifact path; check whether that file exists on disk; if it does, propose (or apply) `UPDATE nodes SET doc_path = ? WHERE id = ?`. The tool exists because the v1→v2 schema migration (§3.1) only adds the column — it does not retro-populate existing rows. Without back-fill, ISS-059 drift detection would have nothing to compare against for any pre-2026-04-29 node.

**Inference rules (canonical table — used identically by §3.3's display contract and this tool):**

- `node_type = "issue"`, `id = "ISS-042"` → `.gid/issues/ISS-042/issue.md`
- `node_type = "feature"`, `id = "auth"` → `.gid/features/auth/design.md` (preferring `design.md` over `requirements.md` because design is the more stable artifact; `requirements.md` is a separate node type if it warrants one)
- `node_type = "design"`, `id = "auth/r2"` → `.gid/features/auth/design.md` (the `r2` revision suffix is informational; the canonical doc is the current `design.md`)
- `node_type = "review"`, parent feature known → `.gid/features/<parent>/reviews/<id>.md`
- `node_type ∈ {"task", "code", "function", "class", "module", "file"}` → no inference; these nodes legitimately have no canonical authored artifact and should remain `doc_path = NULL`

**Default `--dry-run` behavior.** Walks all NULL-doc_path nodes, applies the inference table, classifies each into one of:

- **fillable** — convention rule applies AND target file exists → would set `doc_path`
- **skipped-missing** — convention rule applies but target file does not exist → leaves NULL, prints `would skip: node X → path Y (file not found)`
- **skipped-no-rule** — node type has no convention (`task`, `code`, etc.) → leaves NULL, silent
- **already-set** — `doc_path IS NOT NULL` → leaves alone, silent

Output is a summary (`123 fillable, 4 skipped-missing, 89 skipped-no-rule, 12 already-set`) plus the full per-node lines for the first two categories. `--verbose` prints all four. `--apply` switches from print-only to actual `UPDATE` statements wrapped in a single transaction.

**Idempotence.** Running `gid migrate doc-paths --apply` on a fully-populated DB is a no-op: every node falls into `already-set` or `skipped-no-rule`, the transaction commits zero `UPDATE`s, and exit code is `0`. Running it twice in a row produces identical output the second time. This is GUARD-58.1 (idempotent migration) at the back-fill layer.

**Why a separate subcommand vs. running on every DB open.** Two reasons. First, file-existence checks involve filesystem I/O proportional to graph size; doing this on every CLI invocation would add noticeable latency to fast-path commands like `gid tasks`. Second, back-fill is a one-time corrective action — once the tool runs, future nodes are born with explicit `doc_path` from §3.3's lifecycle contract, so there's nothing for a recurring back-fill to do.

**Tonight's impl scope (per autopilot task spec).** Build the subcommand with `--dry-run` (default) and `--apply`; one integration test that constructs a fixture DB with three NULL-doc_path nodes (one fillable, one skipped-missing, one skipped-no-rule), runs the tool in dry-run mode, and asserts the summary counts. Heuristic edge cases (multi-revision designs, rename history, pre-Layout legacy issues) are deferred to a follow-up.



## 4 Data flow

§3 defined the static surfaces: a column, a struct field, a write/display contract, and a back-fill tool. §4 traces how a `doc_path` value moves through the system across the three lifecycle events that matter — node creation, artifact rename, and artifact deletion. The point of this section is to make clear which transitions §3 actually handles, which it handles partially, and which are explicitly deferred to ISS-059 (drift detection, the next autopilot task).

Throughout this section, "tool" means any code path that mutates the graph: CLI subcommands (`gid_artifact_new`, `gid_extract`, `gid_design --parse`, the rituals' `parse-design` phase), MCP tool handlers, and the storage-layer Rust API used by tests. They share the `NodeUpsert` shape from §3.3, so the data flow is identical regardless of entry point.

### 4.1 Node create with known artifact path

This is the happy path and the only flow §3.3 fully owns end-to-end. The trigger is a tool that creates a new graph node and *also* knows the canonical authored artifact for that node — for example, `gid_artifact_new` allocating `.gid/issues/ISS-073/issue.md` and immediately upserting an `issue` node, or `gid design --parse` parsing a design YAML and upserting `feature` nodes whose owning `design.md` was just written.

The flow:

1. The tool computes (or already knows) the artifact path. For artifact-creating tools this is the path it just wrote to disk; for parsers this is the path of the file being parsed.
2. The tool constructs a `NodeUpsert { id, node_type, doc_path: Some(path), .. }` and hands it to `storage::upsert_node`.
3. `upsert_node` writes the row, with `doc_path` populated from the start. The `display_path()` accessor (§3.2) returns the explicit value without consulting the convention table.
4. Subsequent reads (`gid_tasks`, `gid_artifact_show`, ritual context assembly) see the explicit `doc_path` and route artifact resolution through it directly, never touching the inference fallback.

The invariant this flow establishes: **a node born after the v2 migration with a known artifact has an explicit `doc_path` from row zero.** No back-fill needed for these. This is what makes §3.4's back-fill a one-time corrective action rather than a recurring sync.

What §3 does *not* cover here, and why it's acceptable: tools that create nodes *without* a known artifact — `gid_extract` walking source code, ritual `plan-tasks` decomposing a feature into `task` nodes — continue to insert with `doc_path = NULL`. §3.3's inference rules in §3.4 (the canonical table) explicitly list `task`, `code`, `function`, `class`, `module`, `file` as "no inference" types. They legitimately have no canonical authored artifact, and a NULL there is correct, not drift. ISS-059 must reflect this in its drift classifier — a NULL `doc_path` on an extracted code node is healthy; a NULL `doc_path` on an `issue` or `feature` node is a back-fill candidate.

### 4.2 Artifact rename

The trigger is any operation that moves an artifact file on disk: `gid_refactor` renaming a feature directory, a `mv` of `.gid/issues/ISS-042/issue.md` to a new ID slot (rare but possible during issue ID reallocation), or a Layout-driven slug change on a feature.

This is where §3 is deliberately incomplete, and the design needs to be honest about that. There are two sub-cases.

**4.2a — Rename routed through a graph-aware tool.** When the rename is initiated by a tool that already touches the graph (e.g., `gid_refactor` operating on node IDs, or a ritual phase that knows it's renaming), the tool is responsible for emitting a `NodeUpsert` with the new `doc_path` in the same transaction as the file move. §3.3's contract requires this: any tool that moves an artifact and knows the owning node MUST update `doc_path` atomically with the file system change. The tool order is "write file, then upsert" — if the upsert fails, the file move is left in place but the node still points at the old path, which surfaces as drift in 4.3 and gets caught by ISS-059. We accept this small window because making file-system moves transactional with SQLite would require either a two-phase commit protocol or a write-ahead log on the file system, both well out of scope for ISS-058.

**4.2b — Rename done out-of-band.** When the rename happens outside any graph-aware tool — `git mv`, manual `mv` in a terminal, an editor's "rename file" action — the graph has no signal that anything changed. The node continues to point at the old (now non-existent) path. From the graph's perspective this is indistinguishable from case 4.3 (deletion): `doc_path` references a file that does not exist. ISS-058 does **not** detect or auto-correct this; that detection is the entire premise of ISS-059's `gid_validate --check-drift`. The §3.4 back-fill tool *also* does not help here, because back-fill only fills NULLs — it does not overwrite existing non-NULL `doc_path` values, even if the target file is missing (that would be a destructive corrective action, which the design explicitly avoids).

The practical implication for tonight's scope: ISS-058 lands the column and the convention, and trusts graph-aware tools to keep `doc_path` in sync on rename. Out-of-band renames produce drift, which is exactly what ISS-059 is designed to surface. This division is intentional — ISS-058 is the schema; ISS-059 is the auditor.

### 4.3 Artifact deletion (drift)

The trigger is any operation that removes an artifact file: `rm`, `git rm`, `gid_artifact` deletion (if/when added), or a ritual's cleanup phase removing a superseded design revision.

The flow when deletion happens through a graph-aware tool would mirror 4.2a: the tool is expected to either delete the corresponding node or null its `doc_path` in the same transaction. **§3 does not enforce this** — there is no DB-level cascade from artifact deletion to node mutation, because the artifact is a file on disk, not a row referenced by a foreign key. Enforcement is purely contractual.

The flow when deletion happens out-of-band is identical to 4.2b's failure mode: the node retains its `doc_path`, but the target file no longer exists. The graph now has a non-NULL `doc_path` pointing at a deleted artifact. This is the canonical "drift" condition.

What ISS-058 contributes to detecting this: nothing at runtime, but everything structurally. By making `doc_path` an explicit column rather than an inferred-on-read value, ISS-058 turns drift from "an inconsistency between two implicit conventions" into "a row whose `doc_path` column points at a non-existent file." That is a query ISS-059 can express exactly: `SELECT id, doc_path FROM nodes WHERE doc_path IS NOT NULL AND <file does not exist>`. The file-existence check is the work ISS-059 owns; the column it can check against is the work ISS-058 lands.

### 4.4 Lifecycle summary

The four lifecycle states a `doc_path` can be in, and which component handles each:

- **Born explicit** (4.1) — node created with known artifact, `doc_path` set at insert. **Owned by ISS-058 §3.3.**
- **Born NULL, fillable** — node created without artifact (e.g., legacy pre-v2 row, or a tool that didn't pass `doc_path`), but a convention rule applies and the target file exists. **Owned by ISS-058 §3.4 back-fill tool.**
- **Born NULL, no rule** — node type has no canonical artifact (`task`, `code`, `function`, etc.). `doc_path` stays NULL forever; this is correct state, not drift. **Implicitly handled by ISS-058 §3.4's inference table.**
- **Drift** (4.2b, 4.3) — non-NULL `doc_path` pointing at a non-existent or moved file. **Detected by ISS-059, not ISS-058.**

This four-state classification is the contract ISS-059's `gid_validate --check-drift` will partition the node table against. ISS-058's job ends at "the column exists, is populated correctly on creation, and can be back-filled for legacy rows"; ISS-059's job begins at "tell the human which non-NULL `doc_path`s are stale."

## 5 Migration sequence

§3 defined what each component is. §4 defined how data moves between them. §5 defines **the order in which the components ship**, the verification that gates each step, and the rollback affordance at each boundary. This is the implementation playbook — concrete enough that a sub-agent (or a sleepy potato) can follow it without re-deriving the design, but the section deliberately stops short of writing the actual code. Code lives in §8's test plan and the eventual commits.

The sequencing rule: **each step must leave the codebase compilable, the test suite green, and the `.db` schema usable by both the previous and next step.** No "write the column then write the migration in a separate commit" patterns where intermediate states have a column that nothing reads. This is a strict invariant — it is how the schema-version-2 migration becomes safe to roll forward and (selectively) backward.

### 5.1 Step ordering & rationale

There are five logical changes (numbered to match §3's components):

1. **S1 — `schema.rs`: extend `SCHEMA_SQL` and bump default version to `'2'`.**
   Single edit to the static SQL constant: add `doc_path TEXT` to the `nodes` `CREATE TABLE`, change `INSERT OR IGNORE INTO config (key, value) VALUES ('schema_version', '1')` to `'2'`, and add the partial index `CREATE INDEX IF NOT EXISTS idx_nodes_doc_path ON nodes(doc_path) WHERE doc_path IS NOT NULL`. This is the smallest, most contained edit and ships first.
   **Why first:** new DBs created during subsequent steps land at v2 directly. Without this, any test that creates a fresh DB during S2-S5 development would land at v1 and trigger the migration runner — fine in theory but adds a moving target during S2 development.
   **Verification:** `cargo test -p gid-core` still green (no behavior change for existing tests because the new column is nullable and unread); manual `sqlite3 <fresh-db> ".schema nodes"` shows the new column.

2. **S2 — `storage/sqlite.rs`: add `apply_migrations()` runner; invoke after `execute_batch(SCHEMA_SQL)`.**
   The runner reads `schema_version` from `config`, branches on the value, and for `'1'` runs `ALTER TABLE nodes ADD COLUMN doc_path TEXT` + `CREATE INDEX …` + `UPDATE config SET value = '2'` inside a `BEGIN IMMEDIATE` transaction. The duplicate-column-error catch from §3.1's failure-modes block is implemented here.
   **Why second:** S1 made the SQL constant authoritative; S2 makes existing v1 DBs catch up. Splitting them allows S1 to merge cleanly without dragging in a runtime concept (the migration runner has its own error path and warrants its own review). Between S1 and S2, the `.db` file format is *forwards-compatible* (new code reads old DBs as v1, old code never sees v2 because it doesn't exist yet); this is the safe transitional state for git bisect.
   **Verification:** new test `migration_v1_to_v2_idempotent` exercises three fixtures (empty, populated, already-v2). Test list per §3.1's "Migration test fixtures" paragraph. After S2, `sqlite3` on a v1 fixture confirms `schema_version = 2` and `PRAGMA table_info(nodes)` shows `doc_path`.

3. **S3 — `graph.rs`: add `pub doc_path: Option<String>` to `Node`; serde-wire it.**
   Two small edits: the struct field with `#[serde(skip_serializing_if = "Option::is_none", default)]`, and a `display_path()` accessor returning `self.doc_path.as_deref()` for callers that don't want to know about the `Option`. (Actual convention-fallback logic lives in §3.3's display contract, not in `display_path()` — that accessor returns *only* the explicit value.)
   **Why third:** the column exists in the DB (S1+S2). Adding the struct field after the schema change means the Rust type compiles against a DB that already has the column, so the next step's read/write wiring has a real target.
   **Verification:** `cargo build -p gid-core` clean; the existing `Node` round-trip tests in `graph.rs` pass without modification because `serde(default)` makes the new field tolerant of older serialized blobs.

4. **S4 — `storage/sqlite.rs`: extend `get_node` / `put_node` to read/write `doc_path`.**
   This is the "wire the field through the storage layer" step. `put_node`'s `INSERT OR REPLACE` SQL gains a `doc_path` column and bound parameter; `get_node`'s `SELECT` adds `doc_path` and the row mapper extracts it into the struct. The code change here is the one the spec explicitly says **not to inline into design.md** — `crates/gid-core/src/storage/sqlite.rs` is named as the integration point, but the actual SQL/Rust diff stays out of the design doc.
   **Why fourth:** the struct field exists (S3), the column exists (S1+S2). Wiring read/write completes the round-trip. Crucially, S4 is the first step where `doc_path` becomes *observable* — a node written with `doc_path: Some(...)` will round-trip correctly via the storage layer.
   **Verification:** new unit test in `storage/sqlite.rs` writes a `Node` with `doc_path: Some("foo.md")`, reads it back, asserts equality. Existing storage tests must still pass (the change is additive). `cargo test -p gid-core --lib` runs the storage suite.

5. **S5 — CLI / MCP surface: add `--doc-path` to `gid add` / `gid update`; plumb through to `NodeUpsert`.**
   Two CLI flag additions in `crates/gid-cli/src/main.rs` (where the `add` and `update` subcommands are defined as variants on the `Commands` enum — the `gid-cli` crate is a single-file clap entry point, not a multi-file `cli/` module). The flag accepts a string, validates it's a project-relative path (no `/` prefix, no `..` traversal), and threads through to the underlying `NodeUpsert` shape from §3.3.
   **rustclaw side is explicitly out of scope.** The MCP tool surface (`gid_add_task`, `gid_artifact_new`, etc.) lives in rustclaw's tool wrapper layer; updating those wrappers to accept `doc_path` is a downstream task tracked separately. ISS-058's CLI work is *only* the gid-rs CLI binary.
   **Verification:** `gid add task foo --doc-path .gid/issues/ISS-058/issue.md` then `gid tasks --json | jq '.[] | select(.id == "foo") | .doc'` returns the explicit path. No new failure modes — the flag is purely additive.

### 5.2 What §5 does *not* schedule

Three changes belong to ISS-058 conceptually but are scheduled separately:

- **Back-fill subcommand (§3.4 — `gid migrate doc-paths`).** Lands as a sixth step (S6) but in a separate commit on top of S1-S5. Reason: the back-fill tool reads from the column populated by S1-S4. It cannot ship before them. It also has its own test surface (dry-run output format, `.gid/config.yml` ledger persistence) that warrants a focused review. Splitting it out keeps S1-S5 as "the schema lands" and S6 as "the migration helper lands" — two clean reviewable units.
- **Tool-side `doc_path` writes for existing creators.** §3.3's lifecycle ownership says `gid_artifact_new` MUST set `doc_path` at creation. That edit lives in rustclaw's MCP tool wrapper, not in gid-rs. ISS-058's commit on the gid-rs side accepts the field; rustclaw's commit (filed as a separate issue, referenced at the bottom of §7) populates it.
- **Drift detection (`gid_validate --check-drift`).** Owned by ISS-059, the next autopilot task. ISS-058 does not land detection logic.

### 5.3 Verification gates between steps

Each step ends with the same three checks before moving to the next:

1. `cargo test -p gid-core` (or `--lib` for storage-only steps) — must be green.
2. `cargo clippy -p gid-core --all-targets -- -D warnings` — must be clean (catches accidental dead-code or unused imports from partial wiring).
3. `git status --short` — must be clean after `git add` + `git commit`. The autopilot task tracker explicitly forbids leaving uncommitted work between steps.

If any gate fails, the step is rolled back via `git reset --hard HEAD~1` and the failure is logged in the daily log before retrying. Three-failure rule from the autopilot DoD applies.

### 5.4 Rollback affordance

The migration is **forward-only** by design (GUARD-58.3 forbids dropping the column), but selective rollback at the *step* level is supported:

- After S1 (constant edit): trivial revert — flip the constant back, `git revert` the commit. No DB migration to undo because the runner doesn't exist yet.
- After S2 (runner): a v1→v2 DB cannot be cleanly reverted to v1 (would require dropping the column, forbidden). Recovery path: keep the column, revert the runner code, accept that v2 DBs touched by the broken code retain their (NULL-only) `doc_path` column. No data loss; just an unused column on rolled-back deployments. Acceptable because the column is nullable and unread by S1's reverted code.
- After S3-S5: standard `git revert` of the relevant commits. The DB schema stays at v2 (irrelevant — the column is nullable and unread), the Rust code drops the field. Future re-application is a clean re-apply of the reverted commits.

The asymmetry — column is forward-only, code is freely reversible — is the right shape for a schema-additive migration. It matches what the SQLite docs recommend for evolving schemas and aligns with the "no drops, ever" guard in §6.

### 5.5 Order summary (for the commit log)

```
commit 1: design(iss-058): doc_path schema + migration design       [this design.md]
commit 2: feat(iss-058): add doc_path column + bump schema to v2     [S1]
commit 3: feat(iss-058): apply_migrations runner for v1→v2           [S2]
commit 4: feat(iss-058): Node.doc_path field + serde wiring          [S3]
commit 5: feat(iss-058): storage round-trip for doc_path             [S4]
commit 6: feat(iss-058): --doc-path CLI flag on add/update           [S5]
commit 7: feat(iss-058): gid migrate doc-paths back-fill subcommand  [S6, deferred from this sequence]
```

Six implementation commits + one design commit = seven total. Each is a self-contained, reviewable unit. The autopilot tracker accepts a "design only" partial-completion (commit 1 only) as `⚠️ partial`; a full B1 completion is commits 1-6 (S6 may slip to a follow-up commit if time runs out, since it's strictly additive on top of a working S1-S5).

## 6 GUARDs

GUARDs are non-negotiable invariants. They are the "things that must not happen, ever" — distinct from goals (functional outcomes) and design choices (which are negotiable). Every GUARD here must be enforceable at code-review time (a reviewer can point at a diff and say "this violates GUARD-X") and ideally at test time (a regression test can fail the build if violated). The four GUARDs below are inherited from §2's coverage matrix; this section *defines* them — what they mean, how they are enforced, what failure looks like, and how they are tested.

These GUARDs are not policies that live in a wiki. They are **structural constraints on the implementation**, encoded in the code itself wherever possible.

### 6.1 GUARD-58.1 — `doc_path` must point to an existing file at write time

**Statement:** When a node is written (via `put_node`, `gid add`, `gid update`, or any tool wrapper) with a non-null `doc_path`, the path *must* resolve to an existing file relative to the project root. Otherwise the write is rejected with a `MissingDocPath` error.

**Why this guard exists:** §3.3's lifecycle ownership says creators must set `doc_path` at the same moment the artifact file is created. Without GUARD-58.1, a buggy creator could set `doc_path = ".gid/issues/ISS-9999/issue.md"` *before* the file was written (or after a typo) and the graph would silently carry a broken pointer. The whole value proposition of ISS-058 — "no convention guessing, the path is authoritative" — collapses if the path can be authoritative *and* wrong.

**Enforcement surface:**
- `crates/gid-core/src/storage/sqlite.rs` — `put_node` validates before `INSERT OR REPLACE`. If `doc_path.is_some()`, resolve `<project_root>/<doc_path>` and `Path::try_exists()`. On `Ok(false)` or `Err`, return `Error::MissingDocPath { node_id, doc_path }`.
- `crates/gid-cli/src/main.rs` — the `add` and `update` subcommand handlers (defined inline on the `Commands` enum) call the same existence check before constructing the `NodeUpsert`. CLI fails fast with a human-readable message ("file not found: .gid/issues/ISS-058/issue.md — create the file before adding the node").
- Tool wrappers (rustclaw side, out of scope for this design) inherit the check via the storage-layer enforcement — they cannot bypass it.

**Failure mode:** the write fails. No partial state. The caller gets a structured error and can either (a) create the file first, then retry, or (b) drop the `doc_path` and write the node anyway (degrading to ID convention via §3.3's display fallback).

**Explicit non-coverage:** GUARD-58.1 enforces existence at *write* time, not at *read* time. If a file is deleted after the node is written, `doc_path` becomes stale — that is the **drift** case, owned by ISS-059 (`gid_validate --check-drift`). Validating on every read would be O(N) filesystem hits per query and is the wrong layer. Drift is a periodic-check problem, not a per-read problem.

**Test contract (for §8):**
- Unit: `put_node_rejects_missing_doc_path` — construct a `Node` with `doc_path: Some("nonexistent.md")`, assert `MissingDocPath` error.
- Unit: `put_node_accepts_existing_doc_path` — create temp file, set `doc_path`, assert success and round-trip.
- Unit: `put_node_accepts_null_doc_path` — `doc_path: None` always succeeds (regression for "didn't break the legacy path").
- CLI: `gid_add_rejects_missing_doc_path_with_human_message` — invokes the binary, asserts non-zero exit and stderr contains the file-not-found message.

### 6.2 GUARD-58.2 — Schema migration must be idempotent

**Statement:** Running `apply_migrations()` on a database that is *already* at `schema_version = 2` must be a no-op. No SQL is executed, no rows are touched, no errors are raised, no log lines are produced beyond a single `debug!` confirming "schema already at v2".

**Why this guard exists:** the migration runner runs *every time* `SqliteStorage::open()` is called — that's once per process for `gid` CLI invocations, but potentially many times in long-lived processes (rustclaw daemon, MCP servers). A non-idempotent migration would either (a) crash on second run (re-applying `ALTER TABLE` raises "duplicate column"), (b) produce divergent DB states across re-runs, or (c) flood logs with spurious "migrated v1→v2" entries. All three are unacceptable for a tool that potato runs hundreds of times a day.

**Enforcement surface:**
- `crates/gid-core/src/storage/sqlite.rs` — `apply_migrations()` reads `config.schema_version` first. The match branches:
  - `Some("2")` → return `Ok(())` immediately. No SQL touched.
  - `Some("1")` → run migration block (ALTER + INDEX + UPDATE) inside a single transaction.
  - `None` (config row missing) → treat as a corruption case; log a warning and bail out with `Error::CorruptSchema`. Do *not* attempt a guess-migration — that is how data gets silently mangled.
  - `Some(other)` → unknown version. Bail with `Error::UnknownSchemaVersion(other)`. Forward-compatibility is a future-version concern.
- The duplicate-column-error catch from §3.1's failure-modes paragraph is a *defense in depth* — it catches the case where the version check passes but the column was somehow already added (e.g., manual `sqlite3` intervention by a debugging human). The primary idempotency mechanism is the version check; the duplicate-column catch is a second line.

**Failure mode:** if idempotency breaks, the symptom is loud: every CLI invocation logs a migration line, and on the second run the duplicate-column error fires. This is intentionally noisy because silent re-migration is far worse than a noisy bug. A test catches it before any user does.

**Test contract (for §8):**
- Unit: `migration_v2_is_noop` — open a fresh DB (lands at v2 from S1), call `apply_migrations()` again, assert no error, assert `PRAGMA table_info(nodes)` returned identical column list before and after.
- Unit: `migration_v1_to_v2_idempotent_after_completion` — start with a v1 fixture, run migration, run migration again, assert second run is a no-op (compare DB byte-hash before and after the second call — must be identical except for SQLite's internal page-cache journaling, which we ignore by checking `.dump` output).
- Integration: `cli_repeated_invocation_no_log_spam` — run `gid tasks` 10 times against a v2 DB, grep stderr for "migrating", assert zero matches.

### 6.3 GUARD-58.3 — Dropping the `doc_path` column is forbidden in any future migration

**Statement:** No future schema migration may include `ALTER TABLE nodes DROP COLUMN doc_path` or any equivalent operation that removes the column. The column is **forward-only**. Even if a future design decides to rename it, deprecate it, or move the data elsewhere, the column itself stays in the schema as a tombstone (potentially nullable, ignored by code) until and unless a major version bump (e.g., schema v10) explicitly accepts data loss.

**Why this guard exists:** once `doc_path` is populated for a node, that path is the *only* record of which document the node refers to. There is no second source of truth. If the column is dropped, every populated row's path is annihilated. Recovery would require re-running `gid migrate doc-paths` (the back-fill subcommand from §3.4) and hoping the convention-matching heuristic finds the right files — but if anything has been renamed since the original write, the data is genuinely lost.

The asymmetry from §5.4 is the right shape: the column is forward-only because it carries irreplaceable user data. The Rust *code* that reads/writes it is freely reversible; the *data* is not.

**Enforcement surface:**
- This is a **review-time GUARD**, not a code-time GUARD. There is no compiler check or runtime assertion that prevents a future contributor from writing a `DROP COLUMN` migration — SQLite would happily execute it.
- Enforcement lives in: (a) this design document being canonical, (b) `crates/gid-core/src/storage/schema.rs` having a `// GUARD-58.3: doc_path column is forward-only — see .gid/features/iss-058-doc-path/design.md §6.3` comment near `SCHEMA_SQL`, (c) the migration runner having a similar comment near each version branch, (d) the project's review checklist (review-design skill) including a "does this PR touch schema in a way that loses data?" check.
- A CI lint *could* be added (regex for `DROP COLUMN` in migration files, fail the build) but is out of scope for ISS-058. Filed as a follow-up consideration in §7.

**Failure mode:** the failure is irreversible data loss for any user who has populated `doc_path` on production nodes. There is no automatic recovery; affected users would need to re-run back-fill and accept whatever the convention-matcher can salvage.

**Test contract (for §8):**
- There is no positive test for this GUARD because it is a *non-action* invariant. The relevant test is the migration test suite from GUARD-58.2 — those tests exhaustively enumerate the legal migration shapes; any future PR that adds a `DROP COLUMN` migration would need to also add or modify those tests, at which point the review catches it.
- A weaker safety net: a `schema_columns_only_grow` regression test that snapshots the current `nodes` table schema and asserts that no future migration removes columns. This is documented as a follow-up in §7 (cheap, useful, not strictly necessary for ISS-058 to land).

### 6.4 GUARD-58.4 — `doc_path` is NOT unique; `UNIQUE INDEX(doc_path)` is forbidden

**Statement:** Multiple nodes may share the same `doc_path` value. The schema must NOT declare `UNIQUE INDEX(doc_path)` or `UNIQUE` on the column itself. The non-unique partial index `idx_nodes_doc_path` is the correct shape and is the only index allowed on this column for the foreseeable future.

**Why this guard exists:** the canonical example is a feature with sub-tasks. The feature node and all its sub-task nodes typically point to the same `design.md` — that is the *whole point* of having a feature directory with one design doc. Concretely, in this very feature: `feature:iss-058-doc-path` and its 6+ sub-tasks (B1-Schema, B2-Storage, B3-CLI, etc.) all have `doc_path = ".gid/features/iss-058-doc-path/design.md"`. A unique constraint would reject all but the first node and silently break the entire ISS-058 graph.

A second case: backfill via `gid migrate doc-paths` (§3.4) deliberately writes the same path to a feature node and all matching child task nodes. Without GUARD-58.4 the back-fill would fail on the second insert.

**Enforcement surface:**
- `crates/gid-core/src/storage/schema.rs` — the `CREATE INDEX` statement is explicitly `CREATE INDEX IF NOT EXISTS idx_nodes_doc_path ON nodes(doc_path) WHERE doc_path IS NOT NULL`. No `UNIQUE` keyword. The column declaration is `doc_path TEXT` (no `UNIQUE`, no `PRIMARY KEY`).
- A `// GUARD-58.4: doc_path is intentionally non-unique — feature + sub-task nodes share design docs` comment near both the column declaration and the index declaration.
- The migration runner's `CREATE INDEX` for the v1→v2 migration uses the same non-unique form.

**Failure mode:** if a future contributor adds `UNIQUE INDEX(doc_path)` (perhaps as an "optimization"), every feature with sub-tasks breaks at write time. Existing graphs become impossible to back-fill. This would be caught by the test below before merging.

**Test contract (for §8):**
- Unit: `multiple_nodes_can_share_doc_path` — write two nodes with the same `doc_path`, assert both succeed, assert both round-trip.
- Unit: `feature_and_subtasks_share_design_doc` — concrete fixture with one feature node + three sub-task nodes all set to the same path, assert all four nodes are stored and queryable.
- Schema-shape test: `nodes_table_doc_path_index_is_not_unique` — query `sqlite_master` for the index definition, assert the SQL string does NOT contain "UNIQUE".

### 6.5 GUARD interactions and conflict resolution

These four GUARDs interact in two non-obvious ways that are worth calling out:

**GUARD-58.1 vs GUARD-58.4 (existence vs non-uniqueness):** GUARD-58.1 requires the file to exist; GUARD-58.4 allows multiple nodes to point to it. Together they imply: many nodes may point to one file, but every one of those nodes individually validates the file's existence at write time. This is fine — the existence check is per-write, not per-file. The cost is N filesystem `stat` calls for N writes; in practice writes happen one at a time and the cost is negligible.

**GUARD-58.2 vs GUARD-58.3 (idempotency vs forward-only):** the idempotency check in GUARD-58.2 (`schema_version = '2'` short-circuits) means re-running migrations *already* refuses to drop columns, because no SQL fires at all. GUARD-58.3 is therefore more about *future migrations* (v2→v3 or beyond) than about the v1→v2 step itself. The v1→v2 migration is provably non-destructive (only ADDs); the future-migration concern is what GUARD-58.3 protects against.

**No GUARD conflicts.** All four are compatible and mutually reinforcing.

## 7 Open questions and deferred work

This section is the design's honest accounting of what ISS-058 deliberately does **not** decide. Every item below was raised during design but ruled out of scope tonight — either because it belongs to a different issue, because the right answer depends on data we don't have yet, or because solving it would expand the change beyond what a single autopilot session can land safely.

The rule for §7 entries: each item names a follow-up owner (existing issue, new issue to file, or "decide when needed"), and articulates *why deferring is safe* — i.e., what breaks if we never come back to it (usually: nothing immediate, the system degrades gracefully). Items that *would* break the system if deferred do not belong in §7; they belong in §3-§6 with a hard answer.

### 7.1 Cross-repo `doc_path` (the headline deferred question)

**The case.** A node in one project's graph wants to point at a document that lives in a *different* project's repository. Concrete example: a node in engram's `.gid/graph.db` representing "the consumer of rustclaw's MCP `doc_path` contract" wants to reference `rustclaw#.gid/docs/mcp-tools.md` — a document that physically does not exist inside engram's repo. Today, the only mechanism for cross-project linkage is the frontmatter relation field on artifacts (`relates_to: rustclaw#ISS-058`, handled by `gid_artifact_relate`), not graph-node-to-document linkage.

**Why deferred.** Resolving this requires a decision that ISS-058's column shape cannot make alone:

- **Option α** — keep `doc_path` strictly project-local (current GUARD-58.6 wording in §2). Cross-repo references stay in artifact frontmatter (`relates_to: <project>:<id>`). This is the lowest-friction path and matches how `gid_artifact_relate` already works. Cost: graph-level "which document does this node refer to?" queries cannot reach across repos.
- **Option β** — extend `doc_path` to accept a `<project>:<path>` form (e.g., `rustclaw:.gid/docs/mcp-tools.md`). Requires a project-name resolver in `gid-core` (probably reading `~/.config/gid/projects.yml`, the same registry used by `start_ritual`'s work-unit). The existence-check in GUARD-58.1 becomes "resolve project name → resolve path inside that project's root → `Path::try_exists()`". Non-trivial: it leaks the project registry into the storage layer, which today knows nothing about the multi-project world.
- **Option γ** — store cross-repo references in a *separate* column (`external_doc_ref TEXT`) with explicit "we know this points outside this repo, don't try to validate it locally" semantics. Most honest typing, most schema bloat.

**Decision: defer. File a follow-up issue when the first concrete cross-repo case lands.** The design's working assumption is Option α — `doc_path` is project-local, GUARD-58.6 stands as written, cross-repo linkage stays in frontmatter via `gid_artifact_relate`. This assumption costs nothing today because we have zero in-flight nodes whose canonical doc lives in another repo. The day the first one appears, we'll know which option fits the actual access pattern (does the consumer want to dereference the path, or just record that it exists?), and the new issue can be designed against real requirements rather than speculation.

**Follow-up issue to file (when needed):** `gid-rs#ISS-NNN — cross-repo doc_path references`. Not filed tonight to avoid issue-tracker pollution with a problem that has no current victim. Pre-condition for filing: a real node, in a real graph, that has a real artifact reference into another repo, and a real consumer that wants to dereference it.

**Why deferring is safe.** Until a cross-repo case actually exists, the rule "doc_path is project-local" is enforced by the artifact-existence GUARD-58.1: a write attempt with a path that doesn't resolve inside the current project root fails fast. There is no silent mis-storage path. The worst that happens is a future contributor *tries* to write `rustclaw:foo.md`, the existence check fails (because there's no file at `<engram-root>/rustclaw:foo.md`), they get a clear error, and they file the follow-up issue at that point. The system fails *loudly*, which is the right shape for a deferred decision.

### 7.2 Tool-side `doc_path` writes (rustclaw MCP wrapper)

§3.3's lifecycle-ownership paragraph mandates that `gid_artifact_new` set `doc_path` at creation time. That edit lives in `/Users/potato/rustclaw/src/tools.rs` (where all `gid_*` MCP tool wrappers live — including `gid_artifact_new` near line 6667 and `gid_add_task` near line 3588 as of this writing — and is the canonical surface for tool-side population). It is **not** part of ISS-058's gid-rs scope. The gid-rs commit lands the schema and the storage round-trip; rustclaw needs a paired commit to actually populate the column from the tool layer.

**Follow-up issue to file:** `rustclaw#ISS-NNN — populate doc_path in gid_artifact_new and gid_add_task`. To be filed once gid-rs ISS-058 ships and the new field is available on `crates.io` or via path-dep. Until that ships, the column exists, is nullable, and stays NULL for all newly-created artifacts — same behavior as legacy nodes today, which is exactly the state the §3.4 back-fill subcommand is designed to fix in a single sweep.

**Why deferring is safe.** ISS-058's S6 back-fill subcommand will populate `doc_path` for every NULL row whose artifact can be matched by convention. The rustclaw-side fix prevents *new* drift from accumulating; until it lands, every CLI invocation creates one or two more NULL-doc_path nodes that the back-fill will sweep up next time it runs. No data loss, no schema corruption, just a lengthening list of nodes that need the next back-fill pass.

### 7.3 Legacy `metadata={"issue_doc": ...}` migration

GUARD-58.4 (silent-drop forbidden) requires that `gid_add_task(metadata={"issue_doc": "..."})` either map the legacy key to `doc_path` with a deprecation warning, *or* reject with a clear error pointing to `--doc-path`. The decision was deferred from §2 to §4, then from §4 to here, because the right answer depends on whether any caller in the wild is *actually* passing `metadata={"issue_doc": ...}`.

**Investigation needed before deciding:** `grep -r 'issue_doc' /Users/potato/clawd/projects/ /Users/potato/rustclaw/` to count callers. If zero in-tree callers, just reject with the "use --doc-path" error message — no deprecation period needed. If non-zero, add the soft-mapping with a warning and file a follow-up to remove the soft path after one release cycle.

**Follow-up issue to file:** `gid-rs#ISS-NNN — handle legacy metadata.issue_doc key`. To be filed *or* closed-without-action after the grep above, depending on what it finds. The S5 CLI step in §5 ships with the strict-rejection behavior by default; if the grep finds callers, we relax to soft-mapping in a follow-up commit before S5 lands.

**Why deferring is safe.** The current SQLite backend's behavior is silent-drop, which GUARD-58.4 forbids. Whatever ISS-058 ships — strict reject *or* soft-map-with-warning — is strictly better than today's silent drop. The only risk in deferring the soft-vs-strict decision is mild: if we ship strict rejection and there are in-tree callers, they break loudly. That is acceptable because (a) loud breakage is fixable in five minutes once observed, (b) silent breakage is what GUARD-58.4 explicitly forbids, and (c) the grep is cheap and we'll do it before S5 lands.

### 7.4 Back-fill subcommand: heuristic edge cases

§3.4's back-fill scope is intentionally narrow: walk NULL-doc_path nodes, apply the §3.4 convention table, fill what matches, report what doesn't. Three classes of node will not be cleanly back-filled by this naive matcher:

- **Multi-revision designs.** A feature with `design.md`, `design-v2.md`, and `design-v3.md` (we do this — see e.g. `requirements-r2.md` patterns in engram). The back-filler picks `design.md` per the canonical table; if the actually-canonical doc is a versioned variant, the fill is wrong. Currently no automated way to detect this — would need a heuristic ("most recently modified design-*.md") that is itself wrong in ~10% of cases.
- **Rename history.** A feature whose slug was renamed: graph node ID is `feature:old-slug`, on-disk directory is `.gid/features/new-slug/`. Convention matcher misses entirely. Back-fill leaves the row NULL with a "no match" reason in the dry-run report.
- **Pre-Layout legacy issues.** Issues filed before the Layout standardization (different directory shape, different filename conventions). Some predate `.gid/issues/ISS-NNN/issue.md` entirely — the file might be `.gid/issues/ISS-NNN.md` flat. Back-fill needs a second-pass matcher; not in tonight's scope.

**Follow-up issue to file:** `gid-rs#ISS-NNN — back-fill heuristic improvements (multi-revision, rename history, pre-Layout)`. File after S6 ships and we have real "no-match" counts from the dry-run reports — the priority of each sub-heuristic depends on which of the three classes is actually most common in production graphs.

**Why deferring is safe.** Tonight's back-fill leaves un-matched rows as NULL with a reason code. NULL rows are still valid (legacy state); they just remain candidates for a future back-fill pass. No data loss, no incorrect data. The conservative posture — "fill what we're sure of, leave the rest NULL with a reason" — is exactly the right shape for a tool that runs against production graphs.

### 7.5 CI lint for `DROP COLUMN`

GUARD-58.3 forbids ever dropping the `doc_path` column. Enforcement today is review-time (code comments at three sites, design-review checklist). A CI lint would catch violations *before* review — a cheap regex (`DROP\s+COLUMN` in any file under `crates/gid-core/src/storage/`) wired into the existing `cargo clippy` invocation, or a separate `scripts/check-no-drop-column.sh` step.

**Follow-up issue to file:** `gid-rs#ISS-NNN — CI lint forbidding DROP COLUMN in storage code`. Cheap (~30 minutes of work), strictly improves safety, no migration concerns. Filed as P3 — useful but not blocking.

**Why deferring is safe.** GUARD-58.3 is enforced by review today. The CI lint is belt-and-suspenders, not the primary defense. Until it ships, a contributor would have to (a) write a `DROP COLUMN` migration, (b) get it past three reviewers who all see the GUARD-58.3 comments next to `SCHEMA_SQL`, and (c) ignore the design-review skill's checklist item. Possible, but unlikely.

### 7.6 `schema_columns_only_grow` regression test

§6.3 noted this as a cheap, useful safety net: a test that snapshots the current `nodes` table schema and asserts no future migration removes columns. Implementation is one Rust test that runs `PRAGMA table_info(nodes)` against a freshly-migrated DB and asserts the column set is a *superset* of the v2 baseline.

**Follow-up issue to file:** `gid-rs#ISS-NNN — schema_columns_only_grow regression test`. Same priority as 7.5 (P3, cheap, additive). Likely landed in the same PR as the CI lint.

**Why deferring is safe.** Same reasoning as 7.5 — it's defense in depth on a GUARD that already has review-time and code-comment enforcement.

### 7.7 ISS-059 dependency boundary

Not strictly an "open question" — already settled — but worth documenting in §7 because it's the single most important *non-action* of this design. ISS-058 does **not** detect drift, does **not** auto-correct stale `doc_path` values, does **not** validate file existence on read, does **not** offer a "fix all the broken paths" command. All of that is `gid-rs#ISS-059` (the next autopilot task, B2).

The boundary is: ISS-058 makes drift *queryable* (the column is structured, the index is in place); ISS-059 makes drift *visible and actionable* (the validator runs the query, reports findings, optionally repairs). Conflating the two would have made tonight's autopilot task too large to land safely. They are two issues for a reason.

**No follow-up issue.** This is just a forward reference to `gid-rs#ISS-059` for cross-document navigation.

### 7.8 Tracking summary

The follow-up issues to file (or check before filing) once ISS-058 ships:

- `gid-rs#ISS-NNN` — cross-repo doc_path references (file when first concrete case appears)
- `rustclaw#ISS-NNN` — populate doc_path in gid_artifact_new / gid_add_task (file once ISS-058 ships)
- `gid-rs#ISS-NNN` — handle legacy metadata.issue_doc key (file *or* close after grep)
- `gid-rs#ISS-NNN` — back-fill heuristic improvements (file after S6 dry-run report data exists)
- `gid-rs#ISS-NNN` — CI lint forbidding DROP COLUMN (P3, cheap)
- `gid-rs#ISS-NNN` — schema_columns_only_grow regression test (P3, cheap, likely same PR as the lint)

Six follow-ups, all gracefully deferrable, none blocking ISS-058 from landing. Per the autopilot rule from the task header: every cross-repo reference uses `<project>#<id>` form to disambiguate gid-rs ISS-NNN from rustclaw ISS-NNN.

## 8 Test plan

§8 specifies *what* the tests prove, not *how* they are coded. Implementation lives in commits S1-S6 (§5) and lands alongside each step. Every test below has a single line per case: a name (the `#[test] fn` identifier), the precondition shape, the action under test, and the post-condition that must hold. The body is deliberately one-paragraph, no Rust syntax — that is what "specs, not code" means.

Tests are organized in three layers, in cost-of-execution order: **unit** (in-process, fixture DBs, milliseconds per test), **integration** (process-level, snapshot DB files, tenths of a second per test), **e2e** (full tool-wrapper invocation against a real graph, seconds per test). The unit layer must catch every GUARD violation; integration catches migration shape; e2e catches the contract between gid-rs and rustclaw's MCP layer.

The acceptance bar for each layer is stated up front. Tests that fail their layer's acceptance criteria are not "almost passing" — they are blocking on the relevant step's commit (e.g., a failing unit test for GUARD-58.1 blocks S4 from merging).

### 8.1 Unit tests (`crates/gid-core/src/storage/sqlite.rs::tests` and `graph.rs::tests`)

**Acceptance bar:** every `#[test]` in this section runs in <50ms, uses an in-memory or tempfile SQLite DB, and asserts on a single concern. No test in this section may depend on filesystem state outside its own tempdir or on other tests' ordering. `cargo test -p gid-core --lib` runs the full unit suite in <2s on potato's mac mini.

**8.1.1 `node_roundtrip_with_doc_path_some`** — given a `Node` constructed with `doc_path: Some(".gid/issues/ISS-058/issue.md")` (file created in tempdir before write), when `put_node` followed by `get_node`, then the returned `Node` has `doc_path == Some(".gid/issues/ISS-058/issue.md")` and all other fields are byte-identical to the input. Covers S3+S4 storage round-trip. The "file created in tempdir before write" precondition is required by GUARD-58.1; without it, the write itself would fail and we'd never reach the read assertion.

**8.1.2 `node_roundtrip_with_doc_path_none`** — given a `Node` with `doc_path: None`, when `put_node` then `get_node`, then `doc_path == None` round-trips. Crucial regression for "didn't break the legacy NULL-doc_path path." Most existing nodes will be in this state until the S6 back-fill runs.

**8.1.3 `node_serde_serializes_none_as_absent_field`** — given a `Node` with `doc_path: None`, when serialized via `serde_json::to_string`, then the resulting JSON has no `"doc_path"` key (because of `#[serde(skip_serializing_if = "Option::is_none")]` from §3.2). Validates the wire-format contract — JSON consumers (rustclaw MCP layer, gid CLI `--json` output) see only the field when it has a value.

**8.1.4 `node_serde_deserializes_missing_field_as_none`** — given a JSON blob with no `doc_path` key (legacy serialized nodes from before ISS-058), when `serde_json::from_str::<Node>`, then the resulting `Node` has `doc_path: None`. Validates `#[serde(default)]`. Without this guarantee, every existing serialized graph would fail to deserialize after ISS-058 lands — a complete catastrophe.

**8.1.5 `put_node_rejects_missing_doc_path`** — given a `Node` with `doc_path: Some("nonexistent-fake-path-xyz.md")` (no such file in tempdir), when `put_node`, then the call returns `Err(Error::MissingDocPath { node_id, doc_path })` and the row is NOT written (verified by a follow-up `get_node` returning `Ok(None)`). Direct enforcement test for GUARD-58.1.

**8.1.6 `put_node_accepts_null_doc_path_unconditionally`** — given a `Node` with `doc_path: None`, when `put_node`, then `Ok(())` regardless of any filesystem state. Regression for "the existence check only fires when the value is `Some`."

**8.1.7 `multiple_nodes_can_share_doc_path`** — given a temp file `.gid/features/foo/design.md` and two nodes `feature:foo` + `task:foo-impl` both constructed with `doc_path: Some(".gid/features/foo/design.md")`, when both are written via `put_node`, then both writes succeed (no `UNIQUE constraint failed` error) and both round-trip via `get_node`. Direct enforcement test for GUARD-58.4.

**8.1.8 `feature_and_three_subtasks_share_design_doc`** — extension of 8.1.7 with one feature node + three sub-task nodes (the realistic shape from this very feature: `feature:iss-058-doc-path` + `task:iss-058-S1` + `task:iss-058-S2` + `task:iss-058-S3`), all pointing to the same path. Asserts all four are stored, all four round-trip, and the partial index `idx_nodes_doc_path` contains four entries (verified via `EXPLAIN QUERY PLAN` showing the index is used for `WHERE doc_path = ?`).

**8.1.9 `nodes_table_doc_path_index_is_not_unique`** — query `sqlite_master` for the `idx_nodes_doc_path` row, assert the `sql` field does NOT contain "UNIQUE" (case-insensitive). Schema-shape test for GUARD-58.4 that catches "someone optimized the index to UNIQUE" before it breaks 8.1.7. This is a structural assertion, not a behavioral one — the kind of test that fails for "good intentions, bad outcome" PRs.

**8.1.10 `display_path_returns_doc_path_when_set`** — given a `Node` with `doc_path: Some("foo.md")`, when `display_path()` is called, then it returns `Some("foo.md")`. Validates the §3.2 accessor returns explicit values *only* (the convention-fallback lives in the §3.3 display contract, which is tested at the integration layer).

**8.1.11 `display_path_returns_none_when_unset`** — given a `Node` with `doc_path: None`, when `display_path()` is called, then it returns `None`. Pairs with 8.1.10. The convention fallback is *not* invoked at this layer — that's the §3.3 contract, tested in 8.3.

### 8.2 Integration tests (`crates/gid-core/tests/migration.rs`)

**Acceptance bar:** each test runs in <500ms, operates on real SQLite files copied from `crates/gid-core/tests/fixtures/` to a tempdir, exercises the full `SqliteStorage::open()` path including `apply_migrations()`. `cargo test -p gid-core --test migration` runs the full integration suite in <5s.

**Fixtures required (commit S2 also creates `crates/gid-core/tests/fixtures/`):**
- `v1_empty.db` — a v1 schema DB with `schema_version = '1'` in config and zero rows in nodes. Smallest possible v1 surface.
- `v1_populated.db` — a v1 schema DB with 5-10 representative nodes (one feature, several tasks, mixed metadata shapes). Realistic migration target.
- `v2_already.db` — a v2 schema DB created fresh after S1 lands. Used to test idempotency.

**8.2.1 `migration_v1_empty_to_v2`** — given `v1_empty.db` copied to tempdir, when `SqliteStorage::open()`, then post-condition: `schema_version = '2'` in config, `PRAGMA table_info(nodes)` lists `doc_path TEXT` as a column, `idx_nodes_doc_path` exists in `sqlite_master`. The migration of an empty DB exercises the schema-DDL path without confounding row-data assertions.

**8.2.2 `migration_v1_populated_to_v2_preserves_all_rows`** — given `v1_populated.db`, when `SqliteStorage::open()`, then every row that existed in `nodes` pre-migration still exists post-migration with all original column values byte-identical, and the new `doc_path` column is NULL on every row. This is the data-preservation guarantee — GUARD-58.3 forward-only-column rule means no row should be lost or modified by the v1→v2 transition.

**8.2.3 `migration_v2_is_noop`** — given `v2_already.db`, when `SqliteStorage::open()`, then the DB byte-content is unchanged (compare via `.dump` SQL output before and after, ignoring SQLite page-cache journaling). Direct enforcement test for GUARD-58.2 idempotency.

**8.2.4 `migration_runs_repeatedly_without_error`** — given `v1_empty.db`, when `SqliteStorage::open()` is called five times in sequence (each time re-opening the same file), then no error is produced and the post-condition from 8.2.1 holds after every call. Catches regressions where the second call to `apply_migrations()` would crash on duplicate-column.

**8.2.5 `migration_unknown_version_bails_loudly`** — given a DB with `schema_version = '99'` (synthetically constructed in the test setup), when `SqliteStorage::open()`, then the call returns `Err(Error::UnknownSchemaVersion("99"))` and no SQL is executed against the nodes table. Forward-compatibility safety net — refuses to silently downgrade-or-corrupt a future-schema DB.

**8.2.6 `migration_corrupt_schema_bails_loudly`** — given a DB with the config row missing entirely (synthetically deleted in setup), when `SqliteStorage::open()`, then `Err(Error::CorruptSchema)` and no migration runs. Pairs with 8.2.5 — refuses to guess.

**8.2.7 `migration_v1_to_v2_transactional`** — given `v1_populated.db` and a forced failure injected mid-migration (e.g., a debug hook that raises after `ALTER TABLE` but before `UPDATE config`), when `SqliteStorage::open()`, then post-condition: the DB is rolled back to v1 state — `schema_version = '1'`, no `doc_path` column, no `idx_nodes_doc_path`. Validates the `BEGIN IMMEDIATE` transaction wrapping. The "force failure mid-migration" is the only test in §8 that requires test-only code paths in `apply_migrations()`; the hook is gated behind `#[cfg(test)]`.

**8.2.8 `migration_log_output_is_minimal`** — given `v2_already.db`, when `SqliteStorage::open()` is called 10 times, then stderr captured during the calls contains exactly zero "migrating" log lines (and at most one `debug!` per call confirming "already at v2"). Direct enforcement of GUARD-58.2's "no log spam" failure-mode clause.

### 8.3 End-to-end tests (rustclaw MCP wrapper layer, deferred to rustclaw#ISS-NNN)

**Acceptance bar:** each test invokes the actual MCP tool surface (`gid_artifact_new`, `gid_add_task`, `gid_artifact_show`) against a real `.gid/graph.db` in a tempdir-rooted project, asserts on the full request/response cycle. Slower (1-3s per test) because the tool wrapper is a real process boundary. `cargo test -p rustclaw --test e2e_doc_path` runs the e2e suite in <30s.

**Important boundary:** these tests live in *rustclaw's* test tree, not gid-rs's, because the MCP wrapper is rustclaw's responsibility (per §3.3 and §7.2). The follow-up issue `rustclaw#ISS-NNN — populate doc_path in gid_artifact_new` from §7.8 includes implementing these tests as part of its scope. Stating them here means: when the rustclaw-side issue is filed, the test specs are already designed and don't need re-derivation.

**8.3.1 `gid_artifact_new_populates_doc_path`** — given a tempdir project with an empty `.gid/graph.db`, when `gid_artifact_new(kind="issue", title="Test issue")` is invoked via the MCP tool wrapper, then post-condition: a new node exists in the graph whose `doc_path` field equals the path returned in the tool response (typically `.gid/issues/ISS-NNN/issue.md`), AND the file at that path actually exists. This is the contract test for §3.3's lifecycle ownership rule. Without this passing, ISS-058's whole "creators must set doc_path at creation" promise is hollow.

**8.3.2 `gid_artifact_show_uses_explicit_doc_path_over_convention`** — given a graph with two nodes: node A has `doc_path: Some(".gid/issues/ISS-001/custom-name.md")` (file present), node B has `doc_path: None` and ID `issue:ISS-002` (file at `.gid/issues/ISS-002/issue.md` present), when `gid_artifact_show` is called for each, then node A's response references the explicit path and node B's response references the convention-derived path. Validates the §3.3 display contract: explicit wins, convention is the fallback.

**8.3.3 `gid_artifact_show_falls_back_to_convention_for_legacy_nodes`** — given a v2-migrated graph where every existing node has NULL `doc_path` (the realistic post-migration, pre-back-fill state), when `gid_artifact_show` is called for any node whose ID matches a Layout-conventional pattern, then the response contains the convention-derived path and the response metadata indicates the path was derived (not explicit). The "legacy graph still works" guarantee — without this, the migration would break every existing tool invocation until back-fill ran.

**8.3.4 `gid_add_task_with_doc_path_arg_persists_field`** — given a tempdir project with a real `.gid/issues/ISS-099/issue.md` file, when `gid_add_task(id="task-foo", title="...", doc_path=".gid/issues/ISS-099/issue.md")` is invoked, then the resulting node has `doc_path: Some(".gid/issues/ISS-099/issue.md")` (verified via direct `get_node` from gid-core in the test). Validates the rustclaw-side wiring of the new arg.

**8.3.5 `gid_add_task_rejects_missing_doc_path_with_human_message`** — given the same tempdir project but no file at `.gid/issues/ISS-9999/issue.md`, when `gid_add_task(..., doc_path=".gid/issues/ISS-9999/issue.md")` is invoked, then the tool response is an error whose human-readable message contains both the offending path and a hint to create the file first. Direct e2e enforcement of GUARD-58.1 across the tool boundary.

**8.3.6 `legacy_metadata_issue_doc_handling`** — behavior depends on the §7.3 decision (strict reject vs soft-map). Test stub specified now, finalized when the §7.3 grep-and-decide step runs:
- *If strict reject:* given `gid_add_task(metadata={"issue_doc": "foo.md"})`, then the response is an error pointing to `--doc-path`.
- *If soft-map:* given the same call, then the resulting node has `doc_path: Some("foo.md")` AND a deprecation warning appears in the response logs.

**8.3.7 `gid_migrate_doc_paths_dry_run_reports_unchanged_db`** — given a v2 graph with NULL doc_paths for all nodes, when `gid migrate doc-paths --dry-run` is invoked, then the response lists every node and its proposed fill (or reason for no-match) AND no rows in the DB are modified (verified by byte-hash of the .db file before and after). Validates the §3.4 dry-run contract — "preview without commit" is non-negotiable for back-fill safety.

**8.3.8 `gid_migrate_doc_paths_idempotent`** — given a graph already back-filled by a previous run, when `gid migrate doc-paths` is invoked again, then no nodes are touched (proposed-fill count = 0) and the operation completes successfully. Pairs with GUARD-58.2's idempotency theme — back-fill should be runnable repeatedly without harm.

### 8.4 Test-coverage matrix (which tests cover which §6 GUARDs)

| GUARD | Unit | Integration | E2E |
|---|---|---|---|
| **58.1** existence at write | 8.1.5, 8.1.6 | — | 8.3.5 |
| **58.2** migration idempotency | — | 8.2.3, 8.2.4, 8.2.8 | 8.3.8 |
| **58.3** no DROP COLUMN | — | (covered by 8.2.2 row-preservation + the proposed `schema_columns_only_grow` follow-up from §7.6) | — |
| **58.4** non-uniqueness | 8.1.7, 8.1.8, 8.1.9 | — | — |
| **58.5** legacy NULL tolerance | 8.1.2, 8.1.4 | 8.2.2 | 8.3.3 |
| **58.6** project-local paths | 8.1.5 (relative-path resolution implicit) | — | 8.3.5 |

Every GUARD has at least one direct test. GUARD-58.3 is the only one whose primary coverage is *structural* (row-preservation in 8.2.2 demonstrates non-destructive migrations; the schema-shape test from §7.6 closes the gap when it ships).

### 8.5 What §8 does *not* test

- **Performance / scale.** No test runs against a graph with >10 nodes. Real production graphs have hundreds of nodes; the unit/integration tests do not catch O(N²) regressions in `put_node` or `apply_migrations()`. This is acceptable because the migration is one-shot and additive (no per-node loops in the runner) and the per-write existence check is a single `stat` call. If a future regression makes either super-linear, it will be caught by manual `gid` invocation latency long before automated tests would notice.
- **Concurrent access.** No test exercises two writers simultaneously hitting the same `.db` file. SQLite's WAL mode plus `BEGIN IMMEDIATE` handles this at the storage-engine layer; ISS-058 adds no new concurrent-access surface (the existence check is read-only against the filesystem, not against the DB). If concurrent writers become a concern, that is a gid-core-wide concern, not an ISS-058 concern.
- **Cross-platform path semantics.** All tests assume POSIX path separators (`/`). On Windows, `Path::try_exists()` would handle the platform difference, but the convention table in §3.4 hardcodes `/` separators in the matcher. This is a known limitation — gid-core's overall Windows support story is "best-effort" today, and ISS-058 inherits that posture.

The exclusions list is itself part of the test plan: future contributors who hit one of these limits know explicitly that ISS-058 did not solve them, and where to start a new design conversation if the limit becomes binding.

