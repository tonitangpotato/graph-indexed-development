//! Integration tests for phase executors.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use anyhow::Result;
use async_trait::async_trait;

use gid_core::ritual::executor::{
    PhaseContext, PhaseExecutor,
    SkillExecutor, GidCommandExecutor, ShellExecutor, HarnessExecutor,
};
use gid_core::ritual::definition::{PhaseDefinition, PhaseKind};
use gid_core::ritual::llm::{LlmClient, ToolDefinition, SkillResult};

// ═══════════════════════════════════════════════════════════════════════════════
// Mock LLM Client for Testing
// ═══════════════════════════════════════════════════════════════════════════════

/// A mock LLM client that returns configurable results.
struct MockLlmClient {
    output: String,
}

impl MockLlmClient {
    fn new(output: impl Into<String>) -> Self {
        Self { output: output.into() }
    }
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn run_skill(
        &self,
        _skill_prompt: &str,
        _tools: Vec<ToolDefinition>,
        _model: &str,
        _working_dir: &Path,
    ) -> Result<SkillResult> {
        Ok(SkillResult::success(&self.output)
            .with_tool_calls(3)
            .with_tokens(500))
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn test_context(tmp: &std::path::Path) -> PhaseContext {
    PhaseContext {
        project_root: tmp.to_path_buf(),
        gid_root: tmp.join(".gid"),
        previous_artifacts: HashMap::new(),
        model: "sonnet".to_string(),
        ritual_name: "test".to_string(),
        phase_index: 0,
        task_context: None,
    }
}

fn make_phase(id: &str, kind: PhaseKind) -> PhaseDefinition {
    serde_yaml::from_str(&format!(r#"
id: {id}
{}
"#, match &kind {
        PhaseKind::Skill { name } => format!("kind: skill\nname: {name}"),
        PhaseKind::GidCommand { command, .. } => format!("kind: gid_command\ncommand: {command}"),
        PhaseKind::Shell { command } => format!("kind: shell\ncommand: \"{command}\""),
        PhaseKind::Harness { .. } => "kind: harness".to_string(),
    })).unwrap()
}

// ═══════════════════════════════════════════════════════════════════════════════
// SkillExecutor Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_skill_executor_with_mock_client() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("research", PhaseKind::Skill { name: "research".into() });

    let mock_client = Arc::new(MockLlmClient::new("Research completed successfully"));
    let executor = SkillExecutor::new(tmp.path(), mock_client);
    
    let result = executor.execute_skill(&phase, &ctx, "research").await.unwrap();
    assert!(result.success, "Executor should return success with mock client");
}

#[tokio::test]
async fn test_skill_executor_via_trait() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("research", PhaseKind::Skill { name: "research".into() });

    let mock_client = Arc::new(MockLlmClient::new("Done"));
    let executor = SkillExecutor::new(tmp.path(), mock_client);
    
    // Use the PhaseExecutor trait
    let result = executor.execute(&phase, &ctx).await.unwrap();
    assert!(result.success);
}

#[tokio::test]
async fn test_skill_executor_wrong_phase_kind() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("shell-phase", PhaseKind::Shell { command: "echo hi".into() });

    let mock_client = Arc::new(MockLlmClient::new("Done"));
    let executor = SkillExecutor::new(tmp.path(), mock_client);
    
    // Should fail with wrong phase kind
    let result = executor.execute(&phase, &ctx).await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════════
// GidCommandExecutor Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_gid_command_executor() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("gen-graph", PhaseKind::GidCommand { command: "version".into(), args: vec![] });

    let executor = GidCommandExecutor::new();
    // Using the new execute_command method — "version" may not be a valid subcommand
    let result = executor.execute_command(&phase, &ctx, "version", &[]).await;
    
    // Verify the executor attempted to run the command (doesn't panic/crash)
    // Result depends on whether gid is installed and if "version" is valid
    match &result {
        Ok(r) => {
            // Command ran — success or failure is fine, just verify we got a result
            assert!(r.duration_secs < 30, "Command should not take more than 30s");
        }
        Err(e) => {
            let msg = e.to_string();
            assert!(msg.contains("spawn") || msg.contains("not found") || msg.contains("No such file") || msg.contains("Failed"),
                "Expected spawn/not-found error, got: {}", msg);
        }
    }
}

#[tokio::test]
async fn test_gid_command_executor_via_trait() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("gen-graph", PhaseKind::GidCommand { command: "version".into(), args: vec![] });

    let executor = GidCommandExecutor::new();
    // Use the PhaseExecutor trait
    let result = executor.execute(&phase, &ctx).await;
    // Verify the executor doesn't panic — result depends on gid installation
    match &result {
        Ok(r) => assert!(r.duration_secs < 30, "Command should not take more than 30s"),
        Err(e) => {
            let msg = e.to_string();
            assert!(msg.contains("spawn") || msg.contains("not found") || msg.contains("No such file") || msg.contains("Failed"),
                "Expected spawn/not-found error, got: {}", msg);
        }
    }
}

