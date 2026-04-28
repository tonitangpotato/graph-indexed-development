# Design Review r1 — ISS-053 (Project artifacts model)

> **Reviewer:** claude-code sub-agent
> **Date:** 2026-04-28
> **Target:** `.gid/issues/ISS-053/issue.md`
> **Requirements:** (embedded in issue — constraints §1, acceptance criteria §6)
> **Method:** 36-check review-design skill, depth=full

## Summary

| Severity   | Count |
|------------|-------|
| Critical   | 3     |
| Important  | 5     |
| Minor      | 3     |
| **Total**  | **11**|

**Recommendation:** Needs fixes before implementation — the 3 critical findings (undefined types, name collision, undefined pattern DSL grammar) would cause implementation to stall or diverge. The important findings (separator convention, missing Layout fields, AC testability, migration gaps) should be resolved to avoid rework.

---

## FINDING-1 🔴 Critical — Undefined types: `SlotMap`, `MetaSourceHint`, `RelationIndex`

**Check:** #1 (Every type fully defined?)
**Section:** §4.4 (Layout), §4.6 (ArtifactStore)

Three types are used but never defined anywhere in the document:

1. **`SlotMap`** — used in `ArtifactStore::next_path(&self, kind: &str, parent: Option<&ArtifactRef>, slot_overrides: &SlotMap) -> Result<PathBuf>`. What is `SlotMap`? A `HashMap<String, String>`? Does it map placeholder names to values? No definition, no field listing.

2. **`MetaSourceHint`** — used in `LayoutPattern::metadata_format: MetaSourceHint` and `FallbackRule::metadata_format: MetaSourceHint`. Is this the same as `MetaSource`? Different? It's a separate name, suggesting different semantics (a "hint" for `create` vs the actual parsed source), but the type is never defined.

3. **`RelationIndex`** — used as `Mutex<RelationIndex>` in `ArtifactStore`. What fields does it contain? How is it structured for efficient `relations_from` / `relations_to` queries? This is the entire caching layer and it's undefined.

Additionally, **`Layout::relation_fields()`** is called in §4.5 ("Frontmatter fields matching `Layout::relation_fields()`") but is not listed in the Layout struct definition or methods.

**Suggested fix:** Add definitions:
```rust
pub type SlotMap = BTreeMap<String, String>;  // placeholder_name → value

pub enum MetaSourceHint {
    Yaml,
    MarkdownHeaders,
    Infer,  // auto-detect on read
}

pub struct RelationIndex {
    forward: HashMap<ArtifactRef, Vec<Relation>>,  // from → [relations]
    reverse: HashMap<ArtifactRef, Vec<Relation>>,  // to → [relations]
    built_at: SystemTime,
}
```
And add `relation_fields()` to `Layout`'s API definition.

---

## FINDING-2 🟡 Important — Short-form reference separator conflicts with existing convention

**Check:** #4 (Consistent naming) + #32 (Conflicts with existing architecture)
**Section:** §4.1 (ArtifactRef), §10

The design introduces `project/kind/id` as the short-form reference format (e.g., `"engram/ISS-022"`, `"engram/feature/dim-extract"`). However, the existing `project_registry.rs` (line 26) already documents the convention as `project:issue` format (colon-separated, e.g., `engram:ISS-022`).

The canonical form also uses colon: `"engram:.gid/issues/ISS-022/issue.md"`.

This creates an ambiguity: is the separator `:` or `/`? Currently:
- Canonical = colon: `engram:.gid/issues/ISS-022/issue.md`
- Short form = slash: `engram/ISS-022`

The slash in short forms collides with filesystem path separators, making it hard to distinguish `engram/ISS-022` (a short-form reference) from a relative file path.

**Suggested fix:** Unify on colon as the project separator throughout:
- Canonical: `engram:.gid/issues/ISS-022/issue.md` (already uses colon)
- Short: `engram:ISS-022` (consistent with existing project_registry convention)
- Nested short: `engram:feature/dim-extract/review/design-r1`

This preserves backward compatibility with the established convention and avoids path/reference ambiguity.

---

## FINDING-3 🟡 Important — `Metadata::parse` fallback behavior underspecified for malformed YAML

