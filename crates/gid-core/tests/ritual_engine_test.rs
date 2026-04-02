//! Integration tests for the ritual engine state machine.
//!
//! Tests the full lifecycle: init → run → approve → complete.

use gid_core::ritual::definition::RitualDefinition;
use gid_core::ritual::engine::{RitualEngine, RitualStatus, PhaseStatus};

fn test_definition() -> RitualDefinition {
    let yaml = r#"
name: engine-test
config:
  default_model: sonnet
  approval_mode: mixed
phases:
  - id: research
    kind: skill
    name: research
    approval: required
  - id: execute
    kind: harness
    approval: auto
"#;
    serde_yaml::from_str(yaml).unwrap()
}

#[test]
fn test_engine_init_state() {
    let def = test_definition();
    let tmp = tempfile::tempdir().unwrap();
    let engine = RitualEngine::new(def, tmp.path()).unwrap();

    let state = engine.state();
    assert_eq!(state.ritual_name, "engine-test");
    assert_eq!(state.current_phase, 0);
    assert!(matches!(state.status, RitualStatus::Running));
    assert_eq!(state.phase_states.len(), 2);
    assert!(matches!(state.phase_states[0].status, PhaseStatus::Pending));
}

#[test]
fn test_engine_skip_phase() {
    let def = test_definition();
    let tmp = tempfile::tempdir().unwrap();
    let mut engine = RitualEngine::new(def, tmp.path()).unwrap();

    engine.skip_current().unwrap();
    assert_eq!(engine.state().current_phase, 1);
    assert!(matches!(engine.state().phase_states[0].status, PhaseStatus::Skipped { .. }));
}

#[test]
fn test_engine_cancel() {
    let def = test_definition();
    let tmp = tempfile::tempdir().unwrap();
    let mut engine = RitualEngine::new(def, tmp.path()).unwrap();

    engine.cancel().unwrap();
    assert!(matches!(engine.state().status, RitualStatus::Cancelled));
}

#[test]
fn test_state_persistence() {
    let def = test_definition();
    let tmp = tempfile::tempdir().unwrap();

    // Create and modify engine
    {
        let mut engine = RitualEngine::new(def.clone(), tmp.path()).unwrap();
        engine.skip_current().unwrap();
    }

    // Resume from persisted state
    let engine = RitualEngine::resume(def, tmp.path()).unwrap();
    assert_eq!(engine.state().current_phase, 1);
    assert!(matches!(engine.state().phase_states[0].status, PhaseStatus::Skipped { .. }));
}

#[test]
fn test_json_roundtrip() {
    let json = r#"{
        "ritual_name": "test",
        "started_at": "2026-04-02T21:50:00Z",
        "current_phase": 1,
        "phase_states": [
            {"phase_id": "research", "status": "completed", "started_at": "2026-04-02T21:50:00Z", "completed_at": "2026-04-02T22:00:00Z"},
            {"phase_id": "execute", "status": "running", "started_at": "2026-04-02T22:00:00Z"}
        ],
        "status": {"type": "running"}
    }"#;
    let state: gid_core::ritual::RitualState = serde_json::from_str(json).unwrap();
    assert_eq!(state.current_phase, 1);
    assert!(matches!(state.phase_states[0].status, PhaseStatus::Completed));
    assert!(matches!(state.status, RitualStatus::Running));

    // Roundtrip
    let serialized = serde_json::to_string(&state).unwrap();
    let _: gid_core::ritual::RitualState = serde_json::from_str(&serialized).unwrap();
}
