//! LLM Client trait for ritual phase execution.
//!
//! Defines the interface for running agentic LLM sessions with tools.
//! Implementations are provided by the agent runtime (RustClaw, gidterm, etc.),
//! keeping gid-core free of LLM provider dependencies.

use std::path::{Path, PathBuf};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Definition of a tool available to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (e.g., "Read", "Write", "Bash").
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema defining the tool's input parameters.
    pub input_schema: serde_json::Value,
}

impl ToolDefinition {
    /// Create a new tool definition.
    pub fn new(name: impl Into<String>, description: impl Into<String>, input_schema: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// Result from running a skill (agentic loop).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillResult {
    /// Final output/summary from the LLM.
    pub output: String,
    /// Paths to artifacts created during execution.
    pub artifacts_created: Vec<PathBuf>,
    /// Number of tool calls made during execution.
    pub tool_calls_made: usize,
    /// Total tokens consumed (input + output).
    pub tokens_used: u64,
}

impl SkillResult {
    /// Create a successful skill result.
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            artifacts_created: vec![],
            tool_calls_made: 0,
            tokens_used: 0,
        }
    }

    /// Create a skill result with artifacts.
    pub fn with_artifacts(mut self, artifacts: Vec<PathBuf>) -> Self {
        self.artifacts_created = artifacts;
        self
    }

    /// Set the number of tool calls made.
    pub fn with_tool_calls(mut self, count: usize) -> Self {
        self.tool_calls_made = count;
        self
    }

    /// Set the tokens used.
    pub fn with_tokens(mut self, tokens: u64) -> Self {
        self.tokens_used = tokens;
        self
    }
}

/// Trait for LLM clients that can run agentic skill sessions.
///
/// This trait abstracts the LLM provider and allows gid-core to orchestrate
/// skill execution without depending on specific LLM implementations.
///
/// # Example
///
/// ```ignore
/// struct MyLlmClient { /* ... */ }
///
/// #[async_trait]
/// impl LlmClient for MyLlmClient {
///     async fn run_skill(
///         &self,
///         skill_prompt: &str,
///         tools: Vec<ToolDefinition>,
///         model: &str,
///         working_dir: &Path,
///     ) -> Result<SkillResult> {
///         // Run agentic loop with tools until completion
///         // ...
///     }
/// }
/// ```
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Run a skill — an agentic loop with tools until completion.
    ///
    /// # Arguments
    ///
    /// * `skill_prompt` - The skill's system prompt (from SKILL.md or template)
    /// * `tools` - Tool definitions available to the LLM (pre-filtered by ToolScope)
    /// * `model` - Model identifier (e.g., "sonnet", "opus")
    /// * `working_dir` - Directory to run the skill in
    ///
    /// # Returns
    ///
    /// A `SkillResult` containing the output, artifacts, and usage statistics.
    async fn run_skill(
        &self,
        skill_prompt: &str,
        tools: Vec<ToolDefinition>,
        model: &str,
        working_dir: &Path,
    ) -> Result<SkillResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definition_new() {
        let tool = ToolDefinition::new(
            "Read",
            "Read a file",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        );

        assert_eq!(tool.name, "Read");
        assert_eq!(tool.description, "Read a file");
    }

    #[test]
    fn test_skill_result_builder() {
        let result = SkillResult::success("Done")
            .with_artifacts(vec![PathBuf::from("output.txt")])
            .with_tool_calls(5)
            .with_tokens(1000);

        assert_eq!(result.output, "Done");
        assert_eq!(result.artifacts_created, vec![PathBuf::from("output.txt")]);
        assert_eq!(result.tool_calls_made, 5);
        assert_eq!(result.tokens_used, 1000);
    }
}
