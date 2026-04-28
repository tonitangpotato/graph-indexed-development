# Design Review r2 — ISS-053 (Project artifacts model)

> **Reviewer:** claude-code sub-agent
> **Date:** 2026-04-28
> **Target:** `.gid/issues/ISS-053/issue.md` (§3–§10)
> **Prior review:** `.gid/issues/ISS-053/reviews/design-r1.md` (11 findings, all Applied)
> **Method:** 36-check review-design skill, depth=full

## Summary

| Severity   | Count |
|------------|-------|
| Critical   | 1     |
| Important  | 2     |
| Minor      | 5     |
| **Total**  | **8** |

**Verdict: APPROVED WITH NITS**

The r1 fixes were thorough — all 11 findings were applied correctly in substance. Only one critical issue remains (FINDING-1: `Metadata::parse` signature contradicts its error semantics), which is a leftover from an incomplete r1 fix. The important findings (FINDING-2: undefined types in `Layout::resolve`; FINDING-6: missing `Metadata::new` constructor) are gaps in the new surface area introduced by r1, not regressions.

The 5 minor findings are cosmetic or clarifying. The design is well-structured, internally consistent (modulo the findings above), and ready for implementation once the Critical and Important findings are resolved.

**Estimated implementation confidence:** High — the design specifies types, APIs, acceptance criteria with fixtures, and phasing. An implementer can start from §4 and work through the types sequentially.

---

## FINDING-1 🔴 Critical — `Metadata::parse` signature contradicts its error semantics

**Check:** #7 (Error handling completeness) + r1 FINDING-3 verification
**Section:** §4.2 (Metadata)

The r1 fix for FINDING-3 added `MalformedFrontmatter` error behavior to `Metadata::parse`:

> **Malformed YAML:** if the file begins with `---` frontmatter delimiters but the content between them fails to parse as YAML, returns `Err(MetadataError::MalformedFrontmatter { line, message })`

However, the Rust signature was **not updated** to reflect this:

```rust
pub fn parse(file_contents: &str) -> (Self, /* body */ String);
```

A bare tuple `(Self, String)` cannot return `Err(...)`. The FINDING-3 fix added the error *semantics* but not the *type change*. Two engineers would implement differently: one returning `Result<(Self, String), MetadataError>`, the other keeping the tuple and panicking/logging.

Additionally, `MetadataError` itself is never defined as a type. The design mentions `MetadataError::MalformedFrontmatter { line, message }` but there's no enum definition showing its variants.

**Suggested fix:**
1. Change the signature:
   ```rust
   pub fn parse(file_contents: &str) -> Result<(Self, /* body */ String), MetadataError>;
   ```
2. Add `MetadataError` enum definition:
   ```rust
   pub enum MetadataError {
       MalformedFrontmatter { line: usize, message: String },
   }
   ```
3. Or, if the "never errors" semantics for non-YAML formats should be preserved, split into `parse` (infallible for non-frontmatter) and `parse_frontmatter` (fallible). But the current doc is self-contradictory.

---

## FINDING-2 🟡 Important — Undefined types in `Layout::resolve` signature: `Kind`, `RelativePath`, `LayoutError`

**Check:** #1 (Every type fully defined?)
**Section:** §4.4 (Layout API methods)

The `Layout::resolve` method signature introduced by r1 FINDING-7 uses three types that are not defined:

```rust
pub fn resolve(&self, kind: &Kind, slots: &SlotMap) -> Result<RelativePath, LayoutError>;
```

1. **`Kind`** — The design's D2 decision explicitly says "Kind is a string, not an enum." Every other use in the doc says `kind: String` (e.g., `Artifact::kind: String`, `LayoutPattern::kind: String`, `ArtifactStore::next_id(&self, kind: &str, ...)`). Using `Kind` (capitalized, type-like) here contradicts D2 and introduces an undefined type. Should be `kind: &str`.

2. **`RelativePath`** — Not defined anywhere. Is this `PathBuf`? A newtype wrapper? The rest of the design uses `PathBuf` for paths (e.g., `ArtifactId::path: PathBuf`, `ArtifactStore::next_path` returns `Result<PathBuf>`). Should be `PathBuf` for consistency.

3. **`LayoutError`** — Referenced here and in the `SeqExhausted` variant (§4.4.1 sequence overflow), but never defined as an enum. Its variants are unclear beyond `SeqExhausted`. What about "required slots missing" (mentioned in the `resolve` docstring)?

