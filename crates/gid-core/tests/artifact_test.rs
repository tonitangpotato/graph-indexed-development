//! Integration tests for artifact management.

use gid_core::ritual::artifact::ArtifactManager;
use std::path::PathBuf;

#[test]
fn test_record_and_get_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = ArtifactManager::new(tmp.path());

    let paths = vec![
        tmp.path().join(".gid/features/auth/research.md"),
    ];
    manager.record("research", paths.clone());

    assert!(manager.has_artifacts("research"));
    assert!(!manager.has_artifacts("nonexistent"));

    let stored = manager.get("research").unwrap();
    assert_eq!(stored.len(), 1);
}

#[test]
fn test_clear_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = ArtifactManager::new(tmp.path());

    manager.record("research", vec![PathBuf::from("test.md")]);
    assert!(manager.has_artifacts("research"));

    manager.clear();
    assert!(!manager.has_artifacts("research"));
}

#[test]
fn test_get_all_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut manager = ArtifactManager::new(tmp.path());

    manager.record("phase-a", vec![PathBuf::from("a.md")]);
    manager.record("phase-b", vec![PathBuf::from("b.md"), PathBuf::from("c.md")]);

    let all = manager.get_all();
    assert_eq!(all.len(), 2);
    assert_eq!(all["phase-b"].len(), 2);
}
