//! Ritual Engine — State machine for ritual execution.
//!
//! The engine advances through phases, managing state transitions,
//! checking approval gates, and delegating work to phase executors.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use anyhow::{Context, Result, bail};

use super::definition::{
    RitualDefinition, PhaseDefinition, PhaseKind,
    SkipCondition, FailureStrategy,
};
use super::artifact::ArtifactManager;
use super::approval::ApprovalGate;
use super::executor::{
    PhaseContext, PhaseResult,
    SkillExecutor, GidCommandExecutor, HarnessExecutor, ShellExecutor,
};
use super::llm::LlmClient;
use super::notifier::RitualNotifier;

/// Current state of a ritual execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RitualState {
    /// Name of the ritual being executed.
    pub ritual_name: String,
    /// When the ritual started.
    pub started_at: DateTime<Utc>,
    /// Index of the current phase (0-based).
    pub current_phase: usize,
    /// State of each phase.
    pub phase_states: Vec<PhaseState>,
    /// Overall ritual status.
    pub status: RitualStatus,
}

/// Overall status of the ritual.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RitualStatus {
    /// Ritual is actively running.
    Running,
    /// Waiting for human approval.
    WaitingApproval {
        phase_id: String,
        message: String,
        requested_at: DateTime<Utc>,
    },
    /// Ritual is paused (user requested).
    Paused,
    /// Ritual completed successfully.
    Completed,
    /// Ritual failed.
    Failed {
        phase_id: String,
        error: String,
    },
    /// Ritual was cancelled.
    Cancelled,
}

/// State of a single phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseState {
    /// Phase ID (matches PhaseDefinition.id).
    pub phase_id: String,
    /// Current status of this phase.
    pub status: PhaseStatus,
    /// When execution started (if started).
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    /// When execution completed (if completed).
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// Artifacts produced by this phase.
    #[serde(default)]
    pub artifacts_produced: Vec<String>,
    /// Error message if failed.
    #[serde(default)]
    pub error: Option<String>,
    /// Number of retry attempts used.
    #[serde(default)]
    pub retry_count: u32,
}

/// Status of a single phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    /// Not yet started.
    Pending,
    /// Skipped due to skip_if condition.
    Skipped { reason: String },
    /// Currently executing.
    Running,
    /// Waiting for approval.
    WaitingApproval,
    /// Completed successfully.
    Completed,
    /// Failed.
    Failed,
}

/// The ritual execution engine.
pub struct RitualEngine {
    /// The ritual definition.
    definition: RitualDefinition,
    /// Current execution state.
    state: RitualState,
    /// Project root directory.
    project_root: PathBuf,
    /// GID directory (usually .gid/).
    gid_root: PathBuf,
    /// Artifact manager.
    artifact_manager: ArtifactManager,
    /// LLM client for skill and harness execution.
    /// If None, skill and harness phases will use stub implementations.
    llm_client: Option<Arc<dyn LlmClient>>,
    /// Optional notifier for Telegram notifications.
    notifier: Option<RitualNotifier>,
    /// Whether ritual start notification has been sent (to avoid duplicates on resume).
    start_notified: bool,
}

impl RitualEngine {
    /// Create a new engine, initializing state if not present.
    pub fn new(definition: RitualDefinition, project_root: &Path) -> Result<Self> {
        Self::with_llm_client(definition, project_root, None)
    }

    /// Create a new engine with an LLM client for skill/harness execution.
    pub fn with_llm_client(
        definition: RitualDefinition,
        project_root: &Path,
        llm_client: Option<Arc<dyn LlmClient>>,
    ) -> Result<Self> {
        let gid_root = project_root.join(".gid");
        let state_path = gid_root.join("ritual-state.json");
        
        let (state, resuming) = if state_path.exists() {
            // Resume from existing state
            let content = std::fs::read_to_string(&state_path)
                .context("Failed to read ritual state file")?;
            let state = serde_json::from_str(&content)
                .context("Failed to parse ritual state")?;
            (state, true)
        } else {
            // Initialize new state
            (Self::init_state(&definition), false)
        };
        
        Ok(Self {
            definition,
            state,
            project_root: project_root.to_path_buf(),
            gid_root,
            artifact_manager: ArtifactManager::new(project_root),
            llm_client,
            notifier: None,
            start_notified: resuming, // Don't send start notification on resume
        })
    }
    