**Suggested fix:**
1. Change `Kind` → `&str` (consistent with D2 and all other usage).
2. Change `RelativePath` → `PathBuf` (consistent with `ArtifactId::path` and `next_path`).
3. Define `LayoutError`:
   ```rust
   pub enum LayoutError {
       SeqExhausted { pattern: String, max: u64 },
       MissingSlot { slot_name: String, pattern: String },
       NoMatchingPattern { kind: String },
   }
   ```

---

## FINDING-3 🟢 Minor — §4.5 claims "four RelationSource types" but enum has three variants

**Check:** #4 (Consistent naming) + #11 (Match exhaustiveness)
**Section:** §4.5 (Relation discovery) + §6 (find_references_to AC)

§4.5 defines four discovery rules (frontmatter, markdown links, inline backtick refs, directory nesting), and §6 says "discovers all four `RelationSource` types." But `RelationSource` has only three variants: `Frontmatter`, `MarkdownLink`, `DirectoryNesting`. Inline backtick refs are silently mapped to `MarkdownLink`.

The AC test acknowledges this ("markdown-link and backtick share the `MarkdownLink` discriminant"), so the design is internally consistent at the test level. But the prose "four types" is misleading — there are four *discovery rules* but three *source types*.

**Suggested fix:** Either:
1. (Preferred) Add a fourth variant `InlineBacktickRef` to `RelationSource` — keeps the mapping 1:1 and makes provenance traceable, or
2. Change §4.5 and §6 to say "four discovery *rules*" and "three `RelationSource` *variants*" consistently.

---

## FINDING-4 🟢 Minor — `LayoutPattern::id_template` is redundant with pattern DSL `{id:TEMPLATE}`

**Check:** #3 (No dead definitions) + #35 (Purpose alignment)
**Section:** §4.4 (LayoutPattern struct) + §4.4.1 (Pattern DSL grammar)

`LayoutPattern` has:
```rust
pub id_template: Option<String>,  // for sequenced kinds: "ISS-{seq:04}"
```

But the same information is already embedded in the pattern string via the `{id:TEMPLATE}` DSL construct. For example, pattern `"issues/{id:ISS-{seq:04}}/issue.md"` already encodes the template `ISS-{seq:04}`. The `id_template` field duplicates this, creating a potential desync (what if the pattern says `{id:ISS-{seq:04}}` but `id_template` says `"ISS-{seq:03}"`?).

The only use case where `id_template` adds value is if some patterns don't use `{id:...}` but still need sequenced allocation. But that's not documented.

The postmortem fixture in §6 illustrates the problem:
```yaml
pattern: "postmortems/{id:PM-{seq:03}}/postmortem.md"
id_template: "PM-{seq:03}"  # redundant
```

**Suggested fix:** Either:
1. Remove `id_template` field — extract the template from the `{id:...}` placeholder in the pattern string at parse time, or
2. Document the semantic difference: "id_template is used by `next_id` for bare ID allocation; the `{id:...}` constraint in the pattern is used for path matching/rendering. They MUST agree; mismatch is a LayoutError."

---

## FINDING-5 🟢 Minor — Concurrency model description contradicts `Mutex<RelationIndex>` interior mutability

**Check:** #6 (Data flow completeness) + #21 (Ambiguous prose)
**Section:** §4.6 (ArtifactStore, Concurrency model)

The concurrency model says:
> `&self` for reads (list/get/relations_*); `&mut self` for index updates.

But `index: Mutex<RelationIndex>` provides interior mutability — `Mutex::lock()` takes `&self`, so index updates (lazy rebuild on mtime change) can happen through `&self` methods. There's no need for `&mut self` for index updates when the index is behind a Mutex.

This means:
- `relations_to(&self, ...)` may trigger an index rebuild via `self.index.lock()` — this is fine, `&self` suffices.
- Saying "`&mut self` for index updates" is misleading — with the current design, NO public method needs `&mut self`.

This is cosmetic (the implementation will figure it out), but two engineers might disagree on whether `create`/`update` need `&mut self` or can use `&self` with interior mutability for index invalidation.

**Suggested fix:** Simplify to: "All public methods take `&self`. `Mutex<RelationIndex>` provides interior mutability for lazy cache rebuilds. For daemon usage, wrap in `Arc` — no `RwLock` needed since `Mutex` already allows concurrent read access (with brief lock contention on rebuild)."

---

## FINDING-6 🟡 Important — No `Metadata` constructor for new artifacts; only `parse` (for existing files) is defined

**Check:** #22 (Missing helpers) + #6 (Data flow completeness)
**Section:** §4.2 (Metadata) + §4.6 (ArtifactStore::create)

