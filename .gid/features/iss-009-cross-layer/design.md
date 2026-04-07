# Design: ISS-009 Phase 1 — Cross-Layer Graph Connections

## 1 Problem Statement

GID's graph is supposed to be multi-layered (architecture → task → code), with edges connecting across layers. In reality:

1. **Extract generates no Module nodes** — `NodeKind::Module` exists in types.rs but `extract_from_dir()` never creates them
2. **Extract generates no TestsFor edges** for Rust/TS — Python has a partial `tests_for` implementation in `extract_calls_for_file()`, but Rust and TypeScript test files get no TestsFor edges
3. **No directory aggregation** — files are flat, no `belongs_to` edges grouping files into modules
4. **Query only walks `depends_on`** — `impact()` and `deps()` ignore `calls`, `defined_in`, `imports`, `belongs_to`, `implements`

This means: change a source file → can't find which tests break. Change a function → can't trace impact through callers. Task node exists alongside code nodes but they're completely disconnected.

## 2 Scope

**Phase 1 only** — deterministic extract-time and query-time fixes. No LLM calls, no design.rs changes, no unified.rs changes.

### Goals
- GOAL-1: Extract generates Module nodes from directory structure
- GOAL-2: File nodes have `belongs_to` edges to their Module node
- GOAL-3: Module nodes have `belongs_to` edges to parent Module nodes (nested directories)
- GOAL-4: Test files generate `TestsFor` edges to source files (Rust and TypeScript — Python already partial)
- GOAL-5: `impact()` traverses configurable edge relations (not just `depends_on`)
- GOAL-6: `deps()` traverses configurable edge relations
- GOAL-7: Incremental extract handles Module nodes correctly (add/remove on file changes) *(depends on ISS-006 — incremental extract not yet implemented)*

### Non-Goals
- Cross-layer edges between task and code nodes (Phase 2: unified.rs + design.rs)
- LSP-based precision (ISS-002)
- Semantic module detection (beyond directory structure)

## 3 Design

### 3.1 Module Node Generation

**Strategy**: Each directory containing at least one source file becomes a Module node.

```
src/
  auth/
    mod.rs        → file:src/auth/mod.rs
    middleware.rs  → file:src/auth/middleware.rs
  main.rs         → file:src/main.rs
```

Produces:
```
module:src         (name: "src",    kind: Module)
module:src/auth    (name: "auth",   kind: Module)
file:src/main.rs   ──belongs_to──→ module:src
file:src/auth/mod.rs ──belongs_to──→ module:src/auth
file:src/auth/middleware.rs ──belongs_to──→ module:src/auth
module:src/auth ──belongs_to──→ module:src
```

**ID convention**: `module:{relative_dir_path}` (e.g., `module:src/auth`)

**Name**: directory basename (e.g., `auth`), except root which uses the full relative path

**CodeNode for Module**:

> **Note (FINDING-9):** The `file_path` field is overloaded — for file nodes it stores a file path, for module nodes it stores a directory path. This is a known semantic mismatch accepted for Phase 1. A future refactor may rename the field to `path` to be more generic.

```rust
impl CodeNode {
    pub fn new_module(dir_path: &str) -> Self {
        let name = dir_path.rsplit('/').next().unwrap_or(dir_path);
        Self {
            id: format!("module:{}", dir_path),
            kind: NodeKind::Module,
            name: name.to_string(),
            file_path: dir_path.to_string(), // NOTE: stores directory path, not file path
            line: None,
            decorators: Vec::new(),
            signature: None,
            docstring: None,
            line_count: 0,
            is_test: dir_path.contains("/test") || dir_path.contains("/tests"),
        }
    }
}
```

### 3.2 Module Generation in extract_from_dir

After collecting all source files and before the parsing phase, scan the file entries to identify unique directories.

> **Note (FINDING-10):** `.gidignore` filtering must run **before** `generate_module_nodes()`. The walkdir filtering (which already respects `.gidignore`) produces the `file_entries` list — so excluded directories will not appear in `file_entries` and will not get module nodes. Ensure `generate_module_nodes()` is only called with the already-filtered file list.

