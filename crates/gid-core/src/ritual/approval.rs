//! Approval Gate Controller — Manage approval gates between phases.
//!
//! Handles approval logic and generates human-readable approval requests.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

use super::definition::{PhaseDefinition, ApprovalRequirement, RitualConfig, PhaseKind};

/// An approval request for a completed phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// ID of the phase requiring approval.
    pub phase_id: String,
    /// Human-readable phase name/title.
    pub phase_name: String,
    /// Summary of what was accomplished.
    pub summary: String,
    /// Artifacts to review.
    pub artifacts_to_review: Vec<String>,
    /// When the request was created.
    pub requested_at: DateTime<Utc>,
}

/// Approval gate controller.
pub struct ApprovalGate;

impl ApprovalGate {
    /// Check if this phase needs approval based on config and approval mode.
    pub fn needs_approval(
        phase: &PhaseDefinition,
        ritual_config: &RitualConfig,
    ) -> bool {
        match phase.approval {
            ApprovalRequirement::Required => true,
            ApprovalRequirement::Auto => false,
            ApprovalRequirement::Optional => {
                // Use the global default
                ritual_config.default_approval == ApprovalRequirement::Required
            }
        }
    }
    
    /// Generate approval request summary for a completed phase.
    pub fn create_request(
        phase: &PhaseDefinition,
        artifacts: &[PathBuf],
    ) -> ApprovalRequest {
        let summary = Self::generate_summary(phase, artifacts);
        let artifacts_to_review = artifacts.iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        
        ApprovalRequest {
            phase_id: phase.id.clone(),
            phase_name: Self::get_phase_name(phase),
            summary,
            artifacts_to_review,
            requested_at: Utc::now(),
        }
    }
    
    /// Get a human-readable name for the phase.
    fn get_phase_name(phase: &PhaseDefinition) -> String {
        match &phase.kind {
            PhaseKind::Skill { name } => format!("Skill: {}", name),
            PhaseKind::GidCommand { command, args } => {
                if args.is_empty() {
                    format!("GID: {}", command)
                } else {
                    format!("GID: {} {}", command, args.join(" "))
                }
            }
            PhaseKind::Harness { .. } => "Task Harness Execution".to_string(),
            PhaseKind::Shell { command } => {
                let short_cmd = if command.len() > 40 {
                    format!("{}...", &command[..37])
                } else {
                    command.clone()
                };
                format!("Shell: {}", short_cmd)
            }
        }
    }
    
    /// Generate a summary of what the phase accomplished.
    fn generate_summary(phase: &PhaseDefinition, artifacts: &[PathBuf]) -> String {
        let artifact_count = artifacts.len();
        let artifact_word = if artifact_count == 1 { "artifact" } else { "artifacts" };
        
        match &phase.kind {
            PhaseKind::Skill { name } => {
                match name.as_str() {
                    "requirements" | "idea-intake" => {
                        if artifact_count > 0 {
                            let first = artifacts[0].to_string_lossy();
                            format!("Generated requirements. Review `{}`", first)
                        } else {
                            "Generated requirements document.".to_string()
                        }
                    }
                    "design-doc" | "design" => {
                        if artifact_count > 0 {
                            let first = artifacts[0].to_string_lossy();
                            format!("Generated design document. Review `{}`", first)
                        } else {
                            "Generated design document.".to_string()
                        }
                    }
                    _ => {
                        format!(
                            "Skill '{}' completed with {} {}.",
                            name, artifact_count, artifact_word
                        )
                    }
                }
            }
            PhaseKind::GidCommand { command, args } => {
                match command.as_str() {
                    "design" if args.contains(&"--parse".to_string()) => {
                        "Generated task graph from design. Review `.gid/graph.yml`".to_string()
                    }
                    "extract" => {
                        "Extracted code from completed tasks.".to_string()
                    }
                    "advise" => {
                        "Ran graph analysis. Check for issues.".to_string()
                    }
                    "plan" => {
                        "Generated execution plan.".to_string()
                    }
                    _ => {
                        format!("Ran `gid {}`. {} {} produced.", command, artifact_count, artifact_word)
                    }
                }
            }
            PhaseKind::Harness { .. } => {
                "Task harness completed execution. Review changes and run tests.".to_string()
            }
            PhaseKind::Shell { command } => {
                let short_cmd = if command.len() > 30 {
                    format!("{}...", &command[..27])
                } else {
                    command.clone()
                };
                format!("Shell command `{}` completed.", short_cmd)
            }
        }
    }
    
    /// Format approval request for display (CLI, Telegram, etc.).
    pub fn format_request(request: &ApprovalRequest) -> String {
        let mut output = String::new();
        
        output.push_str(&format!("🔔 Approval Required: {}\n", request.phase_name));
        output.push_str(&format!("   Phase: {}\n", request.phase_id));
        output.push_str("\n");
        output.push_str(&format!("   {}\n", request.summary));
        
        if !request.artifacts_to_review.is_empty() {
            output.push_str("\n   Artifacts to review:\n");
            for artifact in &request.artifacts_to_review {
                output.push_str(&format!("   • {}\n", artifact));
            }
        }
        
        output.push_str("\n   Run `gid ritual approve` to continue.\n");
        output.push_str("   Run `gid ritual skip` to skip this phase.\n");
        
        output
    }
    
