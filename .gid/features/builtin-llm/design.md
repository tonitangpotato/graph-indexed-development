# Feature: Built-in LLM Backend for gid-core

## Problem

`SimpleLlm` trait exists in `gid-core::infer::labeling` but has **zero production implementations** in gid-core itself. Every consumer (gid-cli, RustClaw) copy-pastes its own `CliSimpleLlm` struct. This causes:

1. **Code duplication** — identical `CliSimpleLlm` in gid-cli/main.rs and RustClaw/tools.rs
2. **Silent failures** — each copy swallows errors differently (or not at all)
3. **Distribution blocker** — gid-core can't be used standalone for GidHub/MCP without consumers bringing their own LLM glue
4. **Debug noise** — `eprintln!` scattered through labeling.rs instead of proper `tracing`

## Solution

Move `CliLlm` into gid-core as the built-in `SimpleLlm` implementation behind a feature gate. Consumers that have their own LLM infra (RustClaw) impl the trait themselves; everyone else uses `CliLlm`.

## Architecture

```
gid-core (crate)
├── SimpleLlm              // trait (already exists)
├── CliLlm                 // impl SimpleLlm — calls `claude` CLI
│                          //   feature-gated: "cli-llm"
└── (trait remains open for external impls)

Consumers:
  gid-cli      → uses gid_core::CliLlm (delete local copy)
  gid-mcp      → uses gid_core::CliLlm
  GidHub early → uses gid_core::CliLlm
  GidHub prod  → custom impl SimpleLlm (shared LLM pool)
  RustClaw     → custom impl SimpleLlm (existing Anthropic client)
```

## Scope

### 1. New file: `crates/gid-core/src/infer/llm.rs`

```rust
//! Built-in SimpleLlm implementations for gid-core.
//!
//! Feature-gated behind `cli-llm`. Provides CliLlm which shells out
//! to the `claude` CLI for LLM completions.

use anyhow::{bail, Context, Result};
use super::labeling::SimpleLlm;

/// SimpleLlm implementation that calls the `claude` CLI.
///
/// Suitable for local development and lightweight deployment.
/// Requires `claude` CLI installed and authenticated.
///
/// # Example
/// ```no_run
/// use gid_core::infer::CliLlm;
/// let llm = CliLlm::new("haiku");
/// // Pass to infer pipeline
/// ```
pub struct CliLlm {
    model: String,
}

impl CliLlm {
    pub fn new(model: impl Into<String>) -> Self {
        Self { model: model.into() }
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

#[async_trait::async_trait]
impl SimpleLlm for CliLlm {
    async fn complete(&self, prompt: &str) -> Result<String> {
        tracing::debug!(model = %self.model, prompt_len = prompt.len(), "CliLlm: calling claude CLI");

        let output = tokio::process::Command::new("claude")
            .arg("-p")
            .arg(prompt)
            .arg("--model")
            .arg(&self.model)
            .output()
            .await
            .context("Failed to run `claude` CLI. Is it installed? Run: npm install -g @anthropic-ai/claude-code")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("claude CLI failed (exit {}): {}", output.status, stderr);
        }

        let response = String::from_utf8_lossy(&output.stdout).to_string();
        tracing::debug!(response_len = response.len(), "CliLlm: got response");
        Ok(response)
    }
}
```

### 2. Feature gate in `Cargo.toml`

```toml
[features]
cli-llm = ["infomap", "dep:tokio", "dep:async-trait"]
# infomap already pulls async-trait, but be explicit
full = ["ritual", "sqlite", "infomap", "cli-llm"]
```

### 3. Wire into `infer/mod.rs`

```rust
#[cfg(feature = "cli-llm")]
mod llm;
#[cfg(feature = "cli-llm")]
pub use llm::CliLlm;
```

### 4. Delete duplicates

- **gid-cli/main.rs**: Remove `CliSimpleLlm` struct + impl. Replace with `use gid_core::infer::CliLlm`.
- **RustClaw/tools.rs**: If it has a `CliSimpleLlm`, remove it. RustClaw should impl `SimpleLlm` with its own Anthropic client (separate task, not this feature).

### 5. Clean up labeling.rs

- Replace all `eprintln!` with `tracing::debug!` / `tracing::warn!`
- Ensure LLM errors are logged at `warn` level, not silently swallowed
- The `Err(_e)` in naming fallback should log: `tracing::warn!(error = %_e, component = %id, "LLM naming failed, using fallback")`

## Dependencies

- `tokio` (already available via infomap feature's async-trait usage)
- `async-trait` (already a dep for infomap)
- `tracing` (already a dep)
- No new external dependencies

## Non-Goals

- **AnthropicLlm (HTTP client)**: Not in this PR. When GidHub needs it, add `anthropic-llm` feature with reqwest. CliLlm is sufficient for now.
- **OpenAI/other providers**: Future work. Trait is open for extension.
- **RustClaw adapter**: RustClaw impls SimpleLlm on its side. Not a gid-core change.

## Verification

1. `cargo build --features cli-llm` — compiles
2. `cargo build --features full` — compiles (full includes cli-llm)
3. `cargo build` (default features) — compiles without cli-llm deps
4. gid-cli uses `gid_core::infer::CliLlm` — no local CliSimpleLlm
5. `gid infer --level feature` on a real repo — LLM labeling works, errors logged via tracing not eprintln
6. All existing tests pass

## Files Changed

| File | Change |
|------|--------|
| `crates/gid-core/src/infer/llm.rs` | NEW — CliLlm implementation |
| `crates/gid-core/src/infer/mod.rs` | Add `mod llm` + re-export |
| `crates/gid-core/src/infer/labeling.rs` | eprintln → tracing, better error logging |
| `crates/gid-core/Cargo.toml` | Add `cli-llm` feature |
| `crates/gid-cli/src/main.rs` | Delete CliSimpleLlm, use gid_core::infer::CliLlm |