    /// Resume from persisted state (crash recovery).
    pub fn resume(definition: RitualDefinition, project_root: &Path) -> Result<Self> {
        Self::resume_with_llm_client(definition, project_root, None)
    }

    /// Resume from persisted state with an LLM client.
    pub fn resume_with_llm_client(
        definition: RitualDefinition,
        project_root: &Path,
        llm_client: Option<Arc<dyn LlmClient>>,
    ) -> Result<Self> {
        let gid_root = project_root.join(".gid");
        let state_path = gid_root.join("ritual-state.json");
        
        if !state_path.exists() {
            bail!("No ritual state found. Use 'gid ritual run' to start a new ritual.");
        }
        
        let content = std::fs::read_to_string(&state_path)
            .context("Failed to read ritual state file")?;
        let state: RitualState = serde_json::from_str(&content)
            .context("Failed to parse ritual state")?;
        
        Ok(Self {
            definition,
            state,
            project_root: project_root.to_path_buf(),
            gid_root,
            artifact_manager: ArtifactManager::new(project_root),
            llm_client,
            notifier: None,
            start_notified: true, // Don't send start notification on resume
        })
    }
    
    /// Set the notifier for Telegram notifications.
    pub fn set_notifier(&mut self, notifier: RitualNotifier) {
        self.notifier = Some(notifier);
    }
    
    /// Initialize state for a new ritual.
    fn init_state(definition: &RitualDefinition) -> RitualState {
        let phase_states = definition.phases.iter().map(|p| PhaseState {
            phase_id: p.id.clone(),
            status: PhaseStatus::Pending,
            started_at: None,
            completed_at: None,
            artifacts_produced: vec![],
            error: None,
            retry_count: 0,
        }).collect();
        
        RitualState {
            ritual_name: definition.name.clone(),
            started_at: Utc::now(),
            current_phase: 0,
            phase_states,
            status: RitualStatus::Running,
        }
    }
    
