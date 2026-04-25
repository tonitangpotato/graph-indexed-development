# ISS-001: CodeNode Test Helper / Builder Pattern Refactor

**Status:** open
**Reported:** 2026-04-02

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
