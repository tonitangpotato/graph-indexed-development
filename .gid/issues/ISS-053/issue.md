---
id: "ISS-053"
title: "gid-core has no model for project artifacts (issues, features, designs, reviews, ...)"
status: open
priority: P1
created: 2026-04-26
related: ["ISS-051", "ISS-052", "ISS-029"]
---
# ISS-053 — Project artifacts are not first-class in gid-core

**Status:** open
**Severity:** medium
**Discovered:** 2026-04-26 — agent grabbed `ISS-050` for gid-rs without knowing engram already owned `ISS-050`. potato observed the deeper pattern: `.gid/` already contains *several* artifact types (issue / feature / design / requirements / review / investigation / pivot-note / verify-report / handoff / …) — gid-core knows about *none* of them, so CLI / MCP / rustclaw can't expose operations on them.

> ⚠️ This is v3. v0–v2 had structural mistakes (closed enum kinds, tree-shaped parent, three-tuple ID, separate manual-relations store, bootstrap circularity). Each is now resolved by an explicit decision below. v3 is incrementally drafted — sections fill in order.

## Sections

1. Constraints
2. Reality on disk
3. Decisions (D1–D5)
4. Model (ArtifactRef / Metadata / Artifact / Layout / Relation / ArtifactStore)
5. Wrappers (CLI / MCP / rustclaw)
6. Acceptance criteria — including the binding D2 scalability test
7. Migration & phasing
8. Out of scope
9. Risks
10. Relationship to existing graph tools (coexistence + rustclaw ISS-052 followup)
+ Notes

---

## 1. Constraints (first principles)

Non-negotiable. The model is whatever satisfies all of them simultaneously:

1. **Disk is markdown + directories.** Humans edit with vim. Frontmatter is a hint, not a contract.
2. **New artifact kinds appear constantly and ad hoc.** Today's `.gid/` already contains: issue, feature, design, requirements, review, investigation, pivot-note, verify-report, handoff, tasks-review. Tomorrow more. **Adding a new kind must NOT require editing gid-core code.** A user dropping `issues/ISS-019/PIVOT-NOTE.md` should not be told "register the kind first."
3. **Relations are a graph, not a tree.** Relation kinds also grow (`blocks`, `supersedes`, `derives-from`, `validates`, `applies-to`, …).
4. **Every artifact belongs to a project (namespace).** Cross-project refs explicit; same-project refs short.
5. **CLI / MCP / rustclaw are thin wrappers.** Adding a new artifact kind must produce zero changes in any of these three places.

## 2. Reality on disk (sampled 2026-04-26)

```
.gid/
├── issues/ISS-NNN/
│   ├── issue.md                           ← YAML frontmatter
│   ├── design.md                          ← optional, mixed metadata format
│   ├── investigation.md                   ← optional, ad-hoc
│   ├── PIVOT-NOTE.md                      ← optional, ad-hoc
│   ├── step7a-handoff.md                  ← optional, ad-hoc
│   ├── VERIFY_REPORT-ISS020-P0.md         ← optional, ad-hoc
│   └── reviews/
│       ├── investigation-r1.md
│       └── task-plan-r1.md
├── features/<slug>/
│   ├── requirements.md                    ← markdown headers (`**Date**: ...`)
│   ├── design.md                          ← markdown headers
│   ├── requirements-master.md             ← optional split
│   ├── requirements-1-foo.md              ← optional split, numbered
│   ├── design-1-engine.md                 ← optional split, numbered
│   └── reviews/
│       ├── design-r1.md
│       ├── design-1-engine-r1.md          ← review of split section, r1
│       ├── design-1-engine-r3-part1.md    ← review r3 split into parts
│       ├── requirements-r2-functional.md  ← typed review
│       └── tasks-r1.md
├── rituals/r-XXXXX.json                   ← out of scope (ephemeral state)
└── history/...                            ← out of scope (graph snapshots)
```

Three real properties:
- **Metadata format is mixed.** Issues: YAML frontmatter. Requirements/design/review: `**Field**: value` markdown headers. Some files: nothing.
- **Nesting depth varies.** A review of a split-design's part is 4 levels deep. Three-tuple `(project, kind, id)` cannot represent it.
- **Ad-hoc files are normal.** PIVOT-NOTE, handoff docs, VERIFY_REPORT — these appear at need. Forcing pre-registration kills the workflow.