    /// Run the ritual from current state to completion (or next approval gate).
    pub async fn run(&mut self) -> Result<RitualStatus> {
        // Check if we're resuming from a waiting state
        if let RitualStatus::WaitingApproval { .. } = &self.state.status {
            bail!("Ritual is waiting for approval. Use 'gid ritual approve' to continue.");
        }
        
        if matches!(self.state.status, RitualStatus::Completed | RitualStatus::Cancelled) {
            return Ok(self.state.status.clone());
        }
        
        // Send ritual start notification (only once)
        if !self.start_notified {
            if let Some(ref notifier) = self.notifier {
                let _ = notifier.notify_ritual_start(
                    &self.definition.name,
                    self.definition.phases.len(),
                ).await;
            }
            self.start_notified = true;
        }
        
        self.state.status = RitualStatus::Running;
        self.save_state()?;
        
        while self.state.current_phase < self.definition.phases.len() {
            let phase_idx = self.state.current_phase;
            let phase = &self.definition.phases[phase_idx];
            
            // Check skip condition
            if let Some(reason) = self.check_skip_condition(phase)? {
                self.state.phase_states[phase_idx].status = PhaseStatus::Skipped { reason };
                self.state.current_phase += 1;
                self.save_state()?;
                continue;
            }
            
            // Run pre-hooks
            self.run_hooks(&phase.hooks.pre).await?;
            
            // Execute the phase
            self.state.phase_states[phase_idx].status = PhaseStatus::Running;
            self.state.phase_states[phase_idx].started_at = Some(Utc::now());
            self.save_state()?;
            
            let result = self.execute_phase(phase).await;
            
            match result {
                Ok(phase_result) => {
                    if phase_result.success {
                        // Record artifacts
                        self.state.phase_states[phase_idx].artifacts_produced = phase_result.artifacts.clone();
                        for artifact in &phase_result.artifacts {
                            self.artifact_manager.record(&phase.id, vec![PathBuf::from(artifact)]);
                        }
                        
                        // Run post-hooks
                        self.run_hooks(&phase.hooks.post).await?;
                        
                        // Check if approval needed
                        if ApprovalGate::needs_approval(phase, &self.definition.config) {
                            let request = ApprovalGate::create_request(
                                phase,
                                &phase_result.artifacts.iter().map(PathBuf::from).collect::<Vec<_>>(),
                            );
                            self.state.phase_states[phase_idx].status = PhaseStatus::WaitingApproval;
                            self.state.phase_states[phase_idx].completed_at = Some(Utc::now());
                            self.state.status = RitualStatus::WaitingApproval {
                                phase_id: phase.id.clone(),
                                message: ApprovalGate::format_request(&request),
                                requested_at: Utc::now(),
                            };
                            self.save_state()?;
                            
                            // Notify approval required
                            if let Some(ref notifier) = self.notifier {
                                let _ = notifier.notify_approval_required(
                                    phase,
                                    &phase_result.artifacts,
                                ).await;
                            }
                            
                            return Ok(self.state.status.clone());
                        }
                        
                        // Phase completed successfully
                        self.state.phase_states[phase_idx].status = PhaseStatus::Completed;
                        self.state.phase_states[phase_idx].completed_at = Some(Utc::now());
                        self.state.current_phase += 1;
                        self.save_state()?;
                        
                        // Notify phase completion
                        if let Some(ref notifier) = self.notifier {
                            let _ = notifier.notify_phase_complete(
                                phase,
                                &phase_result,
                                phase_idx,
                                self.definition.phases.len(),
                            ).await;
                        }
                    } else {
                        // Phase failed - extract needed data before mutable borrow
                        let phase_id = phase.id.clone();
                        let on_failure = phase.on_failure.clone();
                        let error_msg = phase_result.error.unwrap_or_else(|| "Unknown error".to_string());
                        self.handle_failure(phase_idx, &phase_id, &on_failure, error_msg).await?;
                        
                        // Check if we should continue or stop
                        if matches!(self.state.status, RitualStatus::Failed { .. }) {
                            return Ok(self.state.status.clone());
                        }
                    }
                }
                Err(e) => {
                    // Extract needed data before mutable borrow
                    let phase_id = phase.id.clone();
                    let on_failure = phase.on_failure.clone();
                    self.handle_failure(phase_idx, &phase_id, &on_failure, e.to_string()).await?;
                    
                    if matches!(self.state.status, RitualStatus::Failed { .. }) {
                        return Ok(self.state.status.clone());
                    }
                }
            }
        }
        
        // All phases completed
        self.state.status = RitualStatus::Completed;
        self.save_state()?;
        
        // Notify ritual completion
        if let Some(ref notifier) = self.notifier {
            let duration_secs = (Utc::now() - self.state.started_at).num_seconds() as u64;
            let _ = notifier.notify_ritual_complete(
                &self.definition.name,
                duration_secs,
            ).await;
        }
        
        Ok(self.state.status.clone())
    }
    
