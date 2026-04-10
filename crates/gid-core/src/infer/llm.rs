//! Built-in [`SimpleLlm`] implementations for gid-core.
//!
//! Feature-gated behind `cli-llm`. Provides [`CliLlm`] which shells out
//! to the `claude` CLI for LLM completions.
//!
//! # Why `cli-llm` exists
//!
//! The `SimpleLlm` trait is intentionally minimal — a single `complete(prompt) → String`.
//! Consumers with their own LLM infrastructure (e.g. RustClaw with its Anthropic client,
//! GidHub with a shared LLM pool) implement the trait directly. `CliLlm` is the built-in
//! "batteries included" implementation for everyone else: gid-cli, gid-mcp, quick prototypes.
//!
//! # Adding new backends
//!
//! Future backends (e.g. `AnthropicLlm` with raw HTTP, `OllamaLlm` for local models)
//! can be added here behind their own feature gates.

use anyhow::{bail, Context, Result};

use super::labeling::SimpleLlm;

/// [`SimpleLlm`] implementation that calls the `claude` CLI.
///
/// Suitable for local development and lightweight deployment.
/// Requires the `claude` CLI to be installed and authenticated.
///
/// ```no_run
/// # use gid_core::infer::CliLlm;
/// let llm = CliLlm::new("haiku");
/// // Pass to infer pipeline:
/// // infer::run(&graph, &config, Some(&llm)).await?;
/// ```
///
/// # Model names
///
/// The `model` parameter is passed directly to `claude --model`. Common values:
/// - `"haiku"` — fast and cheap, good for labeling
/// - `"sonnet"` — balanced
/// - `"opus"` — highest quality
pub struct CliLlm {
    model: String,
}

impl CliLlm {
    /// Create a new `CliLlm` targeting the given model.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }

    /// Returns the configured model name.
    pub fn model(&self) -> &str {
        &self.model
    }
}

#[async_trait::async_trait]
impl SimpleLlm for CliLlm {
    async fn complete(&self, prompt: &str) -> Result<String> {
        tracing::debug!(
            model = %self.model,
            prompt_len = prompt.len(),
            "CliLlm: calling claude CLI"
        );

        let output = tokio::process::Command::new("claude")
            .arg("-p")
            .arg(prompt)
            .arg("--model")
            .arg(&self.model)
            .output()
            .await
            .context(
                "Failed to run `claude` CLI. Is it installed? \
                 Run: npm install -g @anthropic-ai/claude-code",
            )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("claude CLI failed (exit {}): {}", output.status, stderr);
        }

        let response = String::from_utf8_lossy(&output.stdout).to_string();
        tracing::debug!(response_len = response.len(), "CliLlm: got response");
        Ok(response)
    }
}