`ArtifactStore::create(&self, path: &Path, metadata: Metadata, body: &str)` takes a `Metadata` value. `Metadata::parse(file_contents)` constructs `Metadata` from existing file contents. But for **new** artifacts, there is no file to parse — the caller needs to build a `Metadata` from scratch.

The `Metadata` struct has three fields:
```rust
pub fields: BTreeMap<String, MetaValue>,
pub source: MetaSource,
pub raw: String,  // original metadata block, byte-exact
```

For a new artifact:
- `fields`: caller populates
- `source`: should be derived from `LayoutPattern::metadata_format: MetaSourceHint` — but how? There's no mapping from `MetaSourceHint` → `MetaSource`.
- `raw`: empty string? The round-trip preservation semantics are nonsensical for files that don't exist yet.

Without a `Metadata::new(source: MetaSourceHint, fields: BTreeMap<...>)` or equivalent constructor, the `create` workflow is under-specified.

**Suggested fix:** Add a constructor:
```rust
impl Metadata {
    /// Create metadata for a new artifact. `MetaSourceHint` determines the
    /// format for `render()`. `raw` is initially empty (no round-trip source).
    pub fn new(hint: MetaSourceHint, fields: BTreeMap<String, MetaValue>) -> Self;
}
```
Or document that callers construct `Metadata { fields, source: MetaSource::YamlFrontmatter, raw: String::new() }` directly — but then `render()` must handle empty `raw` gracefully.

---

## FINDING-7 🟢 Minor — `{id:...}` render behavior when both `id` and `seq` slots are present and disagree is undefined

**Check:** #21 (Ambiguous prose)
**Section:** §4.4.1 (Pattern DSL, bidirectional example)

The render example shows:
> `slots = {seq: "0042"}` (or auto-allocated) → `issues/ISS-0042/issue.md`

This implies the renderer reconstructs `id` from the template using `seq`. But what if the caller provides `{id: "ISS-0099", seq: "0042"}` — which wins? The match direction populates *both* `id` and `seq` in the `SlotMap`, so round-tripping would provide both. If a consumer modifies `seq` but not `id` (or vice versa), behavior is undefined.

**Suggested fix:** Add a precedence rule: "When rendering `{id:TEMPLATE}`, the template is always re-evaluated from its component slots (`seq`, literals). A pre-populated `id` slot is **ignored** during rendering (it is output-only from matching). This ensures `id` and `seq` never disagree in generated paths."

---

## FINDING-8 🟢 Minor — Broken cross-reference: "§4.5.3" doesn't exist as a section heading

**Check:** #2 (Every reference resolves)
**Section:** §6 (find_references_to AC test)

The AC test says: "expects `RelationSource::MarkdownLink` (via inline-backtick rule §4.5.3)."

But §4.5 has no subsection §4.5.3 — the backtick rule is simply item 3 in an ordered list within §4.5. The reference should be "§4.5 rule 3" or "§4.5 item 3".

**Suggested fix:** Change "§4.5.3" → "§4.5 rule 3".

---

<!-- FINDINGS -->

## r1 Verification

Each of the 11 r1 findings was checked against the current document:

1. **FINDING-1** (Critical, undefined types) — **Verified ✓** — `SlotMap`, `MetaSourceHint`, `RelationIndex` all defined in §4.4 with type aliases/enums. `Layout::relation_fields()` method added to Layout API block. *However*, the new `Layout::resolve` signature introduces **new** undefined types (`Kind`, `RelativePath`, `LayoutError`) — see r2 FINDING-2.
2. **FINDING-2** (Important, separator) — **Verified ✓** — All short-form references now use `:` separator consistently (`engram:ISS-022`, `engram:feature/dim-extract`). No stale `/`-separated references found. Matches `project_registry.rs` convention.
3. **FINDING-3** (Important, malformed YAML) — **Partial ✓** — `MalformedFrontmatter` error behavior documented in §4.2 parse docstring. `gid artifact lint` downgrade to warning specified. *However*, the function signature was not updated from `(Self, String)` to `Result<...>` — see r2 FINDING-1.
4. **FINDING-4** (Minor, seq overflow) — **Verified ✓** — §4.4.1 "Sequence overflow" paragraph added. `Err(LayoutError::SeqExhausted { pattern, max })`. No silent rollover. Root fix, not patch.
5. **FINDING-5** (Important, relation_fields) — **Verified ✓** — `relation_fields: Vec<String>` field added to `Layout` struct with docstring. Default list specified. Overridable via `.gid/layout.yml`.
6. **FINDING-6** (Critical, DSL grammar) — **Verified ✓** — §4.4.1 added with full EBNF, token semantics table, nesting rule, bidirectional example, escaping rules, match precedence. Well-specified.
7. **FINDING-7** (Minor, missing helpers) — **Verified ✓** — `Layout::resolve` and `merge_list` both defined with signatures and semantics. *However*, `Layout::resolve` uses undefined types — see r2 FINDING-2.
8. **FINDING-8** (Critical, name collision) — **Verified ✓** — `ArtifactRef` → `ArtifactId` rename applied throughout §4–§9. Only one remaining mention is the deliberate §4.1 note explaining the rename. No stale references found.
9. **FINDING-9** (Important, D2 test binding) — **Verified ✓** — §6 scalability test expanded with fixture path, exact `layout.yml` snippet, three verification assertions (list, refs grep, next_id from zero), and CI binding statement.
10. **FINDING-10** (Important, testable ACs) — **Verified ✓** — `**Test:**` sub-bullets added to round-trip, resolve, find_references_to, and wrapper ACs. Each specifies fixture path, exact command, expected output.
11. **FINDING-11** (Important, migration) — **Verified ✓** — §7 now has top-level `.gid/reviews/{name}.md` pattern, `.gid/{slug}/{any}.md` fallback acknowledged, Phase 0 migration verification step (blocks rollout), and rollback path note.

