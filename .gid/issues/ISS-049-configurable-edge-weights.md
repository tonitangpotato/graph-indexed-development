# ISS-049: Clustering Edge Weights Should Be Configurable

> **Renumbering note (2026-04-26):** Originally filed as ISS-002 on `iss-001-002-revive` branch, accidentally reusing a closed issue number. Renumbered to ISS-049 to preserve the historical ISS-002 (LSP client / receiver-type matching). Content unchanged.


**Status:** closed
**Reported:** 2026-04-05
**Closed:** 2026-04-26

## Problem

`build_network()` in `infer/clustering.rs` hardcodes edge type weights:

```rust
"calls" => 1.0,
"imports" => 0.8,
"inherits" | "implements" => 0.5,
"defined_in" | "contains" | "belongs_to" => 0.2,
"depends_on" => 0.4,
_ => 0.3,
```

These values are arbitrary (no empirical basis) and directly affect clustering quality. Users cannot tune them per project. Different language ecosystems (Rust vs TypeScript vs Python) likely need different weight profiles.

## Proposed Solution

1. Add `edge_weights: HashMap<String, f64>` to `ClusterConfig`
2. Default values = current hardcoded values
3. User can override via `gid infer --edge-weight calls=1.5 --edge-weight imports=0.5` or config file
4. Consider language-specific presets (e.g., `--preset rust`, `--preset typescript`)

## Impact

- `crates/gid-core/src/infer/clustering.rs` — `build_network()` reads from config instead of hardcoded match
- `ClusterConfig` struct gains new field
- CLI gains new flag or config section

## Priority

P1 — Directly blocks clustering quality tuning. Without this, improving clustering requires code changes.

## Resolution (2026-04-26)

Implemented as designed:

- **`ClusterConfig`** gained `pub edge_weights: HashMap<String, f64>` (defaults from new `default_edge_weights()` helper, mirroring the previous hardcoded `relation_weight()` table).
- **`build_network(graph, config)`** now reads weights from `config.edge_weights.get(relation).copied().unwrap_or(0.0)` instead of calling `relation_weight()`. Unknown relations are skipped (weight 0). Signature change rippled through 33 unit tests + 2 internal callers (`cluster()`, `infer::mod`) + `advise::detect_code_modules()`.
- **`relation_weight()`** retained as a backwards-compatible convenience wrapper (still used by docs / external tooling); `default_edge_weights()` is now the source of truth for the default map.
- **CLI**: added `gid infer --edge-weight RELATION=WEIGHT` (repeatable). Rejects malformed (`abc`), negative, and non-finite values up front.

**Tests added (3):**
- `test_build_network_respects_edge_weight_overrides` — inverted ranking proves overrides flow through.
- `test_build_network_zero_weight_skips_edge` — `weight=0` actually disables a relation.
- `test_default_edge_weights_matches_relation_weight` — guards backward compatibility between the new map and the legacy function.

**Verification:** full workspace `cargo test --features infomap` → 1226/1226 pass.

**Deferred (not part of this issue):**
- Language-specific presets (`--preset rust`, `--preset typescript`) — separate issue if/when empirical data justifies preset profiles.
- Config-file source for weights (`.gid/cluster.toml`) — current CLI flag covers the primary use case; config-file support can come with a broader settings overhaul.

## Status: Closed