    /// Handle a phase failure according to the failure strategy.
    async fn handle_failure(&mut self, phase_idx: usize, phase_id: &str, on_failure: &FailureStrategy, error: String) -> Result<()> {
        // Get phase for notifications
        let phase = &self.definition.phases[phase_idx];
        
        match on_failure {
            FailureStrategy::Retry { max_attempts } => {
                let retry_count = self.state.phase_states[phase_idx].retry_count;
                if retry_count < *max_attempts {
                    self.state.phase_states[phase_idx].retry_count += 1;
                    self.state.phase_states[phase_idx].status = PhaseStatus::Pending;
                    self.save_state()?;
                    // Will retry on next loop iteration
                } else {
                    // Max retries exceeded, escalate
                    let final_error = format!("Max retries ({}) exceeded: {}", max_attempts, error);
                    self.state.phase_states[phase_idx].status = PhaseStatus::Failed;
                    self.state.phase_states[phase_idx].error = Some(error.clone());
                    self.state.status = RitualStatus::Failed {
                        phase_id: phase_id.to_string(),
                        error: final_error.clone(),
                    };
                    self.save_state()?;
                    
                    // Notify phase and ritual failure
                    if let Some(ref notifier) = self.notifier {
                        let _ = notifier.notify_phase_failed(phase, &error).await;
                        let _ = notifier.notify_ritual_failed(
                            &self.definition.name,
                            phase_id,
                            &final_error,
                        ).await;
                    }
                }
            }
            FailureStrategy::Escalate => {
                self.state.phase_states[phase_idx].status = PhaseStatus::Failed;
                self.state.phase_states[phase_idx].error = Some(error.clone());
                self.state.status = RitualStatus::Failed {
                    phase_id: phase_id.to_string(),
                    error: error.clone(),
                };
                self.save_state()?;
                
                // Notify phase and ritual failure
                if let Some(ref notifier) = self.notifier {
                    let _ = notifier.notify_phase_failed(phase, &error).await;
                    let _ = notifier.notify_ritual_failed(
                        &self.definition.name,
                        phase_id,
                        &error,
                    ).await;
                }
            }
            FailureStrategy::Skip => {
                self.state.phase_states[phase_idx].status = PhaseStatus::Skipped {
                    reason: format!("Failed but skipped: {}", error),
                };
                self.state.current_phase += 1;
                self.save_state()?;
                // No notification for skipped failures — just continue
            }
            FailureStrategy::Abort => {
                let abort_error = format!("Aborted: {}", error);
                self.state.phase_states[phase_idx].status = PhaseStatus::Failed;
                self.state.phase_states[phase_idx].error = Some(error.clone());
                self.state.status = RitualStatus::Failed {
                    phase_id: phase_id.to_string(),
                    error: abort_error.clone(),
                };
                self.save_state()?;
                
                // Notify phase and ritual failure
                if let Some(ref notifier) = self.notifier {
                    let _ = notifier.notify_phase_failed(phase, &error).await;
                    let _ = notifier.notify_ritual_failed(
                        &self.definition.name,
                        phase_id,
                        &abort_error,
                    ).await;
                }
            }
        }
        Ok(())
    }
    
    /// Approve the current pending phase and continue.
    pub async fn approve(&mut self) -> Result<RitualStatus> {
        match &self.state.status {
            RitualStatus::WaitingApproval { phase_id, .. } => {
                // Find the phase index
                let phase_idx = self.definition.phase_index(phase_id)
                    .ok_or_else(|| anyhow::anyhow!("Phase not found: {}", phase_id))?;
                
                // Mark phase as completed and advance
                self.state.phase_states[phase_idx].status = PhaseStatus::Completed;
                self.state.current_phase = phase_idx + 1;
                self.state.status = RitualStatus::Running;
                self.save_state()?;
                
                // Continue running
                self.run().await
            }
            _ => bail!("Ritual is not waiting for approval"),
        }
    }
    
    /// Skip the current pending phase.
    pub fn skip_current(&mut self) -> Result<()> {
        let phase_idx = self.state.current_phase;
        if phase_idx >= self.definition.phases.len() {
            bail!("No current phase to skip");
        }
        
        self.state.phase_states[phase_idx].status = PhaseStatus::Skipped {
            reason: "Manually skipped".to_string(),
        };
        self.state.current_phase += 1;
        
        // If we were waiting for approval, go back to running
        if matches!(self.state.status, RitualStatus::WaitingApproval { .. }) {
            self.state.status = RitualStatus::Running;
        }
        
        self.save_state()
    }
    
    /// Cancel the ritual.
    pub fn cancel(&mut self) -> Result<()> {
        self.state.status = RitualStatus::Cancelled;
        self.save_state()
    }
    
    /// Get current state for display.
    pub fn state(&self) -> &RitualState {
        &self.state
    }
    
    /// Get the ritual definition.
    pub fn definition(&self) -> &RitualDefinition {
        &self.definition
    }
    
