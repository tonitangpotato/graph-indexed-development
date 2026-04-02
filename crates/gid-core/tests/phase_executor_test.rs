//! Integration tests for phase executors.

use gid_core::ritual::executor::{PhaseContext, SkillExecutor, GidCommandExecutor, ShellExecutor};
use gid_core::ritual::definition::{PhaseDefinition, PhaseKind};
use std::collections::HashMap;

fn test_context(tmp: &std::path::Path) -> PhaseContext {
    PhaseContext {
        project_root: tmp.to_path_buf(),
        gid_root: tmp.join(".gid"),
        previous_artifacts: HashMap::new(),
        model: "sonnet".to_string(),
        ritual_name: "test".to_string(),
        phase_index: 0,
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

#[tokio::test]
async fn test_skill_executor_stub() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("research", PhaseKind::Skill { name: "research".into() });

    let executor = SkillExecutor::new(tmp.path());
    let result = executor.execute(&phase, &ctx, "research").await.unwrap();
    assert!(result.success, "Stub executor should return success");
}

#[tokio::test]
async fn test_gid_command_executor_stub() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("gen-graph", PhaseKind::GidCommand { command: "design".into(), args: vec![] });

    let executor = GidCommandExecutor::new();
    let result = executor.execute(&phase, &ctx, "design", &[]).await.unwrap();
    assert!(result.success);
}

#[tokio::test]
async fn test_shell_executor_echo() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("check", PhaseKind::Shell { command: "echo hello".into() });

    let executor = ShellExecutor::new(tmp.path());
    let result = executor.execute(&phase, &ctx, "echo hello").await.unwrap();
    assert!(result.success);
}

#[tokio::test]
async fn test_shell_executor_failing_command() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gid")).unwrap();
    let ctx = test_context(tmp.path());
    let phase = make_phase("fail", PhaseKind::Shell { command: "false".into() });

    let executor = ShellExecutor::new(tmp.path());
    let result = executor.execute(&phase, &ctx, "false").await.unwrap();
    assert!(!result.success, "Failed command should return success=false");
    assert!(result.error.is_some());
}
