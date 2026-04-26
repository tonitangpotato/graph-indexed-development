---
id: "ISS-044"
title: "Python import-regex recompiled on every AST-walk iteration"
status: closed
priority: P2
created: 2026-04-26
component: "crates/gid-core/src/code_graph/lang/python.rs, crates/gid-core/src/code_graph/extract.rs, crates/gid-core/src/code_graph/lang/typescript.rs"
related: ["ISS-042"]
---
# ISS-044: Python import-regex recompiled on every AST-walk iteration

**Status:** closed
**Resolution:** fixed (2026-04-26)
**Priority:** P2 (perf; user-visible on large Python codebases)
**Component:** `crates/gid-core/src/code_graph/lang/python.rs`, `crates/gid-core/src/code_graph/extract.rs`, `crates/gid-core/src/code_graph/lang/typescript.rs`
**Filed:** 2026-04-26
**Discovered by:** RustClaw (clippy `regex_creation_in_loops`)
**Related:** ISS-042 (parent cleanup)

---

## Symptom

Inside the top-level AST walk over a Python file's children:

```rust
// python.rs:152
for child in root.children(&mut cursor) {
    match child.kind() {
        // ...
        "import_statement" => {
            let import_text = text(child);
            let re_import = Regex::new(r"import\s+([\w.]+)").unwrap();   // ← compiled every iteration
            if let Some(cap) = re_import.captures(&import_text) {
                // ...
            }
        }
        // ...
    }
}
```

The regex `r"import\s+([\w.]+)"` is recompiled on every loop iteration. For a Python file with N top-level statements (typical: 20–200), that's N regex compilations per file extraction. On a large codebase (e.g. 5,000 .py files × avg 50 top-level nodes each = 250,000 compilations), this is wasted CPU.

There may also be a sibling regex inside the `from ... import ...` branch — check the same loop body for additional `Regex::new` calls.

## Root Cause

Regex was inlined for locality during initial implementation. No one factored it out because perf wasn't measured.

## Impact

- **Per-file overhead:** small (microseconds × N statements)
- **Codebase-scale overhead:** real. Regex compilation in the `regex` crate is non-trivial (DFA construction, optimization passes) — typically 10–50µs per `Regex::new` for a simple pattern, but the `unwrap()` allocation + cleanup also adds up.
- **Estimated total cost on a 5k-file Python repo extraction:** ~5–25 seconds of pure regex-compile overhead. Not catastrophic, but a free 5–25s saving.

## Fix

Two equivalent options, in order of preference:

### Option A: `once_cell::sync::Lazy` or `std::sync::LazyLock` (Rust ≥ 1.80)

Hoist the regex to a module-level `Lazy` static:

```rust
use std::sync::LazyLock;

static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"import\s+([\w.]+)").expect("static regex must compile")
});

// inside the loop:
if let Some(cap) = IMPORT_RE.captures(&import_text) { ... }
```

Compiled exactly once for the lifetime of the process. Thread-safe by construction.

### Option B: Hoist to function-local `let` outside the loop

```rust
fn extract_python_file(...) {
    let re_import = Regex::new(r"import\s+([\w.]+)").unwrap();
    // ...
    for child in root.children(&mut cursor) {
        // ... use re_import inside import_statement branch ...
    }
}
```

Compiled once per file extraction. Simpler but still does the work N_files times. Acceptable for now if A feels heavyweight, but A is strictly better.

**Recommended: A**, because GID's extract pipeline can re-enter `extract_python_file` thousands of times per `gid extract` run.

## Verification

- After fix: `cargo clippy ... 2>&1 | grep regex_creation_in_loops` → empty
- Micro-benchmark (optional but ideal): `cargo bench` on extracting a 100-file Python fixture before/after — expect to see measurable improvement on the import-resolution path
- All 1243 existing tests still pass; behavior unchanged (regex output is identical)

## Sibling Audit

While fixing this, grep for other `Regex::new(...)` calls inside loops across the workspace:

```bash
grep -n -B2 -A2 "Regex::new" crates/gid-core/src/**/*.rs | grep -B5 -A5 "for "
```

Fix any others found in the same PR — they have the same root cause.

---

## Resolution Record (2026-04-26)

### Audit Result

Workspace-wide `grep -rn "Regex::new" crates/gid-core/src/` revealed:

- ✅ Already cached (`OnceLock` / `get_or_init`):
  - `identity.rs:234`
  - `rust_lang.rs:797–801` (5 regexes)
- ❌ Constructed inline (fixed):
  - `python.rs:199` — primary report site, inside `for child in root.children(...)` AST walk
  - `extract.rs:332` — `re_from_import` in test-to-source mapping; defined per-call (so per .py file extracted)
  - `typescript.rs:710-714` — 5 regexes in `extract_typescript_regex` (currently `#[allow(dead_code)]` fallback, kept for future re-activation)
- ⚪ Not in scope (dynamic patterns built from runtime input — must stay):
  - `ritual/gating.rs:99` (compiles user-supplied glob patterns)
  - `ignore.rs:90` (compiles per-pattern `.gidignore` rules)

### Changes Applied

All three "❌" sites refactored to module-level `OnceLock<Regex>` accessor functions, matching the existing convention from `rust_lang.rs`:

1. **`python.rs`** — added `import_statement_re()` helper at the top of the file. Loop body now calls `import_statement_re().captures(&import_text)`.
2. **`extract.rs`** — added `from_import_re()` helper near the top. The `if is_test_file` block uses `from_import_re().captures(line)`.
3. **`typescript.rs`** — `extract_typescript_regex` now declares 5 `static OnceLock<Regex>` blocks at the top of the function; each gets a matching `get_or_init` call. Comment notes this aligns the dead-code fallback with the live extractor convention so the fix doesn't regress if the function is reactivated.

### Verification

- `cargo clippy --all-targets --all-features 2>&1 | grep -i "regex_creation_in_loops"` → empty (warning eliminated)
- `cargo clippy --all-targets --all-features 2>&1 | grep -i "regex"` → zero regex-related warnings
- `cargo test --workspace --all-features` → **1114 + sub-suites all green** (gid-core lib: 1114 passed / 0 failed / 0 ignored; integration suites all pass; 4 ignored in one async suite are pre-existing). No regression vs. baseline.
- `cargo build --workspace --all-features` → clean (3 pre-existing warnings: one unused import in `watch.rs`, one `seen_for` unused-assignment in `rust_lang.rs:1720`, one dead `save_graph_json` in `extract.rs:1608` — all unrelated to this fix)

### Notes

- No new unit tests added — behavior is identical (same regex pattern, same captures), only the construction call site changed. Existing extraction tests (covering Python imports and TypeScript regex fallback) exercise the new code path.
- All three sites use `.expect("static regex must compile")` instead of `.unwrap()` to make a future panic message useful (these regexes are constants — failure means a typo in the pattern, not user input).