**Check:** #7 (Error handling completeness) + #21 (Ambiguous prose)
**Section:** §4.2 (Metadata)

`Metadata::parse` is documented as: "Tries YAML → markdown headers → None. Never errors on unknown formats; returns MetaSource::None."

Ambiguity: What happens when a file has `---` delimiters (looks like YAML frontmatter) but the content between them is malformed YAML? Two possible behaviors:

1. **Strict first-match:** Detect `---` delimiters → attempt YAML parse → YAML parse fails → **error** (frontmatter detected but unparseable).
2. **Graceful fallback:** Detect `---` delimiters → attempt YAML parse → YAML parse fails → fall through to markdown headers → fall through to None.

Option 1 is safer (surfaces typos in frontmatter). Option 2 matches the "never errors" guarantee but silently swallows broken frontmatter — a user with `stauts: open` (typo for `status`) gets `MetaSource::None` and loses all their metadata silently.

This matters because constraint #1 says "humans edit with vim" — typos in frontmatter are expected.

**Suggested fix:** Specify explicitly:
- If `---` delimiters are present → commit to YAML parsing. Malformed YAML → return `MetaSource::YamlFrontmatter` with successfully parsed fields (partial parse) + warning list. Never silently fall through to a different parser.
- If no `---` delimiters → try markdown headers → None.
- This matches the "tolerant" philosophy while not silently losing data.

---

## FINDING-4 🟢 Minor — Sequence overflow behavior for `{seq:NN}` unspecified

**Check:** #9 (Integer overflow)
**Section:** §4.4 (Layout, placeholder vocabulary)

`{seq:04}` is documented as "zero-padded integer" but the behavior when the sequence exceeds the pad width is unspecified. At ISS-9999, the next ID would be either:
- `ISS-10000` (min-width padding, 5 digits — breaks sorting assumptions)
- Error (exact-width enforcement)

For most projects this is theoretical (>9999 issues), but the design should specify: is `04` a minimum width (like `printf %04d`) or an exact width?

**Suggested fix:** Add to §4.4 placeholder vocabulary: "`{seq:NN}` — zero-padded to minimum NN digits (like `printf %0Nd`). Sequences exceeding NN digits are not truncated."

---

## FINDING-5 🟡 Important — `Layout` struct missing `relation_fields` configuration

**Check:** #15 (Configuration vs hardcoding) + #1 (Type fully defined)
**Section:** §4.4 (Layout) + §4.5 (Relation discovery)

§4.5 says: "Frontmatter fields matching `Layout::relation_fields()` (default: `related`, `blocks`, `blocked_by`, `supersedes`, `derives_from`, `applies_to`, `references`)"

This implies `relation_fields` is a method on `Layout` that returns a configurable list. But the `Layout` struct only has two fields: `patterns: Vec<LayoutPattern>` and `fallback: FallbackRule`. There's no `relation_fields` field, no method defined, and no indication that `.gid/layout.yml` can configure which frontmatter keys are treated as relations.

This means either:
1. The list is hardcoded (contradicts the data-over-code philosophy of D2), or
2. It should be a `Layout` field but was omitted from the struct definition.

**Suggested fix:** Add to `Layout`:
```rust
pub struct Layout {
    patterns: Vec<LayoutPattern>,
    fallback: FallbackRule,
    /// Frontmatter field names treated as relation references.
    /// Default: ["related", "blocks", "blocked_by", "supersedes", 
    ///           "derives_from", "applies_to", "references"]
    relation_fields: Vec<String>,
}
```
And document that `.gid/layout.yml` can override `relation_fields`.

---

## FINDING-6 🔴 Critical — Layout pattern DSL grammar is undefined

**Check:** #21 (Ambiguous prose)
**Section:** §4.4 (Layout, LayoutPattern)

The `LayoutPattern::pattern` field uses a custom pattern language (`"issues/{id:ISS-{seq:04}}/issue.md"`) but the grammar is specified only by examples. Two engineers would implement this differently because:

1. **Nested placeholders:** `{id:ISS-{seq:04}}` — is `id` a capture group name and `ISS-{seq:04}` a sub-pattern? Or is the colon a type annotation? The pattern `{id:ISS-{seq:04}}` contains a nested `{}` — how is this parsed?

2. **Segment boundaries:** Does `{slug}` match only within a single path segment (like `/[^/]+/`) or can it span `/`? The examples suggest single-segment, but it's not stated.

3. **`{any}` semantics:** "catch-all" — does this match one segment or the rest of the path (like `**` in glob)?

4. **Matching vs generation:** The same pattern is used for both matching (identify kind from path) and generation (`next_path`). The matching direction needs `{seq:04}` to accept any 4+ digit number; the generation direction needs it to produce the next sequential number. This bidirectional usage requires a formal grammar.

5. **Escaping:** Can literal `{` and `}` appear in patterns? What about `.` — is it regex-special or literal?

**Suggested fix:** Add a formal grammar section:
```
pattern     ::= segment ('/' segment)*
segment     ::= (literal | placeholder)+
placeholder ::= '{' name (':' constraint)? '}'
constraint  ::= literal | seq_spec | placeholder  // if nesting allowed
seq_spec    ::= 'seq:' DIGITS
name        ::= IDENT
literal     ::= [^{}/]+
```
Specify: `{slug}` and `{name}` match `[^/]+` (single segment). `{any}` matches `[^/]+` (single segment) or `.*` (multi-segment)? Define explicitly.

---

## FINDING-7 🟢 Minor — Missing helper definitions: `merge_list`, Layout resolution method

**Check:** #22 (Missing helpers)
**Section:** §4.5 (Relation "manual relate"), §4.4 (Layout)

1. **`merge_list(existing, B)`** — used in `gid artifact relate` flow: `Metadata::set_field("blocks", merge_list(existing, B))`. This function is not defined. Semantics unclear: does it append and deduplicate? Preserve order? What if `B` is already in the list?

2. **Layout path resolution** — `ArtifactRef::parse` "Short forms require a Layout to resolve" but there's no explicit `Layout::resolve_short(s: &str) -> Result<PathBuf>` or equivalent method defined on Layout.