## 3. Decisions (D1–D5)

These are the points where v0–v2 went wrong. Each is decided here so v3 doesn't re-litigate them.

### D1 — Authoritative ID is the file path, not a kind+local_id tuple

`ArtifactRef = (project, relative_path)`. The path is the source of truth on disk anyway. `kind` is **derived** from the path via Layout (§4), used as a label/filter, never as part of the ID.

Rationale: arbitrary nesting (review-of-split-design-part-1) needs unbounded depth. Path has it natively. Tuple flattening was the v2 mistake.

Short forms (`engram/ISS-022`, `engram/feature/dim-extract`) are sugar resolved by Layout. Authoritative form is always the path.

### D2 — Kind is a string, not an enum; Layout is data, not code

`kind: String`. Layout (`.gid/layout.yml`, with built-in defaults shipped in gid-core) is a list of path patterns → kind labels + ID-allocation rules.

Adding a new kind = editing `layout.yml`. Zero gid-core changes. **This is the test of the design** (§6 has it as an acceptance criterion).

Layout has a `fallback` rule: any `.md` under `.gid/` not matching any pattern becomes `kind = note`. Ad-hoc files (PIVOT-NOTE, handoff) are first-class artifacts immediately, no registration step.

Bootstrap order is trivial: built-in default layout is `include_str!`-ed into gid-core, always present. Optional `.gid/layout.yml` is a merge-override on top.

### D3 — Relations are 100% derived from artifact files; no separate persistence

All relations are read from artifact contents (frontmatter fields, markdown links, directory nesting). No `relations.yml`, no `relations.db`.

"Manual relate" CLI/tool = edits the source artifact's frontmatter (e.g., appends `blocks: [...]`). The user-visible operation is unchanged; the storage model is "edit the file."

Rationale: derived + manual two-store designs always desync. Git already versions the artifact files. Single source of truth.

`RelationIndex` exists but is pure cache — rebuild on file-mtime change. Cold-rebuild scans all artifacts; with <1000 artifacts per project, this is milliseconds.

### D4 — Metadata parser is tolerant and round-trip-preserving (raw-bytes mode)

Metadata reads YAML frontmatter OR `**Field**: value` markdown headers OR nothing. Whichever the file uses, that's what gets read. Same format gets written back.

Round-trip preservation is **byte-level for unmodified fields**. We keep the original metadata block as `raw: String`. Edits to a single field rewrite only that line. Whole-block reformatting only happens if the user explicitly normalizes.

This is mandatory because constraint #1 says humans edit these files in vim. We will not silently re-quote their YAML or change their bold-key spacing.

### D5 — ProjectRegistry is the namespace authority, no new mechanism needed

`~/.config/gid/projects.yml` already exists, populated, with aliases. `resolve(ref)` consults it. Cross-project refs that fail to resolve return a clear error pointing at `gid project register`. No new registry.

This was a non-issue I was overthinking in v2 §Risks.

---

## 4. Model

Five types. Each does one thing. None has hardcoded knowledge of "issue" vs "feature" vs anything else.

### 4.1 ArtifactRef — identity (D1)

```rust
pub struct ArtifactRef {
    pub project: String,           // "engram"
    pub path: PathBuf,             // ".gid/issues/ISS-022/issue.md", relative to project root
}

impl ArtifactRef {
    /// Canonical label: "<project>:<relative_path>"
    pub fn label(&self) -> String;

    /// Parse a reference. Accepts:
    ///   - canonical:  "engram:.gid/issues/ISS-022/issue.md"
    ///   - short:      "engram/ISS-022"        (Layout resolves to path)
    ///   - short:      "engram/feature/dim-extract"
    ///   - short:      "engram/feature/dim-extract/review/design-r1"
    /// Short forms require a Layout to resolve.
    pub fn parse(s: &str, layout: &Layout) -> Result<Self>;

    /// Bare local form (no project), resolved against a contextual project.
    pub fn parse_local(s: &str, project: &str, layout: &Layout) -> Result<Self>;
}
```

The path is the source of truth. Short forms are Layout-resolved sugar.

### 4.2 Metadata — tolerant, round-trip safe (D4)