```rust
fn generate_module_nodes(file_entries: &[(String, String, Language)]) -> Vec<(CodeNode, Vec<CodeEdge>)> {
    let mut dir_set: HashSet<String> = HashSet::new();

    // Collect all directories that contain source files
    for (rel_path, _, _) in file_entries {
        let dir = rel_path.rsplitn(2, '/').nth(1).unwrap_or("");
        if !dir.is_empty() {
            // Add this directory and all ancestors
            let mut current = dir.to_string();
            loop {
                dir_set.insert(current.clone());
                match current.rsplitn(2, '/').nth(1) {
                    Some(parent) if !parent.is_empty() => current = parent.to_string(),
                    _ => break,
                }
            }
        }
    }

    let mut results = Vec::new();

    for dir in &dir_set {
        let module_node = CodeNode::new_module(dir);
        let mut edges = Vec::new();

        // Module → parent module (belongs_to)
        if let Some(parent) = dir.rsplitn(2, '/').nth(1) {
            if !parent.is_empty() && dir_set.contains(parent) {
                edges.push(CodeEdge::new(
                    &format!("module:{}", dir),
                    &format!("module:{}", parent),
                    EdgeRelation::BelongsTo,
                ));
            }
        }

        results.push((module_node, edges));
    }

    // File → module edges are added separately (after file nodes exist)
    results
}
```

**File → Module edges**: After file nodes are generated in `integrate_file_results`, add:

```rust
// In integrate_file_results or after:
let dir = rel_path.rsplitn(2, '/').nth(1).unwrap_or("");
if !dir.is_empty() {
    state.edges.push(CodeEdge::new(
        &format!("file:{}", rel_path),
        &format!("module:{}", dir),
        EdgeRelation::BelongsTo,
    ));
}
```

### 3.3 New EdgeRelation: BelongsTo

Add to `types.rs`:

```rust
pub enum EdgeRelation {
    Imports,
    Inherits,
    DefinedIn,
    Calls,
    TestsFor,
    Overrides,
    Implements,
    BelongsTo,  // ← NEW
}
```

Semantics: `file:X belongs_to module:Y` means file X is inside directory Y. `module:X belongs_to module:Y` means directory X is a subdirectory of Y.

**Confidence semantics (FINDING-2, FINDING-11):**
- `BelongsTo` edges use `CodeEdge::new()` → `confidence: 1.0` (deterministic — directory structure is factual)
- `TestsFor` edges use struct literal → `confidence: 0.8` (heuristic — naming convention match, not import analysis)
- Note: `CodeEdge::new()` sets `call_site_line: None` and `call_site_column: None`, which is correct for structural edges like `BelongsTo` (they have no call site). These fields are semantically designed for call-like edges; their presence on structural edges is a known code smell, accepted as non-blocking.

Consider adding a `CodeEdge::new_heuristic(from, to, relation, confidence)` constructor during implementation to make the pattern explicit and avoid raw struct literals for heuristic edges.

**Serde/FromStr (FINDING-8):** Verify that the `EdgeRelation` serde derive (or manual `FromStr`/`Deserialize` impl) handles `"belongs_to" → BelongsTo` correctly. If `EdgeRelation` uses `#[serde(rename_all = "snake_case")]` it should work automatically. Add a roundtrip test (see §5.1) to confirm.

### 3.4 TestsFor Edge Generation (Rust)

Currently Python has partial TestsFor in `extract_calls_for_file()`. Rust and TypeScript have none.

**Rust test detection strategy**:

Rust tests are typically:
1. **In-file tests**: `#[cfg(test)] mod tests { ... }` inside the source file → NOT a separate file, no TestsFor edge needed
2. **tests/ directory**: `tests/test_auth.rs` tests `src/auth.rs` or `src/auth/mod.rs`
3. **Naming convention**: `src/foo.rs` tested by `tests/foo.rs` or `tests/test_foo.rs`

