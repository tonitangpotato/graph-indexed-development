//! API-based LLM client using agentctl-auth.
//!
//! Implements [`LlmClient`] via the shared `agentctl-auth` crate,
//! using OAuth stealth headers for Claude Max Plan.

use std::path::Path;
use std::sync::Arc;
use anyhow::{Context, Result};
use async_trait::async_trait;
use agentctl_auth::claude::{Client as ClaudeClient, ClientBuilder, Tool, ToolHandler, ToolOutput};
use agentctl_auth::pool::AuthPool;
use crate::ritual::llm::{LlmClient, ToolDefinition, SkillResult};

/// Default auth pool path.
fn default_pool_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".agentctl/auth.toml")
}

/// API-based LLM client using agentctl-auth's Claude client.
pub struct ApiLlmClient {
    client: ClaudeClient,
}

impl ApiLlmClient {
    /// Create from the default auth pool (~/.agentctl/auth.toml).
    pub fn from_pool() -> Result<Self> {
        let pool_path = default_pool_path();
        let pool = AuthPool::load(&pool_path)
            .context("Failed to load auth pool")?;
        
        // Verify we have at least one anthropic credential
        let _ = pool.get_default("anthropic")
            .context("No anthropic credential in auth pool")?;
        
        let client = ClientBuilder::new()
            .pool(&pool)
            .build()?;
        
        Ok(Self { client })
    }

    /// Try to create from pool, return None if no pool exists.
    pub fn try_from_pool() -> Option<Self> {
        match Self::from_pool() {
            Ok(client) => {
                tracing::info!("ApiLlmClient: loaded from auth pool");
                Some(client)
            }
            Err(e) => {
                tracing::warn!("ApiLlmClient: failed to load auth pool: {}", e);
                None
            }
        }
    }

    /// Wrap as Arc<dyn LlmClient>.
    pub fn into_arc(self) -> Arc<dyn LlmClient> {
        Arc::new(self)
    }
}

/// Simple tool handler that executes Read/Write/Edit/Bash in a working directory.
struct SkillToolHandler {
    working_dir: std::path::PathBuf,
}

#[async_trait]
impl ToolHandler for SkillToolHandler {
    async fn handle(&self, name: &str, input: &serde_json::Value) -> Result<ToolOutput> {
        match name {
            "Read" => {
                let path = input.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let full_path = self.working_dir.join(path);
                match std::fs::read_to_string(&full_path) {
                    Ok(content) => {
                        if content.len() > 50_000 {
                            Ok(ToolOutput::success(format!("{}\n\n[truncated at 50KB]", &content[..50_000])))
                        } else {
                            Ok(ToolOutput::success(content))
                        }
                    }
                    Err(e) => Ok(ToolOutput::error(format!("Failed to read {}: {}", path, e))),
                }
            }
            "Write" => {
                let path = input.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let content = input.get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let full_path = self.working_dir.join(path);
                if let Some(parent) = full_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&full_path, content) {
                    Ok(_) => Ok(ToolOutput::success(format!("Wrote {} bytes to {}", content.len(), path))),
                    Err(e) => Ok(ToolOutput::error(format!("Failed to write {}: {}", path, e))),
                }
            }
            "Bash" => {
                let command = input.get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let output = std::process::Command::new("bash")
                    .arg("-c")
                    .arg(command)
                    .current_dir(&self.working_dir)
                    .output();
                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let combined = format!("{}{}", stdout, stderr);
                        if out.status.success() {
                            Ok(ToolOutput::success(combined))
                        } else {
                            Ok(ToolOutput::error(format!("Exit {}: {}", out.status, combined)))
                        }
                    }
                    Err(e) => Ok(ToolOutput::error(format!("Failed to exec: {}", e))),
                }
            }
            _ => Ok(ToolOutput::error(format!("Unknown tool: {}", name))),
        }
    }
}

#[async_trait]
impl LlmClient for ApiLlmClient {
    async fn run_skill(
        &self,
        skill_prompt: &str,
        tools: Vec<ToolDefinition>,
        model: &str,
        working_dir: &Path,
    ) -> Result<SkillResult> {
        // Convert ToolDefinition → agentctl-auth Tool
        let api_tools: Vec<Tool> = tools.iter().map(|t| {
            Tool::new(
                &t.name,
                &t.description,
                t.input_schema.clone(),
            )
        }).collect();

        let handler = SkillToolHandler {
            working_dir: working_dir.to_path_buf(),
        };

        // Resolve model aliases to full Anthropic model IDs
        let resolved_model = match model {
            "sonnet" => "claude-sonnet-4-5-20250929",
            "opus" => "claude-opus-4-6",
            "haiku" => "claude-haiku-3-5-20241022",
            other => other,
        };

        // Run agent loop (multi-turn with tool use)
        tracing::info!("ApiLlmClient: starting agent loop with model='{}' (resolved='{}'), tools={}, prompt_len={}", 
            model, resolved_model, api_tools.len(), skill_prompt.len());
        let result = self.client.run_agent_loop(
            resolved_model,
            "You are a development assistant executing a ritual phase. Complete the task and produce the required artifacts.",
            skill_prompt,
            &api_tools,
            20, // max turns
            &handler,
        ).await.map_err(|e| {
            tracing::error!("ApiLlmClient: agent loop failed: {:?}", e);
            e
        }).context("Agent loop failed")?;
        tracing::info!("ApiLlmClient: agent loop completed, {} tool calls, output_len={}", 
            result.tool_calls.len(), result.final_text.len());

        Ok(SkillResult {
            output: result.final_text,
            artifacts_created: vec![], // Artifacts tracked by engine via glob
            tool_calls_made: result.tool_calls.len(),
            tokens_used: result.total_input_tokens + result.total_output_tokens,
        })
    }
}
