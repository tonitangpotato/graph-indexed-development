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
4. Model (ArtifactId / Metadata / Artifact / Layout / Relation / ArtifactStore)
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

`ArtifactId = (project, relative_path)`. The path is the source of truth on disk anyway. `kind` is **derived** from the path via Layout (§4), used as a label/filter, never as part of the ID.

Rationale: arbitrary nesting (review-of-split-design-part-1) needs unbounded depth. Path has it natively. Tuple flattening was the v2 mistake.

Short forms (`engram:ISS-022`, `engram:feature/dim-extract`) are sugar resolved by Layout. The separator between project and artifact id is `:` — aligning with the existing `project_registry.rs` convention (e.g., `engram:ISS-022`) and the canonical label format. The artifact id portion preserves its native format (e.g., `ISS-022`, `feature/dim-extract`, `feature/dim-extract/review/design-r1`); only the leading project segment uses `:`. Authoritative form is always the path.

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

### 4.1 ArtifactId — identity (D1)

> **Note (rename):** Earlier drafts called this `ArtifactRef`. Renamed to `ArtifactId` to avoid collision with the existing `gid_core::ritual::definition::ArtifactRef` (different semantics — inter-phase artifact flow). See §10 for the collision discussion.

```rust
pub struct ArtifactId {
    pub project: String,           // "engram"
    pub path: PathBuf,             // ".gid/issues/ISS-022/issue.md", relative to project root
}

impl ArtifactId {
    /// Canonical label: "<project>:<relative_path>"
    pub fn label(&self) -> String;

    /// Parse a reference. Accepts:
    ///   - canonical:  "engram:.gid/issues/ISS-022/issue.md"
    ///   - short:      "engram:ISS-022"        (Layout resolves to path)
    ///   - short:      "engram:feature/dim-extract"
    ///   - short:      "engram:feature/dim-extract/review/design-r1"
    /// The `:` separator between project and id aligns with the existing
    /// `project_registry.rs` convention. Short forms require a Layout to resolve.
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
    /// For non-frontmatter files, never errors; returns `MetaSource::None`.
    ///
    /// On success, returns `Ok((metadata, body))` where `body` is the file
    /// contents after the metadata block.
    ///
    /// **Malformed YAML:** if the file begins with `---` frontmatter delimiters
    /// but the content between them fails to parse as YAML, returns
    /// `Err(MetadataError::MalformedFrontmatter { line, message })` rather than
    /// silently falling through to the markdown-headers parser. Falling through
    /// would mask typos (e.g., `stauts: open` becoming `MetaSource::None`).
    /// Callers may downgrade this error to a warning + treat the file as having
    /// no frontmatter at their discretion. Specifically, `gid artifact lint`
    /// reports `MalformedFrontmatter` as a **warning, not an error**, so a single
    /// bad file does not block bulk operations like `gid artifact list`.
    pub fn parse(file_contents: &str) -> Result<(Self, /* body */ String), MetadataError>;

    /// Construct empty metadata for a brand-new artifact. The `hint` selects
    /// which format `render()` will emit (frontmatter vs. markdown headers vs.
    /// none). `fields` is empty; `raw` is empty (no round-trip source yet).
    /// Caller populates fields via `set_field` before `ArtifactStore::create`
    /// renders + writes the file.
    pub fn new(source_hint: MetaSourceHint) -> Self;

    /// Replace one field's value, rewriting only the affected line(s).
    /// Other lines (including whitespace, comments, key order) preserved byte-exact.
    pub fn set_field(&mut self, key: &str, value: MetaValue);

    /// Emit metadata block in original MetaSource format. If MetaSource::None,
    /// caller must specify a target source.
    pub fn render(&self) -> String;
}

/// Errors produced by `Metadata::parse`. Non-frontmatter parse paths are
/// infallible (return `MetaSource::None`); only malformed `---`-delimited
/// YAML frontmatter raises an error.
pub enum MetadataError {
    /// File begins with `---` but the YAML between delimiters failed to parse.
    /// `line` is 1-indexed within the file; `message` is the underlying parser
    /// diagnostic (verbatim from `serde_yaml` / equivalent).
    MalformedFrontmatter { line: usize, message: String },
    /// I/O failure while reading the file (only raised by callers that wrap
    /// `parse` with file IO, e.g., `ArtifactStore::open` / `get`).
    IoError(std::io::Error),
}
```