    /// Check if a phase should be skipped.
    fn check_skip_condition(&self, phase: &PhaseDefinition) -> Result<Option<String>> {
        let skip_if = match &phase.skip_if {
            Some(cond) => cond,
            None => return Ok(None),
        };
        
        match skip_if {
            SkipCondition::FileExists { file_exists: path } => {
                let full_path = self.project_root.join(path);
                if full_path.exists() {
                    Ok(Some(format!("File exists: {}", path)))
                } else {
                    Ok(None)
                }
            }
            SkipCondition::GlobMatches { glob_matches: pattern } => {
                let full_pattern = self.project_root.join(pattern).to_string_lossy().to_string();
                let matches: Vec<_> = glob::glob(&full_pattern)
                    .map_err(|e| anyhow::anyhow!("Invalid glob pattern: {}", e))?
                    .filter_map(Result::ok)
                    .collect();
                if !matches.is_empty() {
                    Ok(Some(format!("Glob matches {} files: {}", matches.len(), pattern)))
                } else {
                    Ok(None)
                }
            }
            SkipCondition::ArtifactExists { artifact_exists: artifact } => {
                if self.artifact_manager.get_all().iter().any(|(_, paths)| {
                    paths.iter().any(|p| p.to_string_lossy().contains(artifact))
                }) {
                    Ok(Some(format!("Artifact exists: {}", artifact)))
                } else {
                    Ok(None)
                }
            }
            SkipCondition::Always { always: _ } => {
                Ok(Some("Always skip".to_string()))
            }
        }
    }
    
    /// Execute a single phase.
    async fn execute_phase(&self, phase: &PhaseDefinition) -> Result<PhaseResult> {
        let context = self.build_phase_context(phase)?;
        
        match &phase.kind {
            PhaseKind::Skill { name } => {
                if let Some(ref llm_client) = self.llm_client {
                    let executor = SkillExecutor::new(&self.project_root, llm_client.clone());
                    executor.execute_skill(phase, &context, name).await
                } else {
                    // Stub implementation when no LLM client provided
                    tracing::warn!(
                        "No LLM client provided, skill phase '{}' will be stubbed",
                        phase.id
                    );
                    Ok(PhaseResult::success())
                }
            }
            PhaseKind::GidCommand { command, args } => {
                let executor = GidCommandExecutor::new();
                executor.execute_command(phase, &context, command, args).await
            }
            PhaseKind::Harness { config_overrides } => {
                if let Some(ref llm_client) = self.llm_client {
                    let executor = HarnessExecutor::new(&self.project_root, llm_client.clone());
                    // Also check the phase-level harness_config
                    let overrides = config_overrides.as_ref().or(phase.harness_config.as_ref());
                    executor.execute_harness(phase, &context, overrides).await
                } else {
                    // Stub implementation when no LLM client provided
                    tracing::warn!(
                        "No LLM client provided, harness phase '{}' will be stubbed",
                        phase.id
                    );
                    Ok(PhaseResult::success())
                }
            }
            PhaseKind::Shell { command } => {
                let executor = ShellExecutor::new(&self.project_root);
                executor.execute_shell(phase, &context, command).await
            }
        }
    }
    
    /// Build context for phase execution.
    fn build_phase_context(&self, phase: &PhaseDefinition) -> Result<PhaseContext> {
        // Resolve input artifacts
        let mut previous_artifacts = std::collections::HashMap::new();
        for input in &phase.input {
            let resolved = self.artifact_manager.resolve(input)?;
            let key = input.from_phase.clone().unwrap_or_else(|| "external".to_string());
            previous_artifacts.insert(key, resolved);
        }
        
        let model = phase.model.clone()
            .unwrap_or_else(|| self.definition.config.default_model.clone());
        
        Ok(PhaseContext {
            project_root: self.project_root.clone(),
            gid_root: self.gid_root.clone(),
            previous_artifacts,
            model,
            ritual_name: self.definition.name.clone(),
            phase_index: self.state.current_phase,
            task_context: self.definition.task_context.clone(),
        })
    }
    
