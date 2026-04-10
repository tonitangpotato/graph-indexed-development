# ISS-002: Clustering Edge Weights Should Be Configurable

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

## Status: Open