Approach:
```rust
fn generate_rust_tests_for_edges(
    file_entries: &[(String, String, Language)],
) -> Vec<CodeEdge> {
    let mut edges = Vec::new();

    // Collect source files (non-test)
    let source_files: HashMap<String, String> = file_entries.iter()
        .filter(|(path, _, lang)| {
            *lang == Language::Rust
            && !path.starts_with("tests/")
            && !path.contains("/tests/")
        })
        .map(|(path, _, _)| {
            // stem: "src/auth/middleware.rs" → "auth/middleware"
            // also: "src/auth/mod.rs" → "auth"
            let stem = path
                .strip_prefix("src/").unwrap_or(path)
                .trim_end_matches(".rs");
            let stem = if stem.ends_with("/mod") {
                &stem[..stem.len() - 4]
            } else {
                stem
            };
            (stem.to_string(), format!("file:{}", path))
        })
        .collect();

    // Find test files and match to source
    for (path, _, lang) in file_entries {
        if *lang != Language::Rust {
            continue;
        }
        if !path.starts_with("tests/") && !path.contains("/tests/") {
            continue;
        }

        let test_file_id = format!("file:{}", path);

        // Extract test stem: "tests/test_auth.rs" → "auth", "tests/auth.rs" → "auth"
        // (FINDING-5: simplified — avoid redundant re-stripping in fallback)
        let raw = path.strip_prefix("tests/").unwrap_or(path).trim_end_matches(".rs");
        let test_stem = raw.strip_prefix("test_").unwrap_or(raw);

        // Try matching: exact stem, or module name
        if let Some(source_id) = source_files.get(test_stem) {
            edges.push(CodeEdge {
                from: test_file_id,
                to: source_id.clone(),
                relation: EdgeRelation::TestsFor,
                weight: 0.5,
                call_count: 1,
                in_error_path: false,
                confidence: 0.8,  // naming convention match, not import analysis
                call_site_line: None,
                call_site_column: None,
            });
        }
    }

    edges
}
```

**TypeScript test detection**:

Similar pattern — `*.test.ts`, `*.spec.ts`, `__tests__/*.ts`:

```rust
fn generate_ts_tests_for_edges(
    file_entries: &[(String, String, Language)],
) -> Vec<CodeEdge> {
    let mut edges = Vec::new();

    let source_files: HashMap<String, String> = file_entries.iter()
        .filter(|(path, _, lang)| {
            *lang == Language::TypeScript
            && !path.contains(".test.")
            && !path.contains(".spec.")
            && !path.contains("__tests__/")
        })
        .map(|(path, _, _)| {
            let stem = path
                .trim_end_matches(".ts")
                .trim_end_matches(".tsx")
                .trim_end_matches(".js")
                .trim_end_matches(".jsx");
            (stem.to_string(), format!("file:{}", path))
        })
        .collect();

    for (path, _, lang) in file_entries {
        if *lang != Language::TypeScript {
            continue;
        }

        let is_test = path.contains(".test.") || path.contains(".spec.") || path.contains("__tests__/");
        if !is_test {
            continue;
        }

        let test_file_id = format!("file:{}", path);

        // "auth.test.ts" → "auth", "auth.spec.ts" → "auth"
        let source_stem = path
            .replace(".test.", ".")
            .replace(".spec.", ".")
            .replace("__tests__/", "")
            .trim_end_matches(".ts")
            .trim_end_matches(".tsx")
            .trim_end_matches(".js")
            .trim_end_matches(".jsx")
            .to_string();

        if let Some(source_id) = source_files.get(&source_stem) {
            edges.push(CodeEdge {
                from: test_file_id,
                to: source_id.clone(),
                relation: EdgeRelation::TestsFor,
                weight: 0.5,
                call_count: 1,
                in_error_path: false,
                confidence: 0.8,
                call_site_line: None,
                call_site_column: None,
            });
        }
    }

    edges
}
```

### 3.5 Enhance Python TestsFor

The existing Python TestsFor logic in `extract_calls_for_file()` only matches `from X import` statements. Add naming-convention matching as fallback (same pattern as Rust/TS).

### 3.6 Query Enhancement: Multi-Relation Traversal

> **Note (FINDING-1):** There are two distinct query systems that need enhancement:
> 1. **`CodeGraph::impact_analysis()`** in `code_graph/analysis.rs` — operates on `CodeGraph` with `CodeEdge` / `EdgeRelation`
> 2. **`QueryEngine::impact()`** and `QueryEngine::deps()` in `query.rs` — operates on the task `Graph` with `Node` / `depends_on: Vec<String>`
>
> These are **different types with different traversal mechanisms**. This section addresses both.

#### 3.6.1 CodeGraph Analysis Enhancement

`CodeGraph::impact_analysis()` in `code_graph/analysis.rs` currently traverses all edge relations. Enhance it to accept an optional relation filter:

```rust
/// In code_graph/analysis.rs
impl CodeGraph {
    /// Compute impact — what code nodes are affected if `node_id` changes.
    ///
    /// `relations`: which edge relations to traverse. If None, traverses all.
    pub fn impact_analysis(
        &self,
        node_id: &str,
        relations: Option<&[EdgeRelation]>,
    ) -> Vec<&CodeNode>
}
```

