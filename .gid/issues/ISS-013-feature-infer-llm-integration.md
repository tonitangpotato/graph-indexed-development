# ISS-013: Feature Infer LLM Integration (CliLlm Deadlock)

## Problem

`gid infer --level feature` hangs when called from RustClaw. The `CliLlm` implementation shells out to `claude` CLI in a detached subprocess — this deadlocks in non-tty environments (no stdin, no interactive prompt).

Feature-level inference requires LLM intelligence (grouping 644 components into ~20-30 features). Component-level naming works without LLM, but the feature abstraction step (`infer_features` + `infer_feature_deps`) needs `SimpleLlm`.

## Root Cause

`CliLlm` is the only `SimpleLlm` implementation. It spawns `claude` as a subprocess — unsuitable for:
- Non-interactive environments (daemon, RustClaw agent loop)
- Environments without `claude` CLI installed
- High-volume calls (629 components = 629 process spawns)

## Fix: Two-Layer Solution

### Layer 1: RustClaw (our use case)

**Inject `RustClawLlm: SimpleLlm` at the call site.**

- RustClaw's `gid_infer` tool creates a `RustClawLlm` struct wrapping its existing Anthropic client
- Passes it to `gid-core::infer_features()` 
- Reuses existing auth (OAuth token, Max plan), connection pool, retry logic
- gid-core doesn't need to know about auth — it just calls `async fn ask(&self, prompt) -> String`

Location: `src/tools/gid.rs` (or wherever `gid_infer` tool is implemented)

### Layer 2: gid CLI (external users)

**Add `api-llm` feature to gid-core with `AnthropicLlm` struct.**

- `ANTHROPIC_API_KEY` env var → direct HTTP to `api.anthropic.com`
- gid CLI binary enables this feature, uses it when key is present
- No fallback to `CliLlm` — clear error if no key configured
- `CliLlm` can be deprecated/removed over time

### Architecture After Fix

```
gid-core (library):
  - SimpleLlm trait (unchanged)
  - AnthropicLlm: SimpleLlm (new, feature-gated "api-llm")
  
gid CLI (binary):
  - Uses AnthropicLlm (ANTHROPIC_API_KEY from env)
  - Clear error message if key not set

RustClaw (binary):
  - Uses RustClawLlm: SimpleLlm (wraps existing client)
  - Auth already configured (OAuth token / Max plan)
```

### Key Design Decisions

1. **No fallback** — explicit configuration, explicit failure
2. **gid-core stays clean** — `SimpleLlm` is a trait, implementations are feature-gated or external
3. **RustClaw uses its own client** — no duplicate auth config, no env vars needed
4. **gid CLI gets standalone API support** — for users who don't use RustClaw

## Priority

High — blocks feature-level inference entirely.

## Implementation Order

1. Layer 1 first (RustClaw — unblocks us immediately)
2. Layer 2 later (gid CLI — for open-source users)