    /// Generate a short status line for the approval request.
    pub fn format_short(request: &ApprovalRequest) -> String {
        format!(
            "Waiting approval for '{}': {}",
            request.phase_id,
            request.summary
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::definition::*;
    
    fn create_test_config() -> RitualConfig {
        RitualConfig::default()
    }
    
    fn create_phase_with_approval(approval: ApprovalRequirement) -> PhaseDefinition {
        PhaseDefinition {
            id: "test".to_string(),
            kind: PhaseKind::Shell { command: "echo test".to_string() },
            model: None,
            approval,
            skip_if: None,
            timeout_minutes: None,
            input: vec![],
            output: vec![],
            hooks: PhaseHooks::default(),
            on_failure: FailureStrategy::Escalate,
            harness_config: None,
        }
    }
    
    #[test]
    fn test_needs_approval_required() {
        let config = create_test_config();
        let phase = create_phase_with_approval(ApprovalRequirement::Required);
        
        assert!(ApprovalGate::needs_approval(&phase, &config));
    }
    
    #[test]
    fn test_needs_approval_auto() {
        let config = create_test_config();
        let phase = create_phase_with_approval(ApprovalRequirement::Auto);
        
        assert!(!ApprovalGate::needs_approval(&phase, &config));
    }
    
    #[test]
    fn test_needs_approval_optional_with_default_auto() {
        let config = RitualConfig {
            default_approval: ApprovalRequirement::Auto,
            ..Default::default()
        };
        let phase = create_phase_with_approval(ApprovalRequirement::Optional);
        
        assert!(!ApprovalGate::needs_approval(&phase, &config));
    }
    
    #[test]
    fn test_needs_approval_optional_with_default_required() {
        let config = RitualConfig {
            default_approval: ApprovalRequirement::Required,
            ..Default::default()
        };
        let phase = create_phase_with_approval(ApprovalRequirement::Optional);
        
        assert!(ApprovalGate::needs_approval(&phase, &config));
    }
    
    #[test]
    fn test_create_request() {
        let phase = PhaseDefinition {
            id: "requirements".to_string(),
            kind: PhaseKind::Skill { name: "requirements".to_string() },
            model: None,
            approval: ApprovalRequirement::Required,
            skip_if: None,
            timeout_minutes: None,
            input: vec![],
            output: vec![],
            hooks: PhaseHooks::default(),
            on_failure: FailureStrategy::Escalate,
            harness_config: None,
        };
        
        let artifacts = vec![PathBuf::from(".gid/features/auth/requirements.md")];
        let request = ApprovalGate::create_request(&phase, &artifacts);
        
        assert_eq!(request.phase_id, "requirements");
        assert!(request.summary.contains("requirements"));
        assert_eq!(request.artifacts_to_review.len(), 1);
    }
    
    #[test]
    fn test_format_request() {
        let request = ApprovalRequest {
            phase_id: "design".to_string(),
            phase_name: "Skill: design-doc".to_string(),
            summary: "Generated design document. Review `design.md`".to_string(),
            artifacts_to_review: vec!["design.md".to_string()],
            requested_at: Utc::now(),
        };
        
        let formatted = ApprovalGate::format_request(&request);
        
        assert!(formatted.contains("Approval Required"));
        assert!(formatted.contains("design"));
        assert!(formatted.contains("gid ritual approve"));
    }
    
    #[test]
    fn test_get_phase_name_skill() {
        let phase = PhaseDefinition {
            id: "test".to_string(),
            kind: PhaseKind::Skill { name: "requirements".to_string() },
            model: None,
            approval: ApprovalRequirement::Auto,
            skip_if: None,
            timeout_minutes: None,
            input: vec![],
            output: vec![],
            hooks: PhaseHooks::default(),
            on_failure: FailureStrategy::Escalate,
            harness_config: None,
        };
        
        assert_eq!(ApprovalGate::get_phase_name(&phase), "Skill: requirements");
    }
    
    #[test]
    fn test_get_phase_name_gid_command() {
        let phase = PhaseDefinition {
            id: "test".to_string(),
            kind: PhaseKind::GidCommand {
                command: "design".to_string(),
                args: vec!["--parse".to_string()],
            },
            model: None,
            approval: ApprovalRequirement::Auto,
            skip_if: None,
            timeout_minutes: None,
            input: vec![],
            output: vec![],
            hooks: PhaseHooks::default(),
            on_failure: FailureStrategy::Escalate,
            harness_config: None,
        };
        
        assert_eq!(ApprovalGate::get_phase_name(&phase), "GID: design --parse");
    }
}
