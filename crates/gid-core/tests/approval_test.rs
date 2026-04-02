//! Integration tests for the approval gate system.

use gid_core::ritual::approval::ApprovalGate;
use gid_core::ritual::definition::{RitualDefinition, PhaseKind};
use std::path::PathBuf;

fn make_ritual(approval_mode: &str, phase_approval: &str) -> RitualDefinition {
    serde_yaml::from_str(&format!(r#"
name: test
config:
  default_model: sonnet
  approval_mode: {approval_mode}
phases:
  - id: test-phase
    kind: skill
    name: test
    approval: {phase_approval}
"#)).unwrap()
}

#[test]
fn test_auto_approval_not_needed() {
    let def = make_ritual("auto", "auto");
    let phase = &def.phases[0];
    assert!(!ApprovalGate::needs_approval(phase, &def.config));
}

#[test]
fn test_required_approval_needed() {
    let def = make_ritual("auto", "required");
    let phase = &def.phases[0];
    assert!(ApprovalGate::needs_approval(phase, &def.config));
}

#[test]
fn test_create_approval_request() {
    let def = make_ritual("mixed", "required");
    let phase = &def.phases[0];
    let artifacts = vec![PathBuf::from("docs/research.md")];
    let request = ApprovalGate::create_request(phase, &artifacts);
    assert_eq!(request.phase_id, "test-phase");
}

#[test]
fn test_format_approval_request() {
    let def = make_ritual("mixed", "required");
    let phase = &def.phases[0];
    let artifacts = vec![PathBuf::from("docs/design.md")];
    let request = ApprovalGate::create_request(phase, &artifacts);

    let formatted = ApprovalGate::format_request(&request);
    assert!(!formatted.is_empty());

    let short = ApprovalGate::format_short(&request);
    assert!(!short.is_empty());
}
