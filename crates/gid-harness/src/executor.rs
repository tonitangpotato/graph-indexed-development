//! Task executor — trait for spawning sub-agents and CLI-based implementation.
//!
//! The [`TaskExecutor`] trait abstracts sub-agent spawning, allowing different
//! implementations (CLI, API, mock). [`CliExecutor`] spawns the `claude` CLI
//! in a git worktree with a focused prompt (no workspace files).

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;
use tracing::{info, warn};

use gid_core::harness::types::{TaskContext, TaskResult, HarnessConfig};

/// Trait for spawning sub-agents to execute tasks.
///
/// Implementations handle the specifics of how sub-agents are launched
/// (CLI process, API call, in-process mock, etc.).
#[async_trait]
pub trait TaskExecutor: Send + Sync {
    /// Spawn a sub-agent for the given task in the specified worktree.
    ///
    /// Returns a [`TaskResult`] capturing success/failure, output, and usage stats.
    /// Sub-agent failures are data (returned as `TaskResult { success: false, .. }`),
    /// not panics. Only infrastructure errors (process spawn failure, etc.) return `Err`.
    async fn spawn(
        &self,
        context: &TaskContext,
        worktree_path: &Path,
        config: &HarnessConfig,
    ) -> Result<TaskResult>;
}

/// CLI-based executor that spawns `claude` CLI as sub-agents.
///
/// Each task gets a focused system prompt with only the task context
/// (no SOUL.md, AGENTS.md, USER.md, MEMORY.md — GUARD-12).
#[derive(Debug, Clone)]
pub struct CliExecutor {
    /// Path to the claude CLI binary (default: "claude").
    pub claude_bin: String,
}

impl Default for CliExecutor {
    fn default() -> Self {
        Self {
            claude_bin: "claude".to_string(),
        }
    }
}

impl CliExecutor {
    /// Create a new CLI executor with the default claude binary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a CLI executor with a custom binary path.
    pub fn with_binary(bin: impl Into<String>) -> Self {
        Self {
            claude_bin: bin.into(),
        }
    }

    /// Build the sub-agent prompt from task context.
    ///
    /// The prompt is focused and minimal — no workspace files loaded (GUARD-12).
    /// Contains: task info, goals, design context, guards, verify command.
    pub fn build_prompt(context: &TaskContext) -> String {
        let mut prompt = String::new();

        prompt.push_str("You are a focused coding agent executing a single task.\n\n");

        // Task
        prompt.push_str(&format!("## Your Task\n{}\n\n", context.task_info.title));

        // Description
        if !context.task_info.description.is_empty() {
            prompt.push_str(&format!("## Description\n{}\n\n", context.task_info.description));
        }

        // Goals
        if !context.goals_text.is_empty() {
            prompt.push_str("## Goals\n");
            for goal in &context.goals_text {
                prompt.push_str(&format!("- {}\n", goal));
            }
            prompt.push('\n');
        }

        // Design context
        if let Some(ref excerpt) = context.design_excerpt {
            prompt.push_str(&format!("## Design Context\n{}\n\n", excerpt));
        }

        // Dependency interfaces
        if !context.dependency_interfaces.is_empty() {
            prompt.push_str("## Dependency Interfaces\n");
            for iface in &context.dependency_interfaces {
                prompt.push_str(&format!("- {}\n", iface));
            }
            prompt.push('\n');
        }

        // Guards
        if !context.guards.is_empty() {
            prompt.push_str("## Project Guards (must never be violated)\n");
            for guard in &context.guards {
                prompt.push_str(&format!("- {}\n", guard));
            }
            prompt.push('\n');
        }

        // Verify command
        if let Some(ref verify) = context.task_info.verify {
            prompt.push_str(&format!("## Verify Command\n{}\n\n", verify));
        }

        // Rules
        prompt.push_str("## Rules\n");
        prompt.push_str("1. Stay focused — only implement what's described above\n");
        prompt.push_str("2. Be efficient — write code directly, don't read files unless needed\n");
        prompt.push_str("3. Don't modify .gid/ — graph is managed by the harness\n");
        prompt.push_str("4. Self-test — run the verify command yourself before finishing\n");
        prompt.push_str("5. Report blockers — if you can't complete due to missing dependency, say so clearly\n");

        prompt
    }

    /// Parse the sub-agent output to detect blockers.
    fn detect_blocker(output: &str) -> Option<String> {
        let lower = output.to_lowercase();
        if lower.contains("blocker:") || lower.contains("blocked by") || lower.contains("cannot proceed") {
            // Extract the blocker line
            for line in output.lines() {
                let ll = line.to_lowercase();
                if ll.contains("blocker:") || ll.contains("blocked by") || ll.contains("cannot proceed") {
                    return Some(line.trim().to_string());
                }
            }
            Some("Sub-agent reported a blocker (details in output)".to_string())
        } else {
            None
        }
    }
}

