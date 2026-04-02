//! Integration tests for the ritual template registry.

use gid_core::ritual::template::TemplateRegistry;

#[test]
fn test_builtin_templates_exist() {
    let registry = TemplateRegistry::new();
    let templates = registry.list().unwrap();
    // Builtins should always be available
    assert!(!templates.is_empty(), "Should have builtin templates");
}

#[test]
fn test_load_builtin_template() {
    let registry = TemplateRegistry::new();
    let templates = registry.list().unwrap();

    if let Some(first) = templates.first() {
        let def = registry.load(&first.name).unwrap();
        assert!(!def.phases.is_empty());
    }
}

#[test]
fn test_load_custom_template_from_dir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("custom.yml"), r#"
name: custom-workflow
description: A custom template
config:
  default_model: sonnet
phases:
  - id: do-stuff
    kind: harness
    approval: auto
"#).unwrap();

    let mut registry = TemplateRegistry::new();
    registry.add_path(tmp.path().to_path_buf());

    let templates = registry.list().unwrap();
    let names: Vec<&str> = templates.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"custom"), "Should find custom template: got {:?}", names);
}

#[test]
fn test_load_nonexistent_template() {
    let registry = TemplateRegistry::new();
    let result = registry.load("nonexistent-template-xyz");
    assert!(result.is_err());
}
