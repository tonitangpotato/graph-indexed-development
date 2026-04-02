//! Configuration loader with cascading precedence.
//!
//! Loads harness settings from multiple sources with the following priority:
//! CLI overrides > `.gid/execution.yml` > framework config > built-in defaults.

use std::path::Path;
use std::collections::HashMap;
use anyhow::{Result, Context};
use super::types::{HarnessConfig, ApprovalMode, GuardCheck};

/// Load harness configuration with cascading precedence.
///
/// Priority (highest to lowest):
/// 1. `cli_overrides` — explicit CLI flags
/// 2. `execution_yml` — project-level `.gid/execution.yml`
/// 3. `framework_config` — framework config (e.g., `rustclaw.yaml`)
/// 4. Built-in defaults
///
/// Each layer only overrides fields that are explicitly set. For example,
/// if CLI specifies `max_concurrent=5` but nothing else, all other fields
/// come from execution.yml or defaults.
pub fn load_config(
    cli_overrides: Option<&HarnessConfig>,
    execution_yml: Option<&Path>,
    framework_config: Option<&Path>,
) -> Result<HarnessConfig> {
    // Start with defaults
    let mut config = HarnessConfig::default();

    // Layer 3: framework config (lowest priority override)
    if let Some(path) = framework_config {
        if path.exists() {
            let partial = load_partial_from_yaml(path)
                .with_context(|| format!("Failed to load framework config: {}", path.display()))?;
            merge_partial(&mut config, &partial);
        }
    }

    // Layer 2: .gid/execution.yml
    if let Some(path) = execution_yml {
        if path.exists() {
            let partial = load_partial_from_yaml(path)
                .with_context(|| format!("Failed to load execution.yml: {}", path.display()))?;
            merge_partial(&mut config, &partial);
        }
    }

    // Layer 1: CLI overrides (highest priority)
    if let Some(cli) = cli_overrides {
        merge_config(&mut config, cli);
    }

    Ok(config)
}

/// Partial config parsed from YAML — all fields optional for cascading merge.
#[derive(Debug, Default, serde::Deserialize)]
struct PartialConfig {
    approval_mode: Option<ApprovalMode>,
    max_concurrent: Option<usize>,
    max_retries: Option<u32>,
    max_replans: Option<u32>,
    default_checkpoint: Option<String>,
    model: Option<String>,
    max_iterations: Option<u32>,
    invariant_checks: Option<HashMap<String, GuardCheck>>,
}

/// Load a partial config from a YAML file.
fn load_partial_from_yaml(path: &Path) -> Result<PartialConfig> {
    let content = std::fs::read_to_string(path)?;
    let partial: PartialConfig = serde_yaml::from_str(&content)?;
    Ok(partial)
}

/// Merge a partial config into the target, overriding only set fields.
fn merge_partial(target: &mut HarnessConfig, partial: &PartialConfig) {
    if let Some(ref mode) = partial.approval_mode {
        target.approval_mode = mode.clone();
    }
    if let Some(v) = partial.max_concurrent {
        target.max_concurrent = v;
    }
    if let Some(v) = partial.max_retries {
        target.max_retries = v;
    }
    if let Some(v) = partial.max_replans {
        target.max_replans = v;
    }
    if let Some(ref v) = partial.default_checkpoint {
        target.default_checkpoint = Some(v.clone());
    }
    if let Some(ref v) = partial.model {
        target.model = v.clone();
    }
    if let Some(v) = partial.max_iterations {
        target.max_iterations = v;
    }
    if let Some(ref v) = partial.invariant_checks {
        // Merge invariant checks (override per guard ID)
        for (k, check) in v {
            target.invariant_checks.insert(k.clone(), check.clone());
        }
    }
}

