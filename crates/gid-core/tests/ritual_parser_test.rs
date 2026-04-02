//! Integration tests for ritual definition parsing.
//!
//! Tests YAML parsing, validation, and edge cases across the full
//! definition → engine pipeline.

use gid_core::ritual::definition::{RitualDefinition, PhaseKind, ApprovalRequirement};

#[test]
fn test_parse_full_dev_cycle() {
    let yaml = r#"
name: full-dev-cycle
description: Complete development workflow
config:
  default_model: opus
  approval_mode: mixed
phases:
  - id: research
    kind: skill
    name: research
    model: opus
    approval: required
    outputs:
      - ".gid/features/*/research.md"
  - id: draft-requirements
    kind: skill
    name: requirements
    approval: required
  - id: execute-tasks
    kind: harness
    model: opus
    approval: auto
"#;
    let def: RitualDefinition = serde_yaml::from_str(yaml).expect("Failed to parse");
    assert_eq!(def.name, "full-dev-cycle");
    assert_eq!(def.phases.len(), 3);
    assert!(matches!(def.phases[0].kind, PhaseKind::Skill { ref name } if name == "research"));
    assert_eq!(def.phases[0].approval, ApprovalRequirement::Required);
    assert!(matches!(def.phases[2].kind, PhaseKind::Harness { .. }));
}

#[test]
fn test_parse_minimal_ritual() {
    let yaml = r#"
name: quick
config:
  default_model: sonnet
phases:
  - id: do-it
    kind: harness
    approval: auto
"#;
    let def: RitualDefinition = serde_yaml::from_str(yaml).expect("Failed to parse");
    assert_eq!(def.phases.len(), 1);
}

#[test]
fn test_phase_index_lookup() {
    let yaml = r#"
name: test
config:
  default_model: sonnet
phases:
  - id: alpha
    kind: skill
    name: alpha
  - id: beta
    kind: skill
    name: beta
  - id: gamma
    kind: harness
"#;
    let def: RitualDefinition = serde_yaml::from_str(yaml).expect("Failed to parse");
    assert_eq!(def.phase_index("alpha"), Some(0));
    assert_eq!(def.phase_index("beta"), Some(1));
    assert_eq!(def.phase_index("gamma"), Some(2));
    assert_eq!(def.phase_index("nonexistent"), None);
}
