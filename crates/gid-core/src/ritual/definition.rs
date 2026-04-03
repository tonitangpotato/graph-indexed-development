//! Ritual definition types and YAML parsing.
//!
//! This module defines the structure of a ritual.yml file and provides
//! parsing and validation logic.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use anyhow::{Context, Result, bail};

// HarnessConfig imported for reference, but we use our own override type

/// A ritual definition parsed from ritual.yml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RitualDefinition {
    /// Name of the ritual (e.g., "full-dev-cycle").
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional template to inherit from.
    #[serde(default)]
    pub extends: Option<String>,
    /// Ordered list of phases.
    pub phases: Vec<PhaseDefinition>,
    /// Global configuration.
    #[serde(default)]
    pub config: RitualConfig,
    /// User's task description — injected into skill prompts as context.
    /// Set by the caller (e.g., `/ritual` command or `compose_ritual()`).
    #[serde(default)]
    pub task_context: Option<String>,
}

/// Per-phase configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseDefinition {
    /// Unique identifier for this phase.
    pub id: String,
    /// What this phase does.
    #[serde(flatten)]
    pub kind: PhaseKind,
    /// Model to use for this phase (overrides config.default_model).
    #[serde(default)]
    pub model: Option<String>,
    /// Approval requirement for this phase.
    #[serde(default)]
    pub approval: ApprovalRequirement,
    /// Condition to skip this phase.
    #[serde(default)]
    pub skip_if: Option<SkipCondition>,
    /// Timeout in minutes.
    #[serde(default)]
    pub timeout_minutes: Option<u32>,
    /// Input artifacts from previous phases.
    #[serde(default)]
    pub input: Vec<ArtifactRef>,
    /// Output artifacts this phase produces.
    #[serde(default)]
    pub output: Vec<ArtifactSpec>,
    /// Hooks to run at phase boundaries.
    #[serde(default)]
    pub hooks: PhaseHooks,
    /// What to do on failure.
    #[serde(default)]
    pub on_failure: FailureStrategy,
    /// Harness config overrides (for harness phase only).
    #[serde(default)]
    pub harness_config: Option<HarnessConfigOverride>,
}

impl Default for PhaseDefinition {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: PhaseKind::Shell {
                command: "echo 'no-op'".to_string(),
            },
            model: None,
            approval: ApprovalRequirement::default(),
            skip_if: None,
            timeout_minutes: None,
            input: Vec::new(),
            output: Vec::new(),
            hooks: PhaseHooks::default(),
            on_failure: FailureStrategy::default(),
            harness_config: None,
        }
    }
}

/// What a phase does.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PhaseKind {
    /// Run a skill (LLM session with skill prompt).
    Skill {
        /// Name of the skill to run.
        #[serde(alias = "skill")]
        name: String,
    },
    /// Run a gid command (e.g., design, extract, advise).
    GidCommand {
        /// The gid subcommand to run.
        command: String,
        /// Arguments to pass.
        #[serde(default)]
        args: Vec<String>,
    },
    /// Run the task harness (gid execute).
    Harness {
        /// Optional harness config overrides.
        #[serde(default)]
        config_overrides: Option<HarnessConfigOverride>,
    },
    /// Run an arbitrary shell command.
    Shell {
        /// The shell command to execute.
        command: String,
    },
}

/// Harness configuration overrides for harness phase.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessConfigOverride {
    #[serde(default)]
    pub max_concurrent: Option<usize>,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub model: Option<String>,
}

/// When to require approval.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalRequirement {
    /// Always pause for human approval.
    Required,
    /// Pause only if configured globally.
    Optional,
    /// Never pause (auto-approve).
    #[default]
    Auto,
}

/// When to skip a phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkipCondition {
    /// Skip if file exists.
    FileExists { file_exists: String },
    /// Skip if a glob pattern matches any files.
    GlobMatches { glob_matches: String },
    /// Skip if a previous phase produced a specific artifact.
    ArtifactExists { artifact_exists: String },
    /// Always skip (useful for template overrides).
    Always { always: bool },
}

/// How artifacts flow between phases.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Phase that produced this artifact (if None, from external source).
    #[serde(default)]
    pub from_phase: Option<String>,
    /// Path to the artifact (supports globs and {feature} templates).
    pub path: String,
}

/// Specification for an output artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSpec {
    /// Path where the artifact should be created.
    pub path: String,
    /// Whether this artifact is required for phase success.
    #[serde(default = "default_required")]
    pub required: bool,
}

fn default_required() -> bool {
    true
}

/// Hooks that run at phase boundaries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhaseHooks {
    /// Commands to run before phase execution.
    #[serde(default)]
    pub pre: Vec<String>,
    /// Commands to run after phase execution.
    #[serde(default)]
    pub post: Vec<String>,
}