#[async_trait]
impl TaskExecutor for CliExecutor {
    async fn spawn(
        &self,
        context: &TaskContext,
        worktree_path: &Path,
        config: &HarnessConfig,
    ) -> Result<TaskResult> {
        let prompt = Self::build_prompt(context);
        let start = Instant::now();

        info!(
            task_id = %context.task_info.id,
            worktree = %worktree_path.display(),
            model = %config.model,
            "Spawning sub-agent via CLI"
        );

        // Build command: claude --print --model <model> --max-turns <n> -p "<prompt>"
        let output = tokio::process::Command::new(&self.claude_bin)
            .arg("--print")
            .arg("--model")
            .arg(&config.model)
            .arg("--max-turns")
            .arg(config.max_iterations.to_string())
            .arg("--permission-mode")
            .arg("bypassPermissions")
            .arg("-p")
            .arg(&prompt)
            .current_dir(worktree_path)
            .output()
            .await?;

        let _duration = start.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let success = output.status.success();
        let combined_output = if stderr.is_empty() {
            stdout.clone()
        } else {
            format!("{}\n--- stderr ---\n{}", stdout, stderr)
        };

        let blocker = Self::detect_blocker(&combined_output);

        if !success {
            warn!(
                task_id = %context.task_info.id,
                exit_code = ?output.status.code(),
                "Sub-agent exited with non-zero status"
            );
        }

        // Note: turns_used and tokens_used are approximations.
        // The CLI doesn't expose these directly; a future API-based executor
        // could provide exact counts.
        Ok(TaskResult {
            success,
            output: combined_output,
            turns_used: 0,  // CLI doesn't report turns
            tokens_used: 0, // CLI doesn't report tokens
            blocker,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gid_core::harness::types::TaskInfo;

    fn sample_context() -> TaskContext {
        TaskContext {
            task_info: TaskInfo {
                id: "auth-impl".to_string(),
                title: "Implement auth module".to_string(),
                description: "Create src/auth.rs with login/logout functions".to_string(),
                goals: vec!["GOAL-1.1".to_string()],
                verify: Some("cargo test --test auth".to_string()),
                estimated_turns: 15,
                depends_on: vec!["config-module".to_string()],
                design_ref: Some("3.2".to_string()),
                satisfies: vec!["GOAL-1.1".to_string()],
            },
            goals_text: vec!["GOAL-1.1: Users can authenticate with API key".to_string()],
            design_excerpt: Some("Section 3.2: Auth module handles token storage".to_string()),
            dependency_interfaces: vec!["config::load() -> Result<Config>".to_string()],
            guards: vec!["GUARD-1: All file writes are atomic".to_string()],
        }
    }

    #[test]
    fn test_build_prompt_includes_all_sections() {
        let ctx = sample_context();
        let prompt = CliExecutor::build_prompt(&ctx);

        assert!(prompt.contains("Implement auth module"), "should contain task title");
        assert!(prompt.contains("src/auth.rs"), "should contain description");
        assert!(prompt.contains("GOAL-1.1"), "should contain goals");
        assert!(prompt.contains("Section 3.2"), "should contain design excerpt");
        assert!(prompt.contains("config::load()"), "should contain dependency interfaces");
        assert!(prompt.contains("GUARD-1"), "should contain guards");
        assert!(prompt.contains("cargo test --test auth"), "should contain verify command");
        assert!(prompt.contains("Stay focused"), "should contain rules");
    }

    #[test]
    fn test_build_prompt_no_workspace_files() {
        let ctx = sample_context();
        let prompt = CliExecutor::build_prompt(&ctx);

        // GUARD-12: No workspace files in sub-agent prompt
        assert!(!prompt.contains("SOUL.md"), "must not reference SOUL.md");
        assert!(!prompt.contains("AGENTS.md"), "must not reference AGENTS.md");
        assert!(!prompt.contains("USER.md"), "must not reference USER.md");
        assert!(!prompt.contains("MEMORY.md"), "must not reference MEMORY.md");
    }

    #[test]
    fn test_detect_blocker() {
        assert!(CliExecutor::detect_blocker("I'm stuck. Blocker: missing config module").is_some());
        assert!(CliExecutor::detect_blocker("Cannot proceed without the auth API").is_some());
        assert!(CliExecutor::detect_blocker("Blocked by missing dependency X").is_some());
        assert!(CliExecutor::detect_blocker("Task completed successfully").is_none());
    }

    #[test]
    fn test_build_prompt_handles_empty_context() {
        let ctx = TaskContext {
            task_info: TaskInfo {
                id: "simple".to_string(),
                title: "Simple task".to_string(),
                description: String::new(),
                goals: vec![],
                verify: None,
                estimated_turns: 10,
                depends_on: vec![],
                design_ref: None,
                satisfies: vec![],
            },
            goals_text: vec![],
            design_excerpt: None,
            dependency_interfaces: vec![],
            guards: vec![],
        };
        let prompt = CliExecutor::build_prompt(&ctx);
        assert!(prompt.contains("Simple task"));
        assert!(prompt.contains("Rules"));
    }
}