Default relation set for code graph impact analysis (reverse traversal — who depends on me):
- `Calls` — function B calls A → changing A impacts B
- `Imports` — file B imports file A → changing A impacts B
- `DefinedIn` — function A defined_in file F → changing F impacts A
- `BelongsTo` — file belongs_to module → changing module config impacts files
- `Implements` — class implements trait → changing trait impacts class
- `TestsFor` — test file tests source → changing source impacts test

```rust
const DEFAULT_CODE_IMPACT_RELATIONS: &[EdgeRelation] = &[
    EdgeRelation::Calls,
    EdgeRelation::Imports,
    EdgeRelation::DefinedIn,
    EdgeRelation::BelongsTo,
    EdgeRelation::Implements,
    EdgeRelation::TestsFor,
];
```

#### 3.6.2 QueryEngine Enhancement (Task Graph)

`QueryEngine::impact()` and `QueryEngine::deps()` in `query.rs` (line 16, 40) currently only follow `depends_on` edges on the task `Graph`. Enhance to accept an optional relation filter for the task graph's `depends_on` traversal:

```rust
/// In query.rs
impl<'a> QueryEngine<'a> {
    /// Compute impact — what task nodes are affected if `node_id` changes.
    ///
    /// `relations`: which edge types to traverse. If None, uses default set.
    /// For task graph, the primary relation is `depends_on`.
    pub fn impact(&self, node_id: &str, relations: Option<&[&str]>) -> Vec<&Node>

    pub fn deps(&self, node_id: &str, relations: Option<&[&str]>) -> Vec<&Node>
}
```

Default relation set for task graph:
```rust
const DEFAULT_TASK_IMPACT_RELATIONS: &[&str] = &["depends_on"];
```

> **Future (Phase 2):** When `unified.rs` merges task and code graphs, the QueryEngine may need to traverse both task `depends_on` edges and code `EdgeRelation` edges in a single query.

**Backward compatibility**: The CLI commands (`gid query impact`, `gid query deps`) default to the full relation set. Add `--relation` flag to filter.

### 3.7 Incremental Extract: Module Node Handling

When files are added/removed, module nodes need updating:

1. **File deleted** → check if its directory has any remaining source files. If not, remove the module node.
2. **File added to new directory** → create module node + belongs_to edges.
3. **File moved** → handled by delete + add.

> **Note (FINDING-7):** `extract_incremental()` does not exist yet — it depends on ISS-006 (incremental extract). The logic below documents the intended behavior for when ISS-006 is implemented. GOAL-7 is blocked on ISS-006 completion.

In `extract_incremental()`, after processing file delta:

```rust
// Recompute module nodes from current file set
let current_modules = generate_module_nodes(&file_entries);
let existing_module_ids: HashSet<String> = graph.nodes.iter()
    .filter(|n| n.kind == NodeKind::Module)
    .map(|n| n.id.clone())
    .collect();

// Remove stale module nodes
let current_module_ids: HashSet<String> = current_modules.iter()
    .map(|(n, _)| n.id.clone())
    .collect();
for stale_id in existing_module_ids.difference(&current_module_ids) {
    graph.nodes.retain(|n| n.id != *stale_id);
    graph.edges.retain(|e| e.from != *stale_id && e.to != *stale_id);
}

// Add new module nodes
for (node, edges) in current_modules {
    if !existing_module_ids.contains(&node.id) {
        graph.nodes.push(node);
        graph.edges.extend(edges);
    }
}

// Rebuild file→module belongs_to edges (always recompute — cheap)
graph.edges.retain(|e| e.relation != EdgeRelation::BelongsTo || !e.from.starts_with("file:"));
for (rel_path, _, _) in &file_entries {
    let dir = rel_path.rsplitn(2, '/').nth(1).unwrap_or("");
    if !dir.is_empty() {
        graph.edges.push(CodeEdge::new(
            &format!("file:{}", rel_path),
            &format!("module:{}", dir),
            EdgeRelation::BelongsTo,
        ));
    }
}
```

## 4 Files Changed