    /// Run hook commands.
    async fn run_hooks(&self, commands: &[String]) -> Result<()> {
        for cmd in commands {
            let output = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&self.project_root)
                .output()
                .await
                .with_context(|| format!("Failed to run hook: {}", cmd))?;
            
            if !output.status.success() {
                bail!(
                    "Hook failed: {}\nstdout: {}\nstderr: {}",
                    cmd,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        Ok(())
    }
    
    /// Save state to disk.
    fn save_state(&self) -> Result<()> {
        // Ensure .gid directory exists
        std::fs::create_dir_all(&self.gid_root)?;
        
        let state_path = self.gid_root.join("ritual-state.json");
        let content = serde_json::to_string_pretty(&self.state)?;
        std::fs::write(&state_path, content)
            .context("Failed to write ritual state")?;
        Ok(())
    }
    
    /// Delete state file (after completion/cancellation).
    pub fn clear_state(&self) -> Result<()> {
        let state_path = self.gid_root.join("ritual-state.json");
        if state_path.exists() {
            std::fs::remove_file(&state_path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::definition::ApprovalRequirement;
    use tempfile::TempDir;
    
    fn create_test_definition() -> RitualDefinition {
        RitualDefinition {
            name: "test-ritual".to_string(),
            description: None,
            extends: None,
            phases: vec![
                PhaseDefinition {
                    id: "phase1".to_string(),
                    kind: PhaseKind::Shell { command: "echo phase1".to_string() },
                    model: None,
                    approval: ApprovalRequirement::Auto,
                    skip_if: None,
                    timeout_minutes: None,
                    input: vec![],
                    output: vec![],
                    hooks: super::super::definition::PhaseHooks::default(),
                    on_failure: FailureStrategy::Escalate,
                    harness_config: None,
                },
                PhaseDefinition {
                    id: "phase2".to_string(),
                    kind: PhaseKind::Shell { command: "echo phase2".to_string() },
                    model: None,
                    approval: ApprovalRequirement::Auto,
                    skip_if: None,
                    timeout_minutes: None,
                    input: vec![],
                    output: vec![],
                    hooks: super::super::definition::PhaseHooks::default(),
                    on_failure: FailureStrategy::Escalate,
                    harness_config: None,
                },
            ],
            config: super::super::definition::RitualConfig::default(),
            task_context: None,
        }
    }
    
    #[test]
    fn test_init_state() {
        let def = create_test_definition();
        let state = RitualEngine::init_state(&def);
        
        assert_eq!(state.ritual_name, "test-ritual");
        assert_eq!(state.current_phase, 0);
        assert_eq!(state.phase_states.len(), 2);
        assert!(matches!(state.status, RitualStatus::Running));
    }
    
    #[tokio::test]
    async fn test_engine_new() {
        let temp_dir = TempDir::new().unwrap();
        let def = create_test_definition();
        
        let engine = RitualEngine::new(def, temp_dir.path()).unwrap();
        assert_eq!(engine.state.current_phase, 0);
    }
    
    #[tokio::test]
    async fn test_skip_current() {
        let temp_dir = TempDir::new().unwrap();
        let def = create_test_definition();
        
        let mut engine = RitualEngine::new(def, temp_dir.path()).unwrap();
        engine.skip_current().unwrap();
        
        assert_eq!(engine.state.current_phase, 1);
        assert!(matches!(
            engine.state.phase_states[0].status,
            PhaseStatus::Skipped { .. }
        ));
    }
    
    #[tokio::test]
    async fn test_cancel() {
        let temp_dir = TempDir::new().unwrap();
        let def = create_test_definition();
        
        let mut engine = RitualEngine::new(def, temp_dir.path()).unwrap();
        engine.cancel().unwrap();
        
        assert!(matches!(engine.state.status, RitualStatus::Cancelled));
    }

    #[test]
    fn test_ritual_state_json_roundtrip() {
        let json_str = r#"{
            "ritual_name": "test-toolscope",
            "started_at": "2026-04-02T21:50:00Z",
            "current_phase": 0,
            "phase_states": [
                {"phase_id": "research", "status": "running", "started_at": "2026-04-02T21:50:00Z"},
                {"phase_id": "draft-requirements", "status": "pending"},
                {"phase_id": "execute-tasks", "status": "pending"}
            ],
            "status": {"type": "running"}
        }"#;
        let state: RitualState = serde_json::from_str(json_str).expect("Failed to parse RitualState JSON");
        assert!(matches!(state.status, RitualStatus::Running));
        assert_eq!(state.current_phase, 0);
        assert_eq!(state.phase_states.len(), 3);
        assert!(matches!(state.phase_states[0].status, PhaseStatus::Running));
        assert!(matches!(state.phase_states[1].status, PhaseStatus::Pending));
    }
}