**Suggested fix:** 
- Define `merge_list`: "Appends `value` to `existing` list if not already present. Preserves original ordering."
- Add `Layout::resolve(&self, short: &str) -> Result<PathBuf>` to the Layout API (or clarify it's internal to `ArtifactRef::parse`).

---

## FINDING-8 🔴 Critical — `ArtifactRef` name collision with existing `ritual::definition::ArtifactRef`

**Check:** #26 (Existing functionality duplication) + #29 (Ground truth)
**Section:** §4.1 (ArtifactRef)

**Verified in source:** `gid-core/src/ritual/definition.rs:162` already defines `pub struct ArtifactRef` with fields `from_phase: Option<String>` and `path: String`. The existing type represents inter-phase artifact flow in rituals — completely different semantics from the ISS-053 `ArtifactRef` (project identity).

Both are `pub` types in `gid-core`. Adding a second `pub struct ArtifactRef` in a new module creates:
1. **Import ambiguity** — any file using both ritual artifacts and project artifacts must alias one.
2. **Grep confusion** — searching for `ArtifactRef` hits both, making maintenance harder.
3. **Semantic confusion** — same name, different meanings.

`ritual::artifact.rs` (306 lines) also has `ArtifactManager` which manages file resolution — overlapping with some `ArtifactStore` responsibilities.

**Suggested fix:** Rename the ISS-053 type to avoid collision. Options:
- `ProjectArtifactRef` (explicit but verbose)
- `ArtifactId` (shorter, emphasizes identity role)
- `ArtifactPath` (emphasizes that the ID *is* the path — aligns with D1)

The existing ritual `ArtifactRef` should not be renamed (it has callers).

---

## FINDING-9 🟡 Important — D2 scalability test (§6) is incomplete as a binding acceptance test

**Check:** #6 AC testability (special focus area #1 + #2)
**Section:** §6 (Scalability test)

The D2 binding test is:
```
gid artifact new --kind postmortem --title "..."
gid artifact list --kind postmortem
gid artifact show <ref>
gid artifact relate <pm-ref> applies-to engram/ISS-022
```

Issues:
1. **Missing layout.yml content.** The test says "Add `kind: postmortem` purely via `.gid/layout.yml`" but doesn't show the actual YAML that must be added. Without specifying the pattern (e.g., `postmortems/{id:PM-{seq:03}}/postmortem.md`), the test is ambiguous — what path structure should `gid artifact new --kind postmortem` create?

2. **Missing verification step.** After `relate`, the test should verify: `gid artifact refs engram/ISS-022` returns the postmortem relation. Without this, the test doesn't prove the relation was actually persisted.

3. **No negative test.** The binding claim is "gid-core is unmodified." How is this verified in CI? A test that runs on unmodified gid-core source? Or just a human assertion?

4. **Doesn't test `next_id` for the new kind.** If postmortem uses `{seq}`, does sequence allocation work for a kind that has zero existing artifacts?

**Suggested fix:** Expand the test to include:
```yaml
# .gid/layout.yml addition:
- pattern: "postmortems/{id:PM-{seq:03}}/postmortem.md"
  kind: postmortem
  metadata_format: yaml
  id_template: "PM-{seq:03}"
  seq_scope: project
```
Add verification: `gid artifact refs engram/ISS-022 | grep postmortem`.
Add: "This test runs in CI against an unmodified gid-core checkout with only layout.yml changed."

---

## FINDING-10 🟡 Important — Several ACs in §6 are not automatically testable

**Check:** AC testability (special focus area #2)
**Section:** §6 (Acceptance criteria)

The following ACs lack deterministic, automatable test specifications:

1. **"Metadata::parse round-trips: read → no-op set_field → render → byte-identical"** — Good as stated, but needs a test corpus specified. Which files? The design should say "tested against all `.md` files in engram/.gid/ + gid-rs/.gid/ + rustclaw/.gid/ corpora" (as §7 phase 5 implies but §6 doesn't bind).

2. **"resolve('engram/ISS-022') works regardless of cwd"** — How? This requires `~/.config/gid/projects.yml` to exist with engram registered. Is this a fixture or a real-environment test? CI environments won't have this file.

3. **"find_references_to discovers all four RelationSource types"** — This needs a fixture directory with known artifacts containing each source type. No fixture is specified.

4. **Wrapper ACs ("CLI commands implemented", "MCP tools implemented")** — These are existence checks, not behavioral tests. What does "implemented" mean — compiles? Returns correct output for a known input?

**Suggested fix:** For each AC, specify: (a) input fixture, (b) exact command, (c) expected output. Example: "Given fixture dir `tests/fixtures/iss053/` containing `issues/ISS-001/issue.md` with frontmatter `related: [ISS-002]` and `issues/ISS-002/issue.md`, then `find_references_to(ISS-002)` returns exactly one Relation with `source: Frontmatter { field: 'related' }`."

---

## FINDING-11 🟡 Important — §7 Migration lacks rollback path and `.gid/reviews/` top-level directory coverage

**Check:** #24 (Migration path) + #29 (Ground truth) — special focus area #3
**Section:** §7 (Migration)

1. **No rollback plan.** §7 says "no file moves, no renames" which is good, but if the new Layout's default patterns don't correctly match all existing files, artifacts become invisible (caught only by fallback → `kind: note`). There's no `gid artifact lint --check-layout-coverage` that verifies all existing `.gid/*.md` files match a non-fallback pattern. §9 Risk #1 mentions this but the mitigation ("derive defaults empirically") is a build-time check, not a runtime rollback.

2. **Top-level `.gid/reviews/` not covered by default Layout.** Ground truth verified: `gid-rs/.gid/reviews/` contains 18 review files (e.g., `frictionless-graph-design-r3-review.md`) at the top level — NOT under `features/` or `issues/`. The §2 "Reality on disk" section only shows reviews under `issues/` and `features/` subdirectories. These top-level reviews would fall through to the fallback rule (`kind: note`) instead of being recognized as reviews.

3. **`.gid/sqlite-migration/` and `.gid/incremental-extract/` directories** contain design docs outside the `features/` or `issues/` hierarchy. These also need Layout coverage or explicit acknowledgment that they'll be `kind: note`.

**Suggested fix:**
- Add top-level `.gid/reviews/{name}.md` pattern to default Layout (maps to `kind: review`).
- Add `.gid/{slug}/DESIGN.md` or similar pattern for ad-hoc subdirectories, or explicitly document that these become `kind: note` and that's acceptable.
- Add a migration verification step: "Run `gid artifact list` against engram/gid-rs/rustclaw corpora; verify no artifact that should be `kind: review` or `kind: design` falls to `kind: note`."

---

<!-- FINDINGS -->

## ✅ Passed Checks

- **Check #0**: Document size ✅ — 6 model components in §4, well under 8 limit.
- **Check #2**: References resolve ✅ — all §N cross-refs verified present. (Layout::relation_fields gap covered in FINDING-5.)
- **Check #3**: No dead definitions ✅ — all types used in ArtifactStore API or ACs.
- **Check #5**: State machine N/A ✅ — CRUD model, no state machine.
- **Check #6**: Data flow ✅ — parse→set_field→render chain is consistent; kind derived from path via Layout.
- **Check #8**: String operations ✅ — no unsafe string slicing in design.
- **Check #10**: Option/None ✅ — `get` returns `Result<Option<>>`, `parent` params are `Option`.
- **Check #11**: Match exhaustiveness ✅ — all enums are small, explicit.
- **Check #12**: Ordering sensitivity ✅ — relation discovery rules are independent; Metadata::parse order is intentional.
- **Check #13**: Separation of concerns ✅ — pure types (Metadata, Layout) separated from IO (ArtifactStore).
- **Check #14**: Coupling ✅ — kind derived not stored; relations reference by ArtifactRef not embedded.
- **Check #16**: API surface ✅ — 9 methods on ArtifactStore, all necessary; internals not exposed.
- **Check #17**: Goals/non-goals ✅ — §1 constraints are clear goals; §8 out-of-scope is explicit and comprehensive.
- **Check #18**: Trade-offs ✅ — D1–D5 each document the rejected alternative and rationale. §10 desync acceptance is well-reasoned.
- **Check #19**: Cross-cutting concerns ✅ — performance justified (<1000 artifacts); security N/A (local tool); error visibility via `gid artifact lint`.
- **Check #20**: Abstraction level ✅ — Rust struct signatures with method docs, not implementation bodies. Right level.
- **Check #23**: Dependencies ✅ — uses std types + `NaiveDate` (chrono implied). Acceptable for a Rust crate.
- **Check #25**: Testability ✅ — pure types (Metadata, Layout, ArtifactRef) are unit-testable without IO. ArtifactStore needs tempdir but that's standard.
- **Check #27**: API compatibility ✅ — new module, no existing callers broken (except name collision in FINDING-8).
- **Check #28**: Feature flag ✅ — new additive module, can be gated behind a cargo feature if desired. Not strictly necessary.
- **Check #30**: Technical debt ✅ — §10 explicitly documents the desync debt with (a) what: artifact↔graph drift, (b) why: sync is a larger design, (c) trigger: ISS-052. §9 Risk #5 placeholder vocabulary is similarly documented. No hidden debt.
- **Check #31**: Shortcut detection ✅ — design addresses root cause (no artifact model in gid-core) not symptom (ISS-050 collision). D1–D5 each solve the structural mistake from v0–v2.
- **Check #32**: Architecture conflicts — partially flagged in FINDING-2 (separator convention) and FINDING-8 (name collision). Error handling style (`Result<>`) matches existing gid-core patterns. ✅ for the rest.
- **Check #33**: Simplification ✅ — design handles full complexity: mixed metadata formats, arbitrary nesting, ad-hoc kinds. No edge cases dropped.
- **Check #34**: Breaking-change risk ✅ — new module is additive. §7 says "no file moves, no renames." Existing graph tools unchanged (§10).
- **Check #35**: Purpose alignment ✅ — all 6 types serve the stated goal. No speculative flexibility (no unused config knobs, no interfaces-with-one-impl). Sugar aliases are explicitly marked optional.

## Applied

(None — awaiting human approval before apply phase.)