/// What to do when a phase fails.
/// 
/// Simple strategies (escalate, skip, abort) can be specified as strings.
/// Retry requires nested configuration with max_attempts.
/// 
/// YAML format:
/// - Simple: `on_failure: skip`
/// - Retry: `on_failure: { type: retry, max_attempts: 5 }`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum FailureStrategy {
    /// Re-run the phase up to N times.
    Retry {
        #[serde(default = "default_max_attempts")]
        max_attempts: u32,
    },
    /// Stop and notify human (default).
    #[default]
    Escalate,
    /// Mark as skipped, continue to next phase.
    Skip,
    /// Stop the ritual immediately.
    Abort,
}

fn default_max_attempts() -> u32 {
    3
}

/// Global ritual configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RitualConfig {
    /// Default model for phases that don't specify one.
    #[serde(default = "default_model")]
    pub default_model: String,
    /// Default approval mode.
    #[serde(default)]
    pub default_approval: ApprovalRequirement,
    /// Path to state file.
    #[serde(default = "default_state_file")]
    pub state_file: String,
    /// Path to log file.
    #[serde(default = "default_log_file")]
    pub log_file: String,
    /// Notification configuration.
    #[serde(default)]
    pub notify: Option<super::notifier::RitualNotifyConfig>,
}

impl Default for RitualConfig {
    fn default() -> Self {
        Self {
            default_model: default_model(),
            default_approval: ApprovalRequirement::default(),
            state_file: default_state_file(),
            log_file: default_log_file(),
            notify: None,
        }
    }
}

fn default_model() -> String {
    "sonnet".to_string()
}

fn default_state_file() -> String {
    ".gid/ritual-state.json".to_string()
}

fn default_log_file() -> String {
    ".gid/execution-log.jsonl".to_string()
}

impl RitualDefinition {
    /// Parse from a YAML file, resolving `extends` by loading template.
    pub fn load(path: &Path, template_dirs: &[PathBuf]) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read ritual file: {}", path.display()))?;
        
        let mut definition: RitualDefinition = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse ritual YAML: {}", path.display()))?;
        
        // Resolve template inheritance
        if let Some(ref template_name) = definition.extends {
            let template = Self::load_template(template_name, template_dirs)?;
            definition = Self::merge_with_template(definition, template);
        }
        
