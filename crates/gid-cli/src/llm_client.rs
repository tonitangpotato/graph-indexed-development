//! CLI-based LLM client for ritual phase execution.
//!
//! Implements [`LlmClient`] by shelling out to the `claude` CLI.

use std::path::Path;
use std::sync::Arc;
use anyhow::{Context, Result};
use async_trait::async_trait;
use gid_core::ritual::llm::{LlmClient, ToolDefinition, SkillResult};

/// CLI-based LLM client that shells out to `claude -p`.
pub struct CliLlmClient {
    /// Path to the claude CLI binary.
    claude_bin: String,
}

impl Default for CliLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CliLlmClient {
    /// Create a new CLI LLM client with the default `claude` binary.
    pub fn new() -> Self {
        Self {
            claude_bin: "claude".to_string(),
        }
    }

    /// Create a CLI LLM client with a custom binary path.
    #[allow(dead_code)]
    pub fn with_binary(bin: impl Into<String>) -> Self {
        Self {
            claude_bin: bin.into(),
        }
    }

    /// Wrap as an Arc<dyn LlmClient> for use with RitualEngine.
    pub fn into_arc(self) -> Arc<dyn LlmClient> {
        Arc::new(self)
    }
}

#[async_trait]
impl LlmClient for CliLlmClient {
    async fn run_skill(
        &self,
        skill_prompt: &str,
        tools: Vec<ToolDefinition>,
        model: &str,
        working_dir: &Path,
    ) -> Result<SkillResult> {
        // Defensive validation: ensure tool names are safe (no commas, control chars)
        for tool in &tools {
            if tool.name.contains(',') || tool.name.chars().any(|c| c.is_control()) {
                anyhow::bail!("Invalid tool name: '{}' (contains comma or control character)", tool.name);
            }
        }

        // Defensive validation: prompt shouldn't start with "--" (could be misinterpreted as flag)
        if skill_prompt.trim().starts_with("--") {
            eprintln!("Warning: Skill prompt starts with '--', may cause CLI parsing issues");
        }

        // Build allowed tools list from ToolDefinition names
        let allowed_tools: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

        // Build command: claude -p "<prompt>" --model <model> [--allowedTools ...]
        let mut cmd = tokio::process::Command::new(&self.claude_bin);
        cmd.arg("-p").arg(skill_prompt);
        cmd.arg("--model").arg(model);

        if !allowed_tools.is_empty() {
            cmd.arg("--allowedTools").arg(allowed_tools.join(","));
        }

        cmd.current_dir(working_dir);

        let output = cmd
            .output()
            .await
            .context("Failed to spawn claude CLI")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Parse usage statistics from stderr if available
        let (tool_calls, tokens) = parse_usage_stats(&stderr);

        // Combine output
        let combined_output = if stderr.is_empty() {
            stdout
        } else if !output.status.success() {
            format!("{}\n--- stderr ---\n{}", stdout, stderr)
        } else {
            stdout
        };

        // Scan for artifacts (files that might have been created/modified)
        // For now, just return the output — artifact tracking is handled by the engine
        Ok(SkillResult {
            output: combined_output,
            artifacts_created: vec![],
            tool_calls_made: tool_calls,
            tokens_used: tokens,
        })
    }
}

/// Parse usage statistics from claude CLI stderr output.
fn parse_usage_stats(stderr: &str) -> (usize, u64) {
    let mut tool_calls: usize = 0;
    let mut tokens: u64 = 0;

    for line in stderr.lines() {
        let lower = line.to_lowercase();
        // Parse "Total tokens: 12,345" or "tokens: 12345"
        if lower.contains("token") {
            if let Some(num) = extract_number(line) {
                tokens = num;
            }
        }
        // Parse tool call counts if present
        if lower.contains("tool") && (lower.contains("call") || lower.contains("use")) {
            if let Some(num) = extract_number(line) {
                tool_calls = num as usize;
            }
        }
    }

    (tool_calls, tokens)
}

/// Extract the last number from a string (handles commas).
fn extract_number(s: &str) -> Option<u64> {
    // Find first contiguous digit sequence (with optional commas like 1,234)
    let mut start = None;
    let mut end = 0;
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_digit() {
            if start.is_none() {
                start = Some(i);
            }
            end = i + 1;
        } else if c == ',' && start.is_some() {
            // Allow commas within numbers (e.g. "1,234")
        } else if start.is_some() {
            break;
        }
    }
    let start = start?;
    let cleaned: String = s[start..end].chars().filter(|c| *c != ',').collect();
    cleaned.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_usage_stats() {
        let stderr = "Total tokens: 12,345\nTool calls: 5";
        let (tool_calls, tokens) = parse_usage_stats(stderr);
        assert_eq!(tokens, 12345);
        assert_eq!(tool_calls, 5);
    }

    #[test]
    fn test_extract_number() {
        assert_eq!(extract_number("Total: 1,234"), Some(1234));
        assert_eq!(extract_number("count: 42"), Some(42));
        assert_eq!(extract_number("no number here"), None);
    }
}