/// Merge a full HarnessConfig (CLI overrides) into the target.
///
/// Since HarnessConfig has no Option fields (all have defaults),
/// we treat non-default values as overrides.
fn merge_config(target: &mut HarnessConfig, overrides: &HarnessConfig) {
    let defaults = HarnessConfig::default();

    // Override only if different from default (i.e., explicitly set)
    if overrides.approval_mode != defaults.approval_mode {
        target.approval_mode = overrides.approval_mode.clone();
    }
    if overrides.max_concurrent != defaults.max_concurrent {
        target.max_concurrent = overrides.max_concurrent;
    }
    if overrides.max_retries != defaults.max_retries {
        target.max_retries = overrides.max_retries;
    }
    if overrides.max_replans != defaults.max_replans {
        target.max_replans = overrides.max_replans;
    }
    if overrides.default_checkpoint.is_some() {
        target.default_checkpoint = overrides.default_checkpoint.clone();
    }
    if overrides.model != defaults.model {
        target.model = overrides.model.clone();
    }
    if overrides.max_iterations != defaults.max_iterations {
        target.max_iterations = overrides.max_iterations;
    }
    if !overrides.invariant_checks.is_empty() {
        for (k, v) in &overrides.invariant_checks {
            target.invariant_checks.insert(k.clone(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_defaults() {
        let config = load_config(None, None, None).unwrap();
        assert_eq!(config.approval_mode, ApprovalMode::Mixed);
        assert_eq!(config.max_concurrent, 3);
        assert_eq!(config.max_retries, 1);
        assert_eq!(config.max_replans, 3);
        assert!(config.default_checkpoint.is_none());
        assert_eq!(config.model, "claude-sonnet-4-5");
        assert_eq!(config.max_iterations, 80);
        assert!(config.invariant_checks.is_empty());
    }

    #[test]
    fn test_execution_yml_overrides() {
        let tmp = TempDir::new().unwrap();
        let yml_path = tmp.path().join("execution.yml");
        fs::write(&yml_path, r#"
approval_mode: auto
max_concurrent: 5
max_retries: 3
model: claude-opus-4-5
default_checkpoint: "cargo check && cargo test"
invariant_checks:
  GUARD-1:
    command: "grep -rn 'unwrap()' src/ | wc -l"
    expect: "0"
"#).unwrap();

        let config = load_config(None, Some(&yml_path), None).unwrap();
        assert_eq!(config.approval_mode, ApprovalMode::Auto);
        assert_eq!(config.max_concurrent, 5);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.model, "claude-opus-4-5");
        assert_eq!(config.default_checkpoint, Some("cargo check && cargo test".to_string()));
        assert!(config.invariant_checks.contains_key("GUARD-1"));
        // Non-overridden fields keep defaults
        assert_eq!(config.max_replans, 3);
    }

    #[test]
    fn test_cli_overrides_execution_yml() {
        let tmp = TempDir::new().unwrap();
        let yml_path = tmp.path().join("execution.yml");
        fs::write(&yml_path, r#"
max_concurrent: 5
model: claude-opus-4-5
"#).unwrap();

        let cli = HarnessConfig {
            max_concurrent: 8,
            ..HarnessConfig::default()
        };

        let config = load_config(Some(&cli), Some(&yml_path), None).unwrap();
        // CLI wins for max_concurrent
        assert_eq!(config.max_concurrent, 8);
        // execution.yml wins for model (CLI has default value)
        assert_eq!(config.model, "claude-opus-4-5");
    }

    #[test]
    fn test_framework_config_lowest_priority() {
        let tmp = TempDir::new().unwrap();

        let fw_path = tmp.path().join("framework.yml");
        fs::write(&fw_path, r#"
max_concurrent: 2
model: claude-haiku
"#).unwrap();

        let yml_path = tmp.path().join("execution.yml");
        fs::write(&yml_path, "max_concurrent: 4\n").unwrap();

        let config = load_config(None, Some(&yml_path), Some(&fw_path)).unwrap();
        // execution.yml beats framework
        assert_eq!(config.max_concurrent, 4);
        // framework provides model (execution.yml didn't set it)
        assert_eq!(config.model, "claude-haiku");
    }

    #[test]
    fn test_full_cascade() {
        let tmp = TempDir::new().unwrap();

        let fw_path = tmp.path().join("framework.yml");
        fs::write(&fw_path, r#"
max_concurrent: 2
max_retries: 5
model: claude-haiku
"#).unwrap();

        let yml_path = tmp.path().join("execution.yml");
        fs::write(&yml_path, r#"
max_concurrent: 4
model: claude-sonnet-4-5
"#).unwrap();

        let cli = HarnessConfig {
            max_concurrent: 10,
            ..HarnessConfig::default()
        };

        let config = load_config(Some(&cli), Some(&yml_path), Some(&fw_path)).unwrap();
        assert_eq!(config.max_concurrent, 10);   // CLI
        assert_eq!(config.model, "claude-sonnet-4-5"); // execution.yml
        assert_eq!(config.max_retries, 5);        // framework
        assert_eq!(config.max_replans, 3);         // default
    }

    #[test]
    fn test_missing_files_graceful() {
        let missing = Path::new("/nonexistent/execution.yml");
        // Missing files are silently skipped
        let config = load_config(None, Some(missing), Some(missing)).unwrap();
        assert_eq!(config.max_concurrent, 3); // defaults
    }

    #[test]
    fn test_invalid_yaml_returns_error() {
        let tmp = TempDir::new().unwrap();
        let yml_path = tmp.path().join("bad.yml");
        fs::write(&yml_path, "{{{{invalid yaml!!!!").unwrap();

        let result = load_config(None, Some(&yml_path), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_invariant_checks_merge() {
        let tmp = TempDir::new().unwrap();

        let fw_path = tmp.path().join("framework.yml");
        fs::write(&fw_path, r#"
invariant_checks:
  GUARD-1:
    command: "echo framework"
    expect: "framework"
  GUARD-2:
    command: "echo guard2"
    expect: "guard2"
"#).unwrap();

        let yml_path = tmp.path().join("execution.yml");
        fs::write(&yml_path, r#"
invariant_checks:
  GUARD-1:
    command: "echo project"
    expect: "project"
"#).unwrap();

        let config = load_config(None, Some(&yml_path), Some(&fw_path)).unwrap();
        // GUARD-1 overridden by execution.yml
        assert_eq!(config.invariant_checks["GUARD-1"].expect, "project");
        // GUARD-2 from framework
        assert_eq!(config.invariant_checks["GUARD-2"].expect, "guard2");
    }

    #[test]
    fn test_approval_modes_parse() {
        let tmp = TempDir::new().unwrap();

        for (mode_str, expected) in [
            ("mixed", ApprovalMode::Mixed),
            ("manual", ApprovalMode::Manual),
            ("auto", ApprovalMode::Auto),
        ] {
            let path = tmp.path().join(format!("{}.yml", mode_str));
            fs::write(&path, format!("approval_mode: {}\n", mode_str)).unwrap();
            let config = load_config(None, Some(&path), None).unwrap();
            assert_eq!(config.approval_mode, expected);
        }
    }
}