        definition.validate()?;
        Ok(definition)
    }
    
    /// Load a template by name from the template directories.
    fn load_template(name: &str, template_dirs: &[PathBuf]) -> Result<RitualDefinition> {
        for dir in template_dirs {
            let path = dir.join(format!("{}.yml", name));
            if path.exists() {
                let content = std::fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read template: {}", path.display()))?;
                let template: RitualDefinition = serde_yaml::from_str(&content)
                    .with_context(|| format!("Failed to parse template: {}", path.display()))?;
                return Ok(template);
            }
            // Also try .yaml extension
            let path = dir.join(format!("{}.yaml", name));
            if path.exists() {
                let content = std::fs::read_to_string(&path)
                    .with_context(|| format!("Failed to read template: {}", path.display()))?;
                let template: RitualDefinition = serde_yaml::from_str(&content)
                    .with_context(|| format!("Failed to parse template: {}", path.display()))?;
                return Ok(template);
            }
        }
        bail!("Template not found: {}", name)
    }
    
    /// Merge definition with template (definition values override template).
    fn merge_with_template(mut definition: RitualDefinition, template: RitualDefinition) -> RitualDefinition {
        // If definition has no phases, use template phases
        if definition.phases.is_empty() {
            definition.phases = template.phases;
        }
        
        // Merge config (definition overrides template)
        if definition.description.is_none() {
            definition.description = template.description;
        }
        
        definition
    }
    
    /// Validate the ritual definition.
    pub fn validate(&self) -> Result<()> {
        // Check all phase IDs are unique
        let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for phase in &self.phases {
            if !seen_ids.insert(&phase.id) {
                bail!("Duplicate phase ID: {}", phase.id);
            }
        }
        
        // Check all artifact references resolve
        for phase in &self.phases {
            for input in &phase.input {
                if let Some(ref from_phase) = input.from_phase {
                    if !seen_ids.contains(from_phase.as_str()) {
                        bail!(
                            "Phase '{}' references unknown phase '{}' in input artifact",
                            phase.id, from_phase
                        );
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// Get a phase by ID.
    pub fn get_phase(&self, id: &str) -> Option<&PhaseDefinition> {
        self.phases.iter().find(|p| p.id == id)
    }
    
    /// Get the index of a phase by ID.
    pub fn phase_index(&self, id: &str) -> Option<usize> {
        self.phases.iter().position(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_minimal_ritual() {
        let yaml = r#"
name: minimal
phases:
  - id: test
    kind: shell
    command: echo hello
"#;
        let def: RitualDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.name, "minimal");
        assert_eq!(def.phases.len(), 1);
        assert_eq!(def.phases[0].id, "test");
    }
    
    #[test]
    fn test_parse_full_ritual() {
        let yaml = r#"
name: full-dev-cycle
description: Complete development cycle
phases:
  - id: requirements
    kind: skill
    name: requirements
    model: sonnet
    approval: required
    output:
      - path: ".gid/features/{feature}/requirements.md"
        required: true

  - id: design
    kind: skill
    name: design-doc
    approval: required
    input:
      - from_phase: requirements
        path: ".gid/features/{feature}/requirements.md"
    output:
      - path: ".gid/features/{feature}/design.md"

  - id: execute
    kind: harness
    approval: auto
    harness_config:
      max_concurrent: 3
    hooks:
      post: ["gid extract"]
    on_failure:
      type: escalate

config:
  default_model: sonnet
  default_approval: optional
"#;
        let def: RitualDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.name, "full-dev-cycle");
        assert_eq!(def.phases.len(), 3);
        
        // Check first phase
        assert_eq!(def.phases[0].id, "requirements");
        assert!(matches!(def.phases[0].kind, PhaseKind::Skill { ref name } if name == "requirements"));
        assert_eq!(def.phases[0].approval, ApprovalRequirement::Required);
        
        // Check input/output
        assert_eq!(def.phases[1].input.len(), 1);
        assert_eq!(def.phases[1].input[0].from_phase, Some("requirements".to_string()));
        
        // Check harness phase
        assert!(matches!(def.phases[2].kind, PhaseKind::Harness { .. }));
        assert_eq!(def.phases[2].hooks.post, vec!["gid extract"]);
    }
    
    #[test]
    fn test_validate_duplicate_ids() {
        let def = RitualDefinition {
            name: "test".to_string(),
            description: None,
            extends: None,
            phases: vec![
                PhaseDefinition {
                    id: "dup".to_string(),
                    kind: PhaseKind::Shell { command: "echo 1".to_string() },
                    model: None,
                    approval: ApprovalRequirement::Auto,
                    skip_if: None,
                    timeout_minutes: None,
                    input: vec![],
                    output: vec![],
                    hooks: PhaseHooks::default(),
                    on_failure: FailureStrategy::Escalate,
                    harness_config: None,
                },
                PhaseDefinition {
                    id: "dup".to_string(),
                    kind: PhaseKind::Shell { command: "echo 2".to_string() },
                    model: None,
                    approval: ApprovalRequirement::Auto,
                    skip_if: None,
                    timeout_minutes: None,
                    input: vec![],
                    output: vec![],
                    hooks: PhaseHooks::default(),
                    on_failure: FailureStrategy::Escalate,
                    harness_config: None,
                },
            ],
            config: RitualConfig::default(),
            task_context: None,
        };
        
        assert!(def.validate().is_err());
    }
    
    #[test]
    fn test_skip_conditions() {
        let yaml = r#"
name: skip-test
phases:
  - id: p1
    kind: shell
    command: echo 1
    skip_if:
      file_exists: ".gid/done"
  - id: p2
    kind: shell
    command: echo 2
    skip_if:
      glob_matches: ".gid/features/*/done"
  - id: p3
    kind: shell
    command: echo 3
    skip_if:
      always: true
"#;
        let def: RitualDefinition = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(def.phases[0].skip_if, Some(SkipCondition::FileExists { .. })));
        assert!(matches!(def.phases[1].skip_if, Some(SkipCondition::GlobMatches { .. })));
        assert!(matches!(def.phases[2].skip_if, Some(SkipCondition::Always { .. })));
    }
    
    #[test]
    fn test_failure_strategies() {
        // Test simple string variants (internally tagged format)
        let yaml = r#"
name: failure-test
phases:
  - id: skip
    kind: shell
    command: echo 2
    on_failure:
      type: skip
  - id: abort
    kind: shell
    command: echo 3
    on_failure:
      type: abort
  - id: escalate
    kind: shell
    command: echo 4
    on_failure:
      type: escalate
"#;
        let def: RitualDefinition = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(def.phases[0].on_failure, FailureStrategy::Skip));
        assert!(matches!(def.phases[1].on_failure, FailureStrategy::Abort));
        assert!(matches!(def.phases[2].on_failure, FailureStrategy::Escalate));
        
        // Test retry with config (internally tagged format)
        let yaml_retry = r#"
name: retry-test
phases:
  - id: retry
    kind: shell
    command: echo 1
    on_failure:
      type: retry
      max_attempts: 5
"#;
        let def2: RitualDefinition = serde_yaml::from_str(yaml_retry).unwrap();
        match &def2.phases[0].on_failure {
            FailureStrategy::Retry { max_attempts } => assert_eq!(*max_attempts, 5),
            other => panic!("Expected Retry, got {:?}", other),
        }
    }
}
