# TODO-MASTER.md — gid-rs Project Status

> Last updated: 2026-04-08
> 816 tests | 53,527 lines Rust | 73 source files | 2 crates (gid-core + gid-cli)

---

## Project Stats

| Metric | Value |
|--------|-------|
| Total tests | 816 (752 gid-core + 64 others) |
| Source lines | 53,527 |
| Source files | 73 |
| In-code TODOs | 2 |
| Open issues | 3 (ISS-003, ISS-008, ISS-009) |
| Closed issues | 7 (ISS-001,002,004,005,006,007,010) |

## Crate Structure

```
gid-rs/
├── crates/
│   ├── gid-core/          ← Graph engine, storage, extract, harness, ritual
│   │   └── src/
│   │       ├── graph.rs           (1,595 lines) — core graph types + operations
│   │       ├── advise.rs          (1,725 lines) — analysis + infomap integration
│   │       ├── history.rs         (1,550 lines) — snapshot/diff/restore
│   │       ├── lsp_client.rs      (1,174 lines) — LSP integration for precise edges
│   │       ├── code_graph/
│   │       │   ├── extract.rs     (1,617 lines) — tree-sitter + LSP extraction
│   │       │   ├── lang/rust_lang.rs  (1,772 lines)
│   │       │   ├── lang/typescript.rs (1,335 lines)
│   │       │   └── lang/python.rs     (1,156 lines)
│   │       ├── storage/
│   │       │   ├── sqlite.rs      (2,120 lines) — full GraphStorage impl
│   │       │   └── migration.rs   (2,858 lines) — YAML → SQLite pipeline
│   │       ├── harness/
│   │       │   └── context.rs     (2,826 lines) — task context assembly
│   │       └── ritual/
│   │           ├── state_machine.rs (1,994 lines) — v2 ritual state machine
│   │           ├── executor.rs     (1,274 lines) — v1 executor
│   │           └── v2_executor.rs  (1,122 lines) — v2 executor
│   └── gid-cli/           ← CLI binary
│       └── src/main.rs    (4,325 lines) — 13+ commands, --backend yaml|sqlite
├── docs/                  ← Design docs, roadmap, research
└── .gid/                  ← GID meta (features, graph, reviews)
```

## Issue Tracker Summary

### ✅ Closed

| Issue | Type | Description |
|-------|------|-------------|
| ISS-001 | bug P1 | `gid extract` writes to wrong .gid/ path — fixed with `--output` |
| ISS-002 | improvement P0 | Call edge false positives — **solved via LSP client** (lsp_client.rs) |
| ISS-004 | improvement P1 | LSP `references` + `implementation` support — implemented |
| ISS-005 | improvement P2 | Call site `(line, col)` for LSP lookup — implemented |
| ISS-006 | improvement P2 | Incremental extract — **done**, detects file changes, partial re-extract |
| ISS-007 | bug P0 | Duplicate nodes from module_map partial paths — **fixed**, normalization pass |
| ISS-010 | improvement P1 | Triage-size-driven review depth — **done**, 3-tier review |

### 🟡 Open

| Issue | Type | Priority | Status | Notes |
|-------|------|----------|--------|-------|
| ISS-003 | improvement | P2 | Partially addressed | Leiden clustering → **replaced by infomap-rs** (published crate). Integrated into `advise` command. Semantify still uses path heuristics. |
| ISS-008 | feature | P2 | Open | Shared function detection (semantic dedup). Depends on LSP + community detection. Phase 3 roadmap item. |
| ISS-009 | bug | P0 | Mostly fixed | Cross-layer graph connections. Phase 11 added `link_tasks_to_code()` auto-call in `build_unified_graph()` + extract. Module nodes + TestsFor edges + multi-relation query all implemented. Remaining: design.rs `parse_design_yaml()` doesn't auto-generate `implements` edges from task metadata. |

## In-Code TODOs

1. **graph.rs:741** — `// TODO: after T4.1 migration backfills source on all nodes, remove the None branch`
   - Low priority. The None branch is a safe fallback.

2. **history.rs:200** — `// TODO: Extract from metadata` (git_commit field)
   - Minor. git_commit in HistoryEntry is always None.

## Product Roadmap Status

### ✅ Phase 1 — LSP Client + Graph Quality
- LSP client (rust-analyzer, tsserver, pyright) ✅
- `gid extract --with-lsp` ✅
- Hybrid fallback (LSP → tree-sitter) ✅
- Dangling edges: 1,850 → 0 ✅
- Incremental extract ✅

### ✅ Phase 2 — Storage Upgrade
- SQLite backend (GraphStorage trait) ✅
- YAML → SQLite migration pipeline ✅
- `--backend yaml|sqlite` CLI flag ✅
- Auto-detection (graph.db → SQLite, else YAML) ✅
- graph.yml-first pattern for all commands ✅

### 🟡 Phase 3 — Productization Core (Next)
- [ ] Multi-repo support (global registry + per-repo graph + cross-repo query)
- [ ] MCP server (AI agent integration)
- [ ] LSP server (IDE integration — Cursor/VS Code)
- [x] Community detection → **infomap-rs** published, integrated into `advise`
- [ ] Hybrid search (BM25 + embedding + RRF)
- [ ] Shared function detection (ISS-008)

### ⬜ Phase 4 — GID as a Service
- [ ] Docker image with LSP servers + sandbox
- [ ] API: repo URL → graph → return
- [ ] Multi-tenant + permissions
- [ ] Web UI dashboard
- [ ] SaaS deployment

## Harness / Ritual Status

- **gid-harness**: 15 source files, 6,881 lines. 7-phase pipeline (Phase 1-3 human, Phase 4-7 AI). Complete.
- **Ritual v2**: State machine + executor. Triage-driven review depth. skip_design fix. Workspace detection root fix. Complete.
- **Context assembly**: 5-tier edge ranking, category-based truncation, source loading from disk. 119 tests. Complete.

## Key Completed Features (recent)

| Feature | Commit | Tests Added |
|---------|--------|-------------|
| Integration tests | `4d5c548` | +25 (integration_tests.rs) |
| `gid about` + `--compact` | `4d5c548` | - |
| Cross-layer auto-linking | `4d5c548` | +7 |
| SQLite backend wiring | `8335184` | +339 (context, history, migration, sqlite, bridge) |
| Infomap integration | `2f635a4` | +7 |
| Incremental extract | `e5a98bf` | +25 |
| Scoped design prompts | `e277ae8` | - |
| Zero dangling edges | `c029136` | +17 |
| ISS-009 cross-layer | `0c1f81d` | +21 |
| SqliteStorage | `b747d11` | +11 |
| ISS-010 review depth | `2b3fffd` | - |

## Dependencies (external crates)

- **infomap-rs** v0.1.0 — community detection (published, own crate)
- **tree-sitter** + language grammars — AST parsing
- **rusqlite** 0.32 — SQLite storage (aligned with engramai)
- **serde** + **serde_yaml** — graph serialization
- **clap** — CLI framework
- **tokio** — async runtime (LSP client)

## What's Next

1. **ISS-009 remaining**: `parse_design_yaml()` auto-generate `implements` edges
2. **Phase 3 items**: MCP server is highest value (AI agent integration)
3. **ISS-003 completion**: Replace semantify path heuristics with infomap clustering
4. **ISS-008**: Shared function detection (after Phase 3 foundation)