## ✅ Passed Checks

- **Check #0**: Document size ✅ — 6 model components in §4, under 8 limit.
- **Check #2**: References resolve ✅ — all §N cross-refs verified present (except §4.5.3 → see FINDING-8).
- **Check #3**: No dead definitions ✅ — all types used in ArtifactStore API, ACs, or relation discovery.
- **Check #4**: Consistent naming ✅ — `ArtifactId` used everywhere; `:` separator consistent; `kind: String` consistent (except `Layout::resolve` → FINDING-2).
- **Check #5**: State machine N/A ✅ — CRUD model, no state machine.
- **Check #6**: Data flow ✅ — parse→set_field→render chain consistent; kind derived from path via Layout (except Metadata constructor gap → FINDING-6).
- **Check #7**: Error handling ✅ for all paths except `Metadata::parse` signature (→ FINDING-1).
- **Check #8**: String operations ✅ — no unsafe string slicing in design.
- **Check #9**: Integer overflow ✅ — `{seq:NN}` overflow handled by `SeqExhausted`.
- **Check #10**: Option/None ✅ — `get` returns `Result<Option<>>`, `parent` params are `Option`. No `.unwrap()`.
- **Check #11**: Match exhaustiveness ✅ — all enums have explicit variants; no catch-all branches.
- **Check #12**: Ordering sensitivity ✅ — Metadata parse order intentional; pattern match precedence documented as "first match wins."
- **Check #13**: Separation of concerns ✅ — pure types separated from IO (ArtifactStore).
- **Check #14**: Coupling ✅ — kind derived not stored; relations reference by ArtifactId.
- **Check #15**: Configuration vs hardcoding ✅ — relation_fields, patterns, default layout all configurable.
- **Check #16**: API surface ✅ — 9 methods on ArtifactStore + 2 free functions, all necessary.
- **Check #17**: Goals/non-goals ✅ — §1 constraints clear; §8 out-of-scope comprehensive.
- **Check #18**: Trade-offs ✅ — D1–D5 document rejected alternatives; §10 desync acceptance well-reasoned.
- **Check #19**: Cross-cutting concerns ✅ — performance (<1000 artifacts), security N/A, error visibility via lint.
- **Check #20**: Abstraction level ✅ — Rust signatures + behavior docs, not implementation bodies.
- **Check #21**: Ambiguous prose — mostly resolved by r1 fixes; remaining minor ambiguities in FINDING-5 and FINDING-7.
- **Check #22**: Missing helpers — `merge_list` and `Layout::resolve` now defined; `Metadata::new` missing → FINDING-6.
- **Check #23**: Dependencies ✅ — std + chrono (NaiveDate). No unverified assumptions.
- **Check #24**: Migration path ✅ — §7 detailed with phasing, rollback, verification step.
- **Check #25**: Testability ✅ — pure types unit-testable; fixture-based integration tests specified.
- **Check #26**: No duplicate functionality ✅ — `ArtifactManager` (ritual phase flow) has zero overlap with `ArtifactStore` (project artifact CRUD).
- **Check #27**: API compatibility ✅ — new additive module; no existing callers broken.
- **Check #28**: Feature flag ✅ — can be gated behind cargo feature if desired.
- **Check #29**: Ground truth ✅ — `project_registry.rs` `:` convention confirmed; `ritual::definition::ArtifactRef` confirmed different type; top-level `.gid/reviews/` (18 files) confirmed exists on disk.
- **Check #30**: Technical debt ✅ — §10 desync debt explicitly documented with trigger (ISS-052). §9 Risk #5 placeholder vocabulary documented. No hidden debt from r1 additions.
- **Check #31**: Shortcut detection ✅ — design addresses root cause (no artifact model). D1–D5 fix structural v0–v2 mistakes.
- **Check #32**: Architecture conflicts ✅ — `Result<>` error handling matches existing gid-core patterns. No `unwrap()`. No bypassed layers.
- **Check #33**: Simplification ✅ — handles full complexity: mixed metadata, arbitrary nesting, ad-hoc kinds. Edge cases addressed.
- **Check #34**: Breaking-change risk ✅ — additive module; §7 says no file moves/renames; existing graph tools untouched.
- **Check #35**: Purpose alignment ✅ — all types serve stated goals; sugar aliases explicitly optional; no speculative flexibility.