| File | Change |
|---|---|
| `code_graph/types.rs` | Add `BelongsTo` to `EdgeRelation`, add `CodeNode::new_module()`, verify serde roundtrip for both |
| `code_graph/extract.rs` | Add `generate_module_nodes()`, file→module edges, TestsFor for Rust/TS, integrate into `extract_from_dir()` |
| `code_graph/analysis.rs` | Update `impact_analysis()` to accept optional `EdgeRelation` filter |
| `query.rs` | Update `QueryEngine::impact()` and `QueryEngine::deps()` signatures to accept optional relation filter |
| CLI command handlers | Update `impact`/`deps` commands to pass relation filter (or None for default) |

> **Note (FINDING-1):** `graph.rs` is NOT modified — the query functions live in `query.rs` on `QueryEngine`, and code graph analysis lives in `code_graph/analysis.rs`. The original §4 incorrectly listed `graph.rs`.
>
> **Note (FINDING-7):** Integration into `extract_incremental()` is deferred until ISS-006 is implemented.

## 5 Testing

### 5.1 Module Node Tests

1. `test_module_generation_flat` — flat directory → one module node per dir
2. `test_module_generation_nested` — nested dirs → hierarchical belongs_to edges
3. `test_module_generation_empty_dir` — directory with no source files → no module node
4. `test_file_belongs_to_module` — each non-root file has belongs_to edge to its directory's module
5. `test_root_file_no_belongs_to` — `main.rs` at project root → no belongs_to edge (no module node for root)
6. `test_nodekind_module_serde_roundtrip` — create a Module node, serialize to YAML, deserialize, verify roundtrip (FINDING-3)
7. `test_edge_relation_belongs_to_roundtrip` — serialize `EdgeRelation::BelongsTo`, deserialize, verify `FromStr`/serde handles `"belongs_to"` correctly (FINDING-8)

### 5.2 TestsFor Tests

8. `test_rust_tests_for_matching` — `tests/auth.rs` → `file:src/auth.rs`
9. `test_rust_tests_for_mod` — `tests/auth.rs` → `file:src/auth/mod.rs`
10. `test_ts_test_file_matching` — `auth.test.ts` → `file:auth.ts`
11. `test_ts_spec_file_matching` — `auth.spec.ts` → `file:auth.ts`
12. `test_ts_nested_tests_dir` — `src/components/__tests__/Button.test.tsx` → should match `src/components/Button.tsx` (FINDING-6)
13. `test_python_tests_for_naming` — `test_auth.py` → `file:auth.py`

### 5.3 Query Enhancement Tests

14. `test_code_graph_impact_multi_relation` — code node with `Calls` + `Imports` edges, verify both traversed in `impact_analysis()`
15. `test_code_graph_impact_relation_filter` — explicit `EdgeRelation` filter only traverses specified relations
16. `test_query_engine_impact_multi_relation` — task node with `depends_on`, verify `QueryEngine::impact()` traversal
17. `test_deps_multi_relation` — forward traversal through multiple edge types
18. `test_impact_backward_compat` — `impact(id, None)` uses default set, same behavior as multi-relation

### 5.4 Incremental Tests *(blocked on ISS-006)*

19. `test_incremental_module_add` — new file in new dir → module node created
20. `test_incremental_module_remove` — last file deleted from dir → module node removed
21. `test_incremental_module_stable` — file modified in existing dir → module node unchanged

## 6 Edge Cases

- **Root directory files** (e.g., `main.rs` directly in the extract dir) → no module node, no belongs_to edge *(verified by `test_root_file_no_belongs_to` — see FINDING-4)*
- **Single-file projects** → one file node, no module nodes
- **Hidden directories** (`.git/`) → already filtered by walkdir
- **Symlinks** → already `follow_links: false` in walkdir
- **Empty `tests/` directory** → no test files, no TestsFor edges

## 7 Trade-offs

### Why naming convention for TestsFor instead of import analysis?

Import analysis (what Python already partially does) is more precise but:
- Rust `tests/*.rs` don't `use` the source module by path — they use `use crate::*`
- TypeScript tests use relative imports but the patterns vary wildly
- Naming convention gives 80% accuracy with zero parsing overhead
- We set `confidence: 0.8` to indicate it's heuristic, not definitive
- Phase 2 (or LSP integration from ISS-002) can upgrade confidence to 1.0

### Why not modify unified.rs in Phase 1?

`unified.rs` handles merging CodeGraph + Graph (task graph). That's a Phase 2 concern — connecting task nodes to code nodes. Phase 1 focuses on making the code graph itself complete (modules, test associations) and making queries work with all edge types.