Surgical field edits go through `set_field`. Whole-block reformatting requires explicit `normalize()` (not in v1 of this issue's scope).

### 4.3 Artifact — file + parsed metadata + body

```rust
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: String,                // derived via Layout from id.path
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
    /// Frontmatter fields whose values (string or list of strings) are
    /// interpreted as `ArtifactId` references for relation discovery.
    /// Default: ["related", "blocks", "blocked_by", "supersedes",
    ///           "derives_from", "applies_to", "references"].
    /// Overridable via `.gid/layout.yml`.
    relation_fields: Vec<String>,
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
    pub seq_scope: SeqScope,                // Project | Parent
    // Note: there is no separate `id_template` field. The ID template for a
    // sequenced kind is encoded inside the pattern via `{id:TEMPLATE}` (see
    // §4.4.1). `LayoutPattern::id_template_str()` extracts it from `pattern`
    // when `next_id` needs to render a bare ID without a full path.
}

pub enum SeqScope {
    Project,                 // ISS-N counts within whole project
    Parent { rel: String },  // r-N counts within parent dir
}

pub struct FallbackRule {
    pub kind: String,                       // "note"
    pub metadata_format: MetaSourceHint,
}

// Newly defined supporting types (used by ArtifactStore and Layout):

/// Slot name → captured value. Populated by Layout pattern matching
/// (e.g., {"id" → "ISS-0042", "seq" → "0042", "slug" → "dim-extract"})
/// and consumed by `Layout::resolve` for path rendering.
pub type SlotMap = BTreeMap<String, String>;

/// Hint to the renderer about which metadata format to write.
/// Distinct from `MetaSource`: `MetaSource` records what was *parsed*;
/// `MetaSourceHint` declares what should be *produced* on `create`.
pub enum MetaSourceHint {
    Frontmatter,        // emit YAML `---` block
    KeyValue,           // emit `**Field**: value` markdown headers
    None,               // no metadata block
}

/// Incoming-relation index, built lazily from artifact contents.
/// Keyed by the *target* of each relation, so `relations_to(target)` is O(1).
/// Forward direction (`relations_from`) is computed on demand from the
/// artifact file itself; no separate forward index is cached.
pub type RelationIndex = BTreeMap<ArtifactId, Vec<Relation>>;
```

Layout API methods:

```rust
impl Layout {
    /// Frontmatter field names treated as relation references.
    /// Returns the configured `relation_fields` slice.
    pub fn relation_fields(&self) -> &[String];

    /// Render a concrete relative path from a kind + slot values.
    /// Inverse of pattern matching: given `kind="issue"` and
    /// `slots = {"id" → "ISS-0042", "seq" → "0042"}`, returns
    /// `".gid/issues/ISS-0042/issue.md"`.
    /// Errors if `kind` matches no pattern, required slots are missing, or a
    /// `{seq:NN}` would overflow.
    pub fn resolve(&self, kind: &str, slots: &SlotMap) -> Result<PathBuf, LayoutError>;
}

/// Errors produced by `Layout::resolve` and the sequence-allocation paths
/// (`ArtifactStore::next_id` / `next_path`).
pub enum LayoutError {
    /// `kind` did not match any `LayoutPattern` in the layout.
    UnknownKind(String),
    /// A required slot was not present in the `SlotMap`. `kind` is the kind
    /// being rendered; `slot` is the missing placeholder name.
    MissingSlot { kind: String, slot: String },
    /// `{seq:NN}` would allocate a value past `10^NN - 1`. Widen the layout
    /// (e.g., bump `seq:04` → `seq:05`) before creating new artifacts.
    SeqExhausted { pattern: String, max: u64 },
}

/// Order-preserving union of two string lists. Used by `Metadata::set_field`
/// when the field is a list-typed relation field, and by
/// `gid artifact relate` to merge a new target into an existing list:
///   merge_list(["A", "B"], ["B", "C"]) == ["A", "B", "C"]
/// Duplicates from either side are dropped; first occurrence wins.
pub fn merge_list(existing: &[String], new: &[String]) -> Vec<String>;
```

#### Default layout (built into gid-core via `include_str!`)

Covers today's reality empirically (issue / feature / design / requirements / review / investigation / handoff / verify-report / pivot-note). Projects can override by writing `.gid/layout.yml`.

#### Adding a new kind

Edit `.gid/layout.yml`, add a pattern. No code change. **This is the test for D2.**

#### Placeholder vocabulary (closed set, documented)

`{seq:NN}` — zero-padded integer, scope per `seq_scope`. `{slug}` — kebab-case identifier. `{name}` — free-form basename. `{parent_id}` — captured parent path segment. `{any}` — catch-all. **No user-defined placeholders in v1.** Future kinds needing `{year}-{seq}` etc. extend this list — that *is* a gid-core change, but adding placeholders is rare and additive (doesn't break existing layouts). Documented in Risks (§9).

#### 4.4.1 Pattern DSL grammar

Patterns are bidirectional: the **same** pattern string is used both for matching paths (deriving `kind` + slot captures from a path on disk) and for rendering paths (`Layout::resolve` produces a path from `kind` + `SlotMap`). A formal grammar is required so matcher and renderer agree on edge cases.

```ebnf
pattern     ::= segment ('/' segment)*
segment     ::= (literal | placeholder)+
placeholder ::= '{' name (':' constraint)? '}'
constraint  ::= literal_text | seq_spec | id_template
seq_spec    ::= 'seq:' DIGITS                        (* e.g., seq:04 *)
id_template ::= (literal_text | '{' 'seq:' DIGITS '}')+   (* e.g., ISS-{seq:04} *)
name        ::= [a-z_][a-z0-9_]*
literal      ::= [^{}/]+
literal_text ::= [^{}/]+
```

**Token semantics (within one path segment, i.e., between `/`):**

| Token             | Matches                              | Generates                             |
|-------------------|--------------------------------------|---------------------------------------|
| `{slug}`          | `[a-z0-9_-]+`                        | caller-supplied via `SlotMap`         |
| `{name}`          | `[^/]+` (free-form basename)         | caller-supplied                       |
| `{parent_id}`     | `[^/]+` (captures parent dir name)   | caller-supplied                       |
| `{any}`           | `[^/]+` (single segment, catch-all)  | caller-supplied                       |
| `{seq:NN}`        | `\d{NN,}` (≥ NN digits, zero-padded) | next sequence integer, zero-padded NN |
| `{id:TEMPLATE}`   | a structured ID matching TEMPLATE    | renders TEMPLATE with its slots       |

**Nesting rule:** `{id:...}` may contain `{seq:NN}` and literal text only. No other slot types nest. Example: `{id:ISS-{seq:04}}` is legal; `{id:{slug}-{seq:04}}` is not.

**Example (bidirectional):**

Pattern: `issues/{id:ISS-{seq:04}}/issue.md`

| Direction | Input                           | Output                                     |
|-----------|---------------------------------|--------------------------------------------|
| Match     | `issues/ISS-0042/issue.md`      | `slots = {id: "ISS-0042", seq: "0042"}`    |
| Render    | `slots = {seq: "0042"}` (or auto-allocated) | `issues/ISS-0042/issue.md`     |

**Render precedence for `{id:TEMPLATE}`:** when rendering, `{id:TEMPLATE}` is
resolved first by looking up the `id` slot in the `SlotMap`. If `id` is
present, its rendered form is used verbatim. If `id` is absent, the template
is evaluated by recursively resolving its inner placeholders (currently only
`{seq:NN}` and literal text), with `seq` looked up in the `SlotMap` and
zero-padded to width NN. If both `id` and `seq` are present in slots and `id`
already starts with the rendered seq, the seq is consumed by the id template;
otherwise (neither `id` nor a usable `seq` is supplied) it's an error
(`LayoutError::MissingSlot`).

**Escaping:** Literal `{` and `}` cannot appear in patterns (no escape mechanism in v1). `.` is treated as a literal character (not regex-special). Path separators are always `/`.

**Sequence overflow:** If `ArtifactStore::create` (or `next_id` / `next_path`) would allocate a `{seq:NN}` value that exceeds `10^NN - 1` (e.g., `seq:04` past `9999`), it returns `Err(LayoutError::SeqExhausted { pattern, max })`. Callers must widen the layout (e.g., bump `seq:03` → `seq:04`) before creating new artifacts. **No silent rollover** — a 5-digit ID under a `seq:04` pattern would break sort order assumptions and round-trip matching.

**Match precedence:** Patterns are tried in declaration order; first match wins. The fallback rule is consulted only if no pattern matches.

### 4.5 Relation — derived from artifact contents (D3)

```rust
pub struct Relation {
    pub from: ArtifactId,
    pub to: ArtifactId,
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

1. **Frontmatter fields** matching `Layout::relation_fields()` (default: `related`, `blocks`, `blocked_by`, `supersedes`, `derives_from`, `applies_to`, `references`). Each value is parsed as an ArtifactId.
2. **Markdown links** of the form `[anything](relative_or_absolute_path_to_a_.md_under_.gid/)`. URL fragments and external URLs ignored.
3. **Inline backtick refs** matching `` `<project>/...` `` or `` `<short_form>` `` parseable by `ArtifactId::parse`.
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
    /// Loads each matching file, calling `Metadata::parse` and propagating
    /// `MetadataError` via `?`. A malformed-frontmatter file surfaces as an
    /// error to the caller; `gid artifact list` downgrades to a warning + skip
    /// (per §4.2).
    pub fn list(&self, kind_filter: Option<&str>) -> Result<Vec<Artifact>>;
    /// Reads the file at `id.path`, then `let (meta, body) = Metadata::parse(&contents)?;`.
    pub fn get(&self, id: &ArtifactId) -> Result<Option<Artifact>>;

    // Allocate.
    pub fn next_id(&self, kind: &str, parent: Option<&ArtifactId>) -> Result<String>;
    pub fn next_path(&self, kind: &str, parent: Option<&ArtifactId>, slot_overrides: &SlotMap) -> Result<PathBuf>;

    // Write (all are file writes; D3 means relate also writes a file).
    /// New artifacts: caller builds metadata via `Metadata::new(hint)` + a
    /// sequence of `set_field` calls, then passes it here. `create` renders
    /// metadata + body, writes via `tempfile + rename` (atomic), refuses
    /// overwrites.
    pub fn create(&self, path: &Path, metadata: Metadata, body: &str) -> Result<Artifact>;
    pub fn update(&self, artifact: &Artifact) -> Result<()>;

    // Relations (read-only; "manual relate" goes through update of source artifact).
    pub fn relations_from(&self, id: &ArtifactId) -> Result<Vec<Relation>>;
    pub fn relations_to(&self, id: &ArtifactId) -> Result<Vec<Relation>>;
}

// Cross-project resolver via ProjectRegistry (D5).
pub fn resolve(id: &ArtifactId) -> Result<Artifact>;
pub fn find_references_to(target: &ArtifactId) -> Result<Vec<Relation>>;
```

Every operation is **kind-agnostic at the type level**. Behavior differences live in `Layout`.

#### Concurrency model

- `&self` for all public methods. `index: Mutex<RelationIndex>` provides
  interior mutability for the lazy cache, so query methods (`relations_from`,
  `relations_to`) can rebuild on mtime change without `&mut self`.
- Daemon usage: wrap `ArtifactStore` in `Arc<...>` at the call site. gid-core
  does not impose a sharing model.
- Index invalidation: each query checks dir mtime; if newer than cache,
  rebuild. Cheap (<1000 artifacts per project).
- **Workload shape:** read-mostly (list/show/refs dominate; create/update are
  rare, human-driven). The `RelationIndex` is the only in-memory cache; it is
  rebuilt from files, never persisted, so there is no cross-process cache
  invalidation problem.
- **Atomic writes:** `create` / `update` write via `tempfile` in the target
  directory followed by `rename(2)` onto the final path — readers either see
  the old file or the new file, never a torn write.
- **Concurrent edits to the same file:** last-writer-wins. Acceptable because
  artifact files are human-edited markdown with low contention; conflicts are
  resolved by git, not by gid-core. There is no advisory locking.
- **No separate index to keep in sync:** D3 (relations derived from files)
  means writing an artifact file is the only mutation — there is no
  `relations.db` that could disagree with disk after a crash mid-write.


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

- [ ] `gid_core::{Artifact, ArtifactId, Metadata, MetaSource, MetaValue, Layout, LayoutPattern, Relation, RelationSource, ArtifactStore, resolve, find_references_to}` all exist and are public.
- [ ] Default `Layout` ships built-in (via `include_str!`); `.gid/layout.yml` override is optional.
- [ ] `ArtifactId::parse` accepts canonical, short (kind/id), short (kind/slug), and nested-short forms.

### Functional

- [ ] `Metadata::parse` round-trips: read -> no-op `set_field` for an existing key with the same value -> render -> byte-identical to original.
  - **Test:** Fixture `tests/fixtures/iss053/roundtrip-corpus/` containing files copied verbatim from `engram/.gid/`, `gid-rs/.gid/`, and `rustclaw/.gid/` (issue.md, design.md, requirements.md, review files, ad-hoc files). For each fixture file `F`: `let (m, body) = Metadata::parse(read(F))?; m.set_field(first_key, m.fields[first_key].clone()); assert_eq!(read(F), m.render() + body)`. Expected: byte-identical for 100% of fixture files. CI fails on any non-match with a unified diff.
- [ ] `Metadata::set_field` for a new key inserts in original `MetaSource` style.
- [ ] `ArtifactStore::next_id` returns project-scoped ISS-{seq} for issues, parent-scoped r-{seq} for reviews; errors clearly for slug kinds without caller-supplied slot.
- [ ] `ArtifactStore::create` is mkdir-atomic and refuses overwrites.
- [ ] Two stores for different projects can both have `ISS-050` without conflict (regression test for the original collision).
- [ ] `resolve("engram:ISS-022")` works regardless of cwd, via ProjectRegistry.
  - **Test:** Fixture `tests/fixtures/iss053/projects.yml` registering `engram` → `tests/fixtures/iss053/engram-root/` (which contains `.gid/issues/ISS-022/issue.md`). Set env var `GID_PROJECTS_YML=tests/fixtures/iss053/projects.yml` to mock `~/.config/gid/projects.yml`. Run from cwd `/tmp` (outside any project): `resolve(&ArtifactId::parse("engram:ISS-022", &layout)?)`. Expected: `Ok(Artifact { kind: "issue", id: ArtifactId { project: "engram", path: ".gid/issues/ISS-022/issue.md" }, ... })`.
- [ ] `find_references_to` discovers relations from all four discovery rules in §4.5 (frontmatter, markdown link, inline backtick, directory nesting), mapped onto the three `RelationSource` variants (`Frontmatter`, `MarkdownLink`, `DirectoryNesting`; backtick refs share the `MarkdownLink` discriminant).
  - **Test:** Fixture `tests/fixtures/iss053/relations/` with one file per discovery rule, all pointing at the same target `ISS-002`:
    - `frontmatter.md` — frontmatter `related: [ISS-002]` → expects `RelationSource::Frontmatter { field: "related" }`.
    - `markdown-link.md` — body contains `[see](.gid/issues/ISS-002/issue.md)` → expects `RelationSource::MarkdownLink`.
    - `backtick.md` — body contains `` `engram:ISS-002` `` → expects `RelationSource::MarkdownLink` (via inline-backtick rule §4.5 rule 3).
    - `reviews/of-ISS-002.md` (placed under a parent dir for `ISS-002`) → expects `RelationSource::DirectoryNesting`.
    Run `find_references_to(ISS-002)`. Expected: 4 relations returned, one per source type. Test asserts the set of `source` discriminants equals `{Frontmatter, MarkdownLink, DirectoryNesting}` (markdown-link and backtick share the `MarkdownLink` discriminant; assert at least one of each was discovered by inspecting which fixture file was the `from`).
- [ ] `gid artifact relate A blocks B` modifies A's frontmatter; subsequent `find_references_to(B)` returns the new edge; no `relations.yml`/`.db` is created.

### Scalability test (D2 — the binding test)

- [ ] Add `kind: postmortem` purely via `.gid/layout.yml` (no Rust changes). Run:
  ```
  gid artifact new --kind postmortem --title "..."
  gid artifact list --kind postmortem
  gid artifact show <ref>
  gid artifact relate <pm-ref> applies-to engram:ISS-022
  ```
  All succeed. **gid-core is unmodified.** This test failing means the design failed.

  **Test fixture:** `tests/fixtures/iss053/postmortem-extension/` containing a custom `.gid/layout.yml` that adds the `postmortem` kind:

  ```yaml
  # .gid/layout.yml addition:
  patterns:
    - pattern: "postmortems/{id:PM-{seq:03}}/postmortem.md"
      kind: postmortem
      metadata_format: frontmatter
      seq_scope: project
  ```

  **Verification (after the four commands above):**
  - `gid artifact list --kind postmortem` returns at least one row.
  - `gid artifact refs engram:ISS-022 | grep postmortem` returns at least one row (proves the `applies-to` relation was persisted in the postmortem's frontmatter and is discoverable via reverse lookup).
  - `next_id` for the postmortem kind starts at `PM-001` (sequence allocation works for a kind with zero pre-existing artifacts).

  **CI binding:** This test runs in CI against an unmodified `gid-core` checkout — only `layout.yml` changes between the baseline and the test run. No Rust code changes, no recompile of `gid-core`. If the test passes only with code changes, the design has failed.

### Wrappers

- [ ] CLI commands (§5.1) implemented; sugar aliases optional.
  - **Test:** Given fixture `tests/fixtures/iss053/cli-smoke/` (a project root with `.gid/issues/ISS-001/issue.md`), run `gid artifact list --kind issue --project cli-smoke --json`. Expected stdout (JSON):
    ```json
    [{"project":"cli-smoke","path":".gid/issues/ISS-001/issue.md","kind":"issue","title":"..."}]
    ```
    Exit code 0. Run `gid artifact show cli-smoke:ISS-001 --json` and assert `.kind == "issue"` and `.id.path == ".gid/issues/ISS-001/issue.md"`.
- [ ] MCP tools (§5.2) implemented.
  - **Test:** MCP server fixture invokes `gid_artifact_list { "kind": "issue", "project": "cli-smoke" }`. Expected response shape: `{"content":[{"type":"text","text":"<JSON array matching CLI output above>"}]}`. One assertion per tool (6 tools total).
- [ ] rustclaw tools (§5.3) implemented.
  - **Test:** rustclaw native tool registry exposes `gid_artifact_list`, `gid_artifact_show`, `gid_artifact_new`, `gid_artifact_update`, `gid_artifact_relate`, `gid_artifact_refs`. For each, invoke with the same fixture and assert the JSON return value matches the MCP response (content-equal, modulo transport wrapping).

### Documentation

- [ ] `<project>/<short>` reference convention documented.
- [ ] Default layout documented (the patterns shipped in gid-core).
- [ ] Placeholder vocabulary (§4.4) documented as closed v1 set.
- [ ] Relation discovery rules (§4.5) documented precisely.

## 7. Migration

- **No file moves, no renames.** Existing `.gid/` layout becomes the default Layout. On-disk files unchanged.
- **No graph schema changes.** Issues/features that already exist as graph nodes (post-ISS-028 backfill) remain; this issue does not touch the graph layer.
- **Optional `gid artifact lint`**: surfaces bare `ISS-NNN` / unqualified slugs in markdown bodies; suggests project-prefixing. Lint only — never auto-rewrites human prose.

### Default Layout patterns (additions for ground-truth coverage)

In addition to the patterns implied by §2 (issues/, features/, their reviews/), the default Layout ships with:

- `.gid/reviews/{name}.md` → `kind: review` — covers **top-level** reviews (e.g., `gid-rs/.gid/reviews/frictionless-graph-design-r3-review.md`, ~18 such files in `gid-rs` alone). These would otherwise fall to the fallback rule.
- `.gid/{slug}/{any}.md` → `kind: note` — explicit fallback for ad-hoc design subdirs like `.gid/sqlite-migration/`, `.gid/incremental-extract/`. Acknowledged: docs in these dirs become `kind: note`, not `kind: design`. Acceptable in v1 because (a) these are uncommon, (b) projects can override via `.gid/layout.yml`, (c) the alternative (auto-detect by filename `DESIGN.md` etc.) is heuristic and brittle.

### Migration verification (Phase 0 — blocks rollout)

Before any of the phases below, run:

```
gid artifact list --json
```

against the `engram`, `gid-rs`, and `rustclaw` `.gid/` corpora. Verify:

- Zero artifacts that should be `kind: review` fall to `kind: note`.
- Zero artifacts that should be `kind: design` fall to `kind: note`.
- Zero artifacts that should be `kind: issue` / `kind: feature` / `kind: requirements` fall to `kind: note`.

Any miscategorization **blocks rollout** until the default Layout patterns are extended to cover the missing cases.

### Rollback path

Rollback: revert the `.gid/layout.yml` change (or, for the built-in defaults, revert the gid-core release that introduced them). Since this issue's constraint is **no files moved or renamed**, reverting `layout.yml` is sufficient — the on-disk artifact files are untouched. `ArtifactStore` is read-only at this stage (writes are limited to `create` / `update` / `relate`, which only ever touch artifact files, never a relations DB). No data corruption is possible from rolling back the layout config.

### Phasing

Single PR is too big. Phases:

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
- **Markdown-link relation discovery is heuristic.** False positives are worse than false negatives (a fake edge confuses graph reasoning more than a missed one). Mitigation: only match `[](...)` and backticked refs that successfully `ArtifactId::parse`; document the regex precisely.
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
