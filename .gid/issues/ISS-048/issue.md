---
id: "ISS-048"
title: "CodeNode Test Helper / Builder Pattern Refactor"
status: closed
priority: P2
created: 2026-04-26
closed: 2026-04-26
---
# ISS-048: CodeNode Test Helper / Builder Pattern Refactor

> **Renumbering note (2026-04-26):** Originally filed as ISS-001 on `iss-001-002-revive` branch, accidentally reusing a closed issue number. Renumbered to ISS-048 to preserve the historical ISS-001 (`gid extract --output` flag bug). Content unchanged.


**Status:** closed
**Reported:** 2026-04-02
**Closed:** 2026-04-26

## Resolution

Added `CodeNode::test_default(id, kind)` helper in `crates/gid-core/src/code_graph/types.rs:359` that returns a fully-populated `CodeNode` with sensible defaults. Tests now use Functional Update Syntax (FRU) to override only the fields they care about:

```rust
CodeNode {
    name: "main.rs".into(),
    file_path: "src/main.rs".into(),
    line_count: 100,
    ..CodeNode::test_default("file:src/main.rs", NodeKind::File)
}
```

When new fields are added to `CodeNode`, only `test_default` needs updating — not every test instantiation.

### Refactored Sites (10 test struct literals)

- `crates/gid-core/src/unify.rs` — 3 sites (test_codegraph_graph_roundtrip)
- `crates/gid-core/src/complexity.rs` — 3 sites (test_complex_with_many_files, test_risk_level)
- `crates/gid-core/src/unified.rs` — 2 sites (test_build_unified_graph)
- `crates/gid-core/src/code_graph/tests.rs` — 2 sites (remap_cross_file_impl_edges test)

Production-code site `unify.rs:111` (extraction logic, not a test) was intentionally NOT refactored — `test_default` is a test helper, not a general default.

## Verification

```
$ cargo test -p gid-core --features full --lib
test result: ok. 1092 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.23s
```

Zero occurrences of old-style `id: ...to_string(), kind: NodeKind::...` fields in the four refactored test files (verified via grep).

---

## Original Report

## Problem

CodeNode struct literal is used directly in ~50+ places across test files. Adding a new field requires updating every single instantiation manually. This happened when adding `visibility`, `lang`, `body_hash`, `end_line`, `complexity` — required touching 4-5 test files with dozens of changes each.

## Affected Files

- `crates/gid-core/src/complexity.rs` (tests)
- `crates/gid-core/src/unified.rs` (tests)
- `crates/gid-core/src/unify.rs` (tests)
- `crates/gid-core/src/code_graph/tests.rs`

## Proposed Solution

1. Add `CodeNode::test_default(name, kind)` helper that fills all fields with sensible defaults
2. Or implement a builder pattern: `CodeNode::builder("foo", Function).file("main.rs").build()`
3. All test code uses the helper instead of raw struct literals
4. Future field additions only need to update the helper, not every test

## Priority

P2 — Not blocking, but saves significant time on future CodeNode changes.

## Status: Open