## Applied

All 8 findings applied 2026-04-28 to `.gid/issues/ISS-053/issue.md`.

- **FINDING-1 (Critical) — Applied.** §4.2: changed `Metadata::parse` signature to `Result<(Self, String), MetadataError>`; added `MetadataError` enum (`MalformedFrontmatter { line, message }`, `IoError(std::io::Error)`); updated docstring to note `Ok((metadata, body))` shape; updated §4.6 `ArtifactStore::list`/`get` docstrings to show `?` propagation; updated §6 round-trip test to use `parse(...)?`.
- **FINDING-2 (Important) — Applied.** §4.4: `Layout::resolve` now `(&self, kind: &str, slots: &SlotMap) -> Result<PathBuf, LayoutError>`. Added `LayoutError` enum near §4.4 with `UnknownKind(String)`, `MissingSlot { kind, slot }`, `SeqExhausted { pattern, max }` (the §4.4.1 SeqExhausted variant now lives in this enum).
- **FINDING-3 (Important) — Applied.** §4.2: added `Metadata::new(source_hint: MetaSourceHint) -> Self` constructor with semantic note "Caller populates fields via `set_field` before render." §4.6 `create` docstring now references `Metadata::new(hint)` as the construction path for new artifacts.
- **FINDING-4 (Minor) — Applied.** §6 AC reworded: "all four discovery rules in §4.5 (frontmatter, markdown link, inline backtick, directory nesting), mapped onto the three `RelationSource` variants (`Frontmatter`, `MarkdownLink`, `DirectoryNesting`; backtick refs share the `MarkdownLink` discriminant)." Variant list (3) and rule list (4) are now both explicit and consistent.
- **FINDING-5 (Minor) — Applied.** §4.4: removed the standalone `id_template: Option<String>` field from `LayoutPattern`; replaced with an inline note that the ID template is encoded inside the pattern via `{id:TEMPLATE}` and extracted by a helper. §6 postmortem fixture YAML no longer includes the redundant `id_template:` line.
- **FINDING-6 (Minor) — Applied.** §4.6 "Concurrency model" expanded with read-mostly workload note, in-memory-only RelationIndex (no persistent cache → no cross-process invalidation), `tempfile + rename` atomic writes, last-writer-wins for concurrent edits (acceptable: human-edited markdown, low contention, git resolves), and explicit "no separate index to keep in sync" note tied to D3.
- **FINDING-7 (Minor) — Applied.** §4.4.1: added "Render precedence for `{id:TEMPLATE}`" paragraph after the bidirectional table — `id` slot wins if present; else template is recursively resolved from `seq` + literals; if `id` and `seq` both present and `id` starts with rendered `seq`, the seq is consumed; otherwise `LayoutError::MissingSlot`.
- **FINDING-8 (Minor) — Applied.** §6 broken cross-reference "§4.5.3" replaced with "§4.5 rule 3" (the inline-backtick rule is item 3 in §4.5's ordered list, not a numbered subsection).

### Consistency sweep

- `grep -n "ArtifactRef" .gid/issues/ISS-053/issue.md` → 1 hit, the deliberate §4.1 rename callout. ✓
- `grep -n "Metadata::parse" .gid/issues/ISS-053/issue.md` → all occurrences are Result-aware (signature, docstrings, round-trip test uses `parse(...)?`). ✓
- `grep -n "RelativePath" .gid/issues/ISS-053/issue.md` → 0 hits. ✓
- `grep -n "id_template:" .gid/issues/ISS-053/issue.md` → 0 hits (only `id_template` survives in the EBNF grammar nonterminal, which is correct). ✓
- No stale "§4.5.3" references; no stale "all four `RelationSource` types" prose. ✓