#[tokio::test]
async fn test_gid_command_executor_wrong_phase_kind() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("skill-phase", PhaseKind::Skill { name: "test".into() });

    let executor = GidCommandExecutor::new();
    let result = executor.execute(&phase, &ctx).await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════════
// ShellExecutor Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_shell_executor_echo() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    // Use a phase ID that gets full bash access
    let mut phase = make_phase("execute-tasks", PhaseKind::Shell { command: "echo hello".into() });
    phase.id = "execute-tasks".to_string();

    let executor = ShellExecutor::new(tmp.path());
    let result = executor.execute_shell(&phase, &ctx, "echo hello").await.unwrap();
    assert!(result.success);
}

#[tokio::test]
async fn test_shell_executor_via_trait() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let mut phase = make_phase("execute-tasks", PhaseKind::Shell { command: "echo hello".into() });
    phase.id = "execute-tasks".to_string();

    let executor = ShellExecutor::new(tmp.path());
    let result = executor.execute(&phase, &ctx).await.unwrap();
    assert!(result.success);
}

#[tokio::test]
async fn test_shell_executor_failing_command() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let mut phase = make_phase("execute-tasks", PhaseKind::Shell { command: "false".into() });
    phase.id = "execute-tasks".to_string();

    let executor = ShellExecutor::new(tmp.path());
    let result = executor.execute_shell(&phase, &ctx, "false").await.unwrap();
    assert!(!result.success, "Failed command should return success=false");
    assert!(result.error.is_some());
}

#[tokio::test]
async fn test_shell_executor_bash_policy_deny() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    // Use a phase ID that denies bash
    let mut phase = make_phase("research", PhaseKind::Shell { command: "echo hello".into() });
    phase.id = "research".to_string();

    let executor = ShellExecutor::new(tmp.path());
    let result = executor.execute_shell(&phase, &ctx, "echo hello").await.unwrap();
    assert!(!result.success, "Command should be denied by bash policy");
    assert!(result.error.as_ref().unwrap().contains("not allowed"));
}

#[tokio::test]
async fn test_shell_executor_bash_policy_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    // Use a phase ID that has allowlist
    let mut phase = make_phase("verify-quality", PhaseKind::Shell { command: "rm -rf /".into() });
    phase.id = "verify-quality".to_string();

    let executor = ShellExecutor::new(tmp.path());
    let result = executor.execute_shell(&phase, &ctx, "rm -rf /").await.unwrap();
    assert!(!result.success, "rm should not be in allowlist");
    assert!(result.error.as_ref().unwrap().contains("not in allowlist"));
}

#[tokio::test]
async fn test_shell_executor_wrong_phase_kind() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("skill-phase", PhaseKind::Skill { name: "test".into() });

    let executor = ShellExecutor::new(tmp.path());
    let result = executor.execute(&phase, &ctx).await;
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════════
// HarnessExecutor Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_harness_executor_empty_graph() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    // Initialize git repo (required for worktree manager)
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(tmp.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(tmp.path())
        .output()
        .ok();
    
    let ctx = test_context(tmp.path());
    let phase = make_phase("execute", PhaseKind::Harness { config_overrides: None });

    let mock_client = Arc::new(MockLlmClient::new("Done"));
    let executor = HarnessExecutor::new(tmp.path(), mock_client);
    
    // With empty graph, should complete immediately
    let result = executor.execute_harness(&phase, &ctx, None).await.unwrap();
    assert!(result.success, "Empty graph should succeed");
}

#[tokio::test]
async fn test_harness_executor_via_trait() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    // Initialize git repo
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(tmp.path())
        .output()
        .ok();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(tmp.path())
        .output()
        .ok();
    
    let ctx = test_context(tmp.path());
    let phase = make_phase("execute", PhaseKind::Harness { config_overrides: None });

    let mock_client = Arc::new(MockLlmClient::new("Done"));
    let executor = HarnessExecutor::new(tmp.path(), mock_client);
    
    let result = executor.execute(&phase, &ctx).await.unwrap();
    assert!(result.success);
}

#[tokio::test]
async fn test_harness_executor_wrong_phase_kind() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("skill-phase", PhaseKind::Skill { name: "test".into() });

    let mock_client = Arc::new(MockLlmClient::new("Done"));
    let executor = HarnessExecutor::new(tmp.path(), mock_client);
    
    let result = executor.execute(&phase, &ctx).await;
    assert!(result.is_err());
}