```rust
pub struct Metadata {
    pub fields: BTreeMap<String, MetaValue>,
    pub source: MetaSource,
    pub raw: String,                // original metadata block, byte-exact
}

pub enum MetaSource {
    YamlFrontmatter,                // ---\n...\n---
    MarkdownHeaders,                // **Key**: value lines at top
    None,                           // file has no metadata block
}

pub enum MetaValue {
    String(String),
    List(Vec<String>),
    Date(NaiveDate),
}

impl Metadata {
    /// Parse from file contents. Tries YAML → markdown headers → None.
    /// Never errors on unknown formats; returns MetaSource::None.
    pub fn parse(file_contents: &str) -> (Self, /* body */ String);

    /// Replace one field's value, rewriting only the affected line(s).
    /// Other lines (including whitespace, comments, key order) preserved byte-exact.
    pub fn set_field(&mut self, key: &str, value: MetaValue);

    /// Emit metadata block in original MetaSource format. If MetaSource::None,
    /// caller must specify a target source.
    pub fn render(&self) -> String;
}
```

Surgical field edits go through `set_field`. Whole-block reformatting requires explicit `normalize()` (not in v1 of this issue's scope).

### 4.3 Artifact — file + parsed metadata + body

```rust
pub struct Artifact {
    pub r#ref: ArtifactRef,
    pub kind: String,                // derived via Layout from r#ref.path
    pub metadata: Metadata,
    pub body: String,                // markdown after metadata block
}
```

`kind` is derived, not stored on disk. Two artifacts with identical paths in two projects have different `r#ref.project`, hence different identity.

### 4.4 Layout — pattern-driven, data over code (D2)

```rust
pub struct Layout {
    patterns: Vec<LayoutPattern>,
    fallback: FallbackRule,
}

pub struct LayoutPattern {
    /// Glob-like with named captures and ID generators.
    /// Examples:
    ///   "issues/{id:ISS-{seq:04}}/issue.md"
    ///   "features/{slug}/requirements.md"
    ///   "features/{slug}/reviews/{name}.md"
    ///   "issues/{parent_id}/reviews/{name}.md"
    ///   "issues/{parent_id}/{any}.md"        (catch issue-attached docs)
    pub pattern: String,
    pub kind: String,
    pub metadata_format: MetaSourceHint,    // expected, used for `create`
    pub id_template: Option<String>,        // for sequenced kinds: "ISS-{seq:04}"
    pub seq_scope: SeqScope,                // Project | Parent
}

pub enum SeqScope {
    Project,                 // ISS-N counts within whole project
    Parent { rel: String },  // r-N counts within parent dir
}

pub struct FallbackRule {
    pub kind: String,                       // "note"
    pub metadata_format: MetaSourceHint,
}
```

#### Default layout (built into gid-core via `include_str!`)

Covers today's reality empirically (issue / feature / design / requirements / review / investigation / handoff / verify-report / pivot-note). Projects can override by writing `.gid/layout.yml`.

#### Adding a new kind

Edit `.gid/layout.yml`, add a pattern. No code change. **This is the test for D2.**

#### Placeholder vocabulary (closed set, documented)

`{seq:NN}` — zero-padded integer, scope per `seq_scope`. `{slug}` — kebab-case identifier. `{name}` — free-form basename. `{parent_id}` — captured parent path segment. `{any}` — catch-all. **No user-defined placeholders in v1.** Future kinds needing `{year}-{seq}` etc. extend this list — that *is* a gid-core change, but adding placeholders is rare and additive (doesn't break existing layouts). Documented in Risks (§9).

### 4.5 Relation — derived from artifact contents (D3)

```rust
pub struct Relation {
    pub from: ArtifactRef,
    pub to: ArtifactRef,
    pub kind: String,                       // "related", "blocks", "supersedes", "reviews", ...
    pub source: RelationSource,             // where we learned it
}

pub enum RelationSource {
    Frontmatter { field: String },          // related: [ISS-051]
    MarkdownLink,                           // [text](.gid/issues/ISS-051/issue.md)
    DirectoryNesting,                       // reviews/X.md → reviews → parent dir
}
```

#### Relation discovery rules (precise)

1. **Frontmatter fields** matching `Layout::relation_fields()` (default: `related`, `blocks`, `blocked_by`, `supersedes`, `derives_from`, `applies_to`, `references`). Each value is parsed as an ArtifactRef.
2. **Markdown links** of the form `[anything](relative_or_absolute_path_to_a_.md_under_.gid/)`. URL fragments and external URLs ignored.
3. **Inline backtick refs** matching `` `<project>/...` `` or `` `<short_form>` `` parseable by `ArtifactRef::parse`.
4. **Directory nesting**: any artifact under `<X>/reviews/Y.md` emits `Relation { from: Y, to: X, kind: "reviews", source: DirectoryNesting }`.

#### "Manual relate" = edit source artifact (D3)

CLI `gid artifact relate A blocks B` → reads A, calls `Metadata::set_field("blocks", merge_list(existing, B))`, writes A back. There is no separate relations store.

### 4.6 ArtifactStore — kind-agnostic operations

```rust
pub struct ArtifactStore {
    project: String,
    project_root: PathBuf,
    layout: Layout,
    index: Mutex<RelationIndex>,            // cache, mtime-invalidated
}

impl ArtifactStore {
    pub fn open(project: &str) -> Result<Self>;

    // Read.
    pub fn list(&self, kind_filter: Option<&str>) -> Result<Vec<Artifact>>;
    pub fn get(&self, r#ref: &ArtifactRef) -> Result<Option<Artifact>>;

    // Allocate.
    pub fn next_id(&self, kind: &str, parent: Option<&ArtifactRef>) -> Result<String>;
    pub fn next_path(&self, kind: &str, parent: Option<&ArtifactRef>, slot_overrides: &SlotMap) -> Result<PathBuf>;

    // Write (all are file writes; D3 means relate also writes a file).
    pub fn create(&self, path: &Path, metadata: Metadata, body: &str) -> Result<Artifact>;
    pub fn update(&self, artifact: &Artifact) -> Result<()>;

    // Relations (read-only; "manual relate" goes through update of source artifact).
    pub fn relations_from(&self, r#ref: &ArtifactRef) -> Result<Vec<Relation>>;
    pub fn relations_to(&self, r#ref: &ArtifactRef) -> Result<Vec<Relation>>;
}

// Cross-project resolver via ProjectRegistry (D5).
pub fn resolve(r#ref: &ArtifactRef) -> Result<Artifact>;
pub fn find_references_to(target: &ArtifactRef) -> Result<Vec<Relation>>;
```

Every operation is **kind-agnostic at the type level**. Behavior differences live in `Layout`.

#### Concurrency model

- `&self` for reads (list/get/relations_*); `&mut self` for index updates.
- Daemon usage: wrap `ArtifactStore` in `Arc<Mutex<...>>` or `Arc<RwLock<...>>` at the call site. gid-core does not impose a sharing model.
- Index invalidation: each query checks dir mtime; if newer than cache, rebuild. Cheap.


## 5. Wrappers

All thin. None grows when a new kind is added.

### 5.1 CLI

```
gid artifact list [--kind K] [--project P]
gid artifact show <ref>
gid artifact new --kind K [--project P] [--parent <ref>] [--title T] [slot=value ...]
gid artifact update <ref> [--field key=value ...]
gid artifact relate <from-ref> <kind> <to-ref>     # = update <from-ref>'s frontmatter
gid artifact refs <ref>                             # find_references_to
```

Six verbs. Stable across all future kinds.

Optional sugar (CLI binary only, NOT gid-core):
- `gid issue new`, `gid issue show`, `gid issue list` -> expand to `gid artifact ... --kind issue`
- Same for `gid feature`, `gid review`, etc.
- Defined in CLI parser, not in the crate. Adding a sugar alias is opt-in cosmetic.

### 5.2 MCP server

Six tools, mirroring CLI:
- `gid_artifact_list { kind?, project? }`
- `gid_artifact_show { ref }`
- `gid_artifact_new { kind, project?, parent?, title?, slots? }`
- `gid_artifact_update { ref, fields }`
- `gid_artifact_relate { from, kind, to }`
- `gid_artifact_refs { ref }`

Each is ~10 lines, calls gid-core directly.

### 5.3 rustclaw native tools

Same six tools, registered as native function-call tools, identical surface to MCP. Implementation: `gid_core::ArtifactStore::open(project)?.method(...)`, return JSON.

## 6. Acceptance criteria

### Type / API

- [ ] `gid_core::{Artifact, ArtifactRef, Metadata, MetaSource, MetaValue, Layout, LayoutPattern, Relation, RelationSource, ArtifactStore, resolve, find_references_to}` all exist and are public.
- [ ] Default `Layout` ships built-in (via `include_str!`); `.gid/layout.yml` override is optional.
- [ ] `ArtifactRef::parse` accepts canonical, short (kind/id), short (kind/slug), and nested-short forms.

### Functional

- [ ] `Metadata::parse` round-trips: read -> no-op `set_field` for an existing key with the same value -> render -> byte-identical to original.
- [ ] `Metadata::set_field` for a new key inserts in original `MetaSource` style.
- [ ] `ArtifactStore::next_id` returns project-scoped ISS-{seq} for issues, parent-scoped r-{seq} for reviews; errors clearly for slug kinds without caller-supplied slot.
- [ ] `ArtifactStore::create` is mkdir-atomic and refuses overwrites.
- [ ] Two stores for different projects can both have `ISS-050` without conflict (regression test for the original collision).
- [ ] `resolve("engram/ISS-022")` works regardless of cwd, via ProjectRegistry.
- [ ] `find_references_to` discovers all four `RelationSource` types per the rules in §4.5.
- [ ] `gid artifact relate A blocks B` modifies A's frontmatter; subsequent `find_references_to(B)` returns the new edge; no `relations.yml`/`.db` is created.

### Scalability test (D2 — the binding test)

- [ ] Add `kind: postmortem` purely via `.gid/layout.yml` (no Rust changes). Run:
  ```
  gid artifact new --kind postmortem --title "..."
  gid artifact list --kind postmortem
  gid artifact show <ref>
  gid artifact relate <pm-ref> applies-to engram/ISS-022
  ```
  All succeed. **gid-core is unmodified.** This test failing means the design failed.

### Wrappers

- [ ] CLI commands (§5.1) implemented; sugar aliases optional.
- [ ] MCP tools (§5.2) implemented.
- [ ] rustclaw tools (§5.3) implemented.

### Documentation

- [ ] `<project>/<short>` reference convention documented.
- [ ] Default layout documented (the patterns shipped in gid-core).
- [ ] Placeholder vocabulary (§4.4) documented as closed v1 set.
- [ ] Relation discovery rules (§4.5) documented precisely.

## 7. Migration

- **No file moves, no renames.** Existing `.gid/` layout becomes the default Layout. On-disk files unchanged.
- **No graph schema changes.** Issues/features that already exist as graph nodes (post-ISS-028 backfill) remain; this issue does not touch the graph layer.
- **Optional `gid artifact lint`**: surfaces bare `ISS-NNN` / unqualified slugs in markdown bodies; suggests project-prefixing. Lint only — never auto-rewrites human prose.
- **Phasing** (single PR is too big):
  1. gid-core types + Layout + Metadata + ArtifactStore (pure, with unit tests).
  2. CLI `gid artifact …` commands.
  3. MCP tools.
  4. rustclaw native tools.
  5. Default layout coverage verified against engram / gid-rs / rustclaw `.gid/` corpora as fixtures.
  6. Optional sugar aliases (`gid issue`, etc.).
  7. Documentation page.

Phase (1) is the only blocker for the others; (2)–(7) can be parallelized after.

## 8. Out of scope

- Renaming directory schemes (rejected; convention preserved).
- Strict frontmatter schema enforcement (would violate constraint #1).
- Sync between artifacts and graph nodes (separate concern; ISS-028 territory).
- Versioning / history of artifact edits (git already does this).
- GitHub Issues sync. Hard no.
- ID allocation across git branches (multi-branch race; see Risks §9).

## 9. Risks

- **Default Layout must cover all current files**, or existing artifacts become invisible (caught by `fallback: kind = note`) and lose kind-specific behavior. Mitigation: derive defaults empirically from engram + gid-rs + rustclaw `.gid/` corpora; ship as fixtures-tested defaults.
- **Tolerant Metadata parser hides typos.** Misspelled frontmatter keys silently become unread fields. Mitigation: `gid artifact lint` flags fields not in the kind's known set; warn, never reject (rejection violates constraint #1).
- **Markdown-link relation discovery is heuristic.** False positives are worse than false negatives (a fake edge confuses graph reasoning more than a missed one). Mitigation: only match `[](...)` and backticked refs that successfully `ArtifactRef::parse`; document the regex precisely.
- **String-typed `kind` admits typos.** `kid: "isue"` quietly creates a new kind. Mitigation: layout's known kinds form a soft set; lint warns on out-of-set kinds. Never errors (would violate D2).
- **Placeholder vocabulary is closed in v1.** A future kind needing `{year}-{seq}` requires extending the placeholder parser in gid-core. This *is* a gid-core change — a deliberate concession. Acceptable because (a) additions are rare, (b) they're additive (don't break existing layouts), (c) a user-pluggable placeholder DSL is out of proportion to the problem. Trade-off documented; revisit if pressure mounts.
- **Multi-branch ID race.** Two git branches both allocate `ISS-053`. On merge, both directories exist. Out of scope here; addressed by branch-aware allocation in a future issue. Workaround: rebase + rename, same as any branch conflict.
- **Round-trip metadata claim is fragile at edges.** Trailing whitespace, BOM, mixed line endings. Mitigation: round-trip test corpus from real `.gid/` files; failures = bugs. Byte-exact promise applies to *unmodified* fields only; whole-block normalization is opt-in.

## 10. Relationship to existing graph tools (in-scope clarification)

rustclaw exposes ~30 native `gid_*` tools today (`gid_tasks`, `gid_add_task`, `gid_update_task`, `gid_complete`, `gid_read`, `gid_query_impact`, `gid_query_deps`, `gid_validate`, `gid_visual`, `gid_extract`, `gid_design`, `gid_plan`, …). These operate on **graph nodes** stored in `.gid/graph.db` (or legacy `graph.yml`). After ISS-028 backfill, issue/feature artifacts also exist as graph nodes — meaning issues live in **two places**:

- **Artifact layer** (this issue): `.gid/issues/ISS-022/issue.md` — source of truth on disk, human-edited.
- **Graph layer** (existing): node `ISS-022` in `graph.db` — structured for fast traversal, derived/manually-backfilled.

ISS-053's scope is **only the artifact layer**. The 6 new `gid_artifact_*` tools (§5) coexist with the 30 existing graph tools without modifying them.

### Decision: temporary coexistence, desync risk explicitly accepted

- `gid_artifact_*` tools read/write artifact files.
- Existing `gid_*` tools read/write graph nodes.
- **No automatic synchronization in this issue's scope.** A user editing `.gid/issues/ISS-022/issue.md` does NOT automatically update the graph node, and vice versa.
- Users may experience drift (e.g., status changed in artifact frontmatter but old in graph). Acceptable in v1 because: (a) the artifact file is authoritative for human workflows, (b) graph queries today already tolerate stale data, (c) the alternative (synchronous bidirectional sync) is a much larger design that belongs in its own issue.

### Followup: rustclaw ISS-052 — Artifact↔Graph synchronization

A separate issue is opened in **rustclaw scope** (`/Users/potato/rustclaw/.gid/issues/ISS-052/issue.md`), not gid-core, because the synchronization layer lives where the tools converge — rustclaw's native `gid_*` tools are the surface where users hit the desync. That issue covers:

- Detect desync on tool entry (artifact frontmatter `status:` vs graph node `status`).
- Decide direction (artifact → graph as default, since artifact is authoritative).
- Optional fs-watch background sync in daemon mode.
- Migration of `gid_add_task` / `gid_update_task` to write through ArtifactStore where the graph node corresponds to an artifact.
- Strategy for graph nodes that have NO artifact counterpart (code nodes from `gid_extract`, computed nodes from `gid_infer`) — these stay graph-only.

Until that issue lands, the practical guidance is: **for artifacts (issue/feature/design/review), edit the file**; the graph view may lag and is best refreshed via `gid_extract` / re-backfill workflows.

## Notes

- After this fix, the original ISS-050 collision is structurally impossible: `engram` and `gid-rs` each scope their `next_id("issue")` to their own `issues/` dir; cross-project refs always carry the project name.
- ISS-051 / ISS-052 (ritual fixes) do not depend on this; they proceed in parallel.
- Generalization comes from removing closed sets in code (no `ArtifactKind` enum, no fixed parent tree) and putting variation in data (`layout.yml`, frontmatter, file paths). Same pattern as rustclaw channels (capabilities-driven) and skills (frontmatter-driven).
