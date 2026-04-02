//! Verifier — runs verification commands at task and layer level.
//!
//! Provides three levels of verification:
//! - **Per-task**: runs `metadata.verify` in the worktree before merging
//! - **Layer checkpoint**: runs after all layer tasks merge to main
//! - **Guard checks**: runs invariant checks from `execution.yml`

use std::path::Path;

use anyhow::Result;
use tracing::{info, warn};

use gid_core::harness::types::{TaskInfo, ExecutionLayer, GuardCheck, VerifyResult};

/// Result of a guard check execution.
#[derive(Debug, Clone)]
pub struct GuardResult {
    /// The guard ID (e.g., "GUARD-1").
    pub guard_id: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Actual output from the command.
    pub actual_output: String,
    /// Expected output.
    pub expected_output: String,
}

/// Verifier runs verification commands at task and layer level.
///
/// Task verification happens in the worktree before merge.
/// Layer checkpoint runs on main branch after all merges.
/// Guard checks verify project invariants.
pub struct Verifier {
    /// Default checkpoint command (auto-detected or from config).
    pub default_checkpoint: Option<String>,
    /// Working directory for layer-level verification (main branch).
    pub project_root: std::path::PathBuf,
}

impl Verifier {
    /// Create a new verifier for the given project root.
    pub fn new(project_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            default_checkpoint: None,
            project_root: project_root.into(),
        }
    }

    /// Set the default checkpoint command.
    pub fn with_checkpoint(mut self, checkpoint: impl Into<String>) -> Self {
        self.default_checkpoint = Some(checkpoint.into());
        self
    }

    /// Run a task's verify command in its worktree.
    ///
    /// Returns [`VerifyResult::Pass`] if the command exits 0,
    /// [`VerifyResult::Fail`] otherwise. Returns `Pass` if no verify command.
    pub async fn verify_task(&self, task: &TaskInfo, worktree: &Path) -> Result<VerifyResult> {
        let verify_cmd = match &task.verify {
            Some(cmd) => cmd,
            None => {
                info!(task_id = %task.id, "No verify command, skipping");
                return Ok(VerifyResult::Pass);
            }
        };

        info!(task_id = %task.id, cmd = %verify_cmd, "Running task verification");
        run_shell_command(verify_cmd, worktree).await
    }

    /// Run layer checkpoint on main branch after all merges.
    ///
    /// Uses the layer's checkpoint command, falling back to `default_checkpoint`.
    /// Returns `Pass` if no checkpoint command is configured.
    pub async fn verify_layer(&self, layer: &ExecutionLayer) -> Result<VerifyResult> {
        let checkpoint = layer.checkpoint.as_deref()
            .or(self.default_checkpoint.as_deref());

        let cmd = match checkpoint {
            Some(cmd) => cmd,
            None => {
                info!(layer = layer.index, "No checkpoint command, skipping");
                return Ok(VerifyResult::Pass);
            }
        };

        info!(layer = layer.index, cmd = %cmd, "Running layer checkpoint");
        run_shell_command(cmd, &self.project_root).await
    }

    /// Run guard checks and return results for each.
    ///
    /// Each guard maps to a shell command + expected output.
    /// The check passes if the trimmed command output matches the expected value.
    pub async fn verify_guards(&self, checks: &[(&str, &GuardCheck)]) -> Result<Vec<GuardResult>> {
        let mut results = Vec::new();

        for (guard_id, check) in checks {
            info!(guard_id, cmd = %check.command, "Running guard check");

            let output = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&check.command)
                .current_dir(&self.project_root)
                .output()
                .await?;

            let actual = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let passed = actual == check.expect.trim();

            if !passed {
                warn!(
                    guard_id,
                    expected = %check.expect,
                    actual = %actual,
                    "Guard check FAILED"
                );
            }

            results.push(GuardResult {
                guard_id: guard_id.to_string(),
                passed,
                actual_output: actual,
                expected_output: check.expect.clone(),
            });
        }

        Ok(results)
    }
}

/// Run a shell command in the given directory and return a VerifyResult.
async fn run_shell_command(cmd: &str, dir: &Path) -> Result<VerifyResult> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);
    let exit_code = output.status.code().unwrap_or(-1);

    if output.status.success() {
        info!(cmd, "Verification passed");
        Ok(VerifyResult::Pass)
    } else {
        warn!(cmd, exit_code, output = %combined.trim(), "Verification failed");
        Ok(VerifyResult::Fail {
            output: combined.trim().to_string(),
            exit_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_verify_task_pass() {
        let verifier = Verifier::new("/tmp");
        let task = TaskInfo {
            id: "test".to_string(),
            title: "Test".to_string(),
            description: String::new(),
            goals: vec![],
            verify: Some("true".to_string()), // always succeeds
            estimated_turns: 10,
            depends_on: vec![],
            design_ref: None,
            satisfies: vec![],
        };

        let result = verifier.verify_task(&task, Path::new("/tmp")).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn test_verify_task_fail() {
        let verifier = Verifier::new("/tmp");
        let task = TaskInfo {
            id: "test".to_string(),
            title: "Test".to_string(),
            description: String::new(),
            goals: vec![],
            verify: Some("false".to_string()), // always fails
            estimated_turns: 10,
            depends_on: vec![],
            design_ref: None,
            satisfies: vec![],
        };

        let result = verifier.verify_task(&task, Path::new("/tmp")).await.unwrap();
        assert!(matches!(result, VerifyResult::Fail { .. }));
    }

    #[tokio::test]
    async fn test_verify_task_no_command() {
        let verifier = Verifier::new("/tmp");
        let task = TaskInfo {
            id: "test".to_string(),
            title: "Test".to_string(),
            description: String::new(),
            goals: vec![],
            verify: None,
            estimated_turns: 10,
            depends_on: vec![],
            design_ref: None,
            satisfies: vec![],
        };

        let result = verifier.verify_task(&task, Path::new("/tmp")).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn test_verify_layer_with_checkpoint() {
        let verifier = Verifier::new("/tmp");
        let layer = ExecutionLayer {
            index: 0,
            tasks: vec![],
            checkpoint: Some("echo ok".to_string()),
        };

        let result = verifier.verify_layer(&layer).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn test_verify_layer_no_checkpoint() {
        let verifier = Verifier::new("/tmp");
        let layer = ExecutionLayer {
            index: 0,
            tasks: vec![],
            checkpoint: None,
        };

        let result = verifier.verify_layer(&layer).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn test_verify_guards() {
        let verifier = Verifier::new("/tmp");
        let check = GuardCheck {
            command: "echo 0".to_string(),
            expect: "0".to_string(),
        };

        let results = verifier.verify_guards(&[("GUARD-1", &check)]).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].passed);
        assert_eq!(results[0].guard_id, "GUARD-1");

        // Failing guard
        let bad_check = GuardCheck {
            command: "echo 5".to_string(),
            expect: "0".to_string(),
        };
        let results = verifier.verify_guards(&[("GUARD-2", &bad_check)]).await.unwrap();
        assert!(!results[0].passed);
    }
}
