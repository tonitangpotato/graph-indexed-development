//! V2 Ritual Executor — Bridges the pure state machine to real IO.
//!
//! Takes `RitualAction`s produced by `transition()` and executes them,
//! producing `RitualEvent`s that feed back into the state machine.
//!
//! Responsibilities:
//! - DetectProject → filesystem scan → ProjectDetected
//! - RunPlanning → read DESIGN.md + LLM call → PlanDecided
//! - RunSkill → load skill prompt + LLM → SkillCompleted/SkillFailed
//! - RunShell → tokio::process::Command → ShellCompleted/ShellFailed
//! - Notify, SaveState, UpdateGraph, Cleanup → fire-and-forget (no event)

use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use tracing::{info, warn, error};

use super::composer::ProjectState as ComposerProjectState;
use super::llm::{LlmClient, ToolDefinition};
use super::scope::default_scope_for_phase;
use super::state_machine::{
    RitualAction, RitualEvent, RitualState, ImplementStrategy,
    ProjectState as V2ProjectState,
};

/// Callback for sending notifications (fire-and-forget).
pub type NotifyFn = Arc<dyn Fn(String) + Send + Sync>;

/// Build the triage prompt for a given task and project context.
/// Single source of truth — used by both gid-core and external consumers (e.g., RustClaw).
pub fn build_triage_prompt(task: &str, project_ctx: &str) -> String {
    format!(
        r#"You are a triage agent. Assess this development task quickly.

{project_ctx}

Task: "{task}"

Respond with ONLY a JSON object (no markdown, no explanation):
{{
  "clarity": "clear" or "ambiguous",
  "clarify_questions": ["question1", ...] (only if ambiguous, otherwise empty array),
  "size": "small", "medium", or "large",
  "skip_design": true/false,
  "skip_graph": true/false
}}

Guidelines:
- "small": bug fix, add a simple command, change a config value, rename something
- "medium": add a feature that touches 2-3 files, refactor a module
- "large": new subsystem, architectural change, multi-file feature
- skip_design=true if the task is small enough that a DESIGN.md update adds no value
- skip_graph=true ONLY if the task modifies existing code without adding new modules, files, or components
- skip_graph=false if the task adds ANY new files, modules, subsystems, or architectural components — even if a graph already exists, it needs to be UPDATED with new nodes
- "ambiguous" if the task description is vague, could mean multiple things, or lacks critical info
- Short ≠ simple. "fix the bug" is ambiguous. "fix the auth retry loop in llm.rs" is clear and small."#
    )
}

/// V2 executor configuration.
pub struct V2ExecutorConfig {
    /// Project root directory.
    pub project_root: PathBuf,
    /// LLM client for skill execution and planning.
    pub llm_client: Option<Arc<dyn LlmClient>>,
    /// Notification callback (e.g., send Telegram message).
    pub notify: Option<NotifyFn>,
    /// Model to use for skill phases.
    pub skill_model: String,
    /// Model to use for planning (cheaper).
    pub planning_model: String,
}

impl Default for V2ExecutorConfig {
    fn default() -> Self {
        Self {
            project_root: PathBuf::from("."),
            llm_client: None,
            notify: None,
            skill_model: "opus".to_string(),
            planning_model: "sonnet".to_string(),
        }
    }
}

/// The V2 executor — executes actions, returns events.
pub struct V2Executor {
    config: V2ExecutorConfig,
}

impl V2Executor {
    pub fn new(config: V2ExecutorConfig) -> Self {
        Self { config }
    }

    /// Execute an action. Returns Some(event) for event-producing actions, None for fire-and-forget.
    pub async fn execute(&self, action: &RitualAction, state: &RitualState) -> Option<RitualEvent> {
        match action {
            RitualAction::DetectProject => Some(self.detect_project().await),
            RitualAction::RunTriage { task } => Some(self.run_triage(task, state).await),
            RitualAction::RunSkill { name, context } => {
                Some(self.run_skill(name, context).await)
            }
            RitualAction::RunShell { command } => Some(self.run_shell(command).await),
            RitualAction::RunPlanning => Some(self.run_planning(state).await),
            RitualAction::RunHarness { tasks } => Some(self.run_harness(tasks).await),
            RitualAction::Notify { message } => {
                self.notify(message);
                None
            }
            RitualAction::SaveState => {
                self.save_state(state);
                None
            }
            RitualAction::UpdateGraph { description } => {
                self.update_graph(description);
                None
            }
            RitualAction::ApplyReview { approved } => {
                // Fire-and-forget: apply review findings via apply-review skill
                // In gid-core context, this is a no-op (RustClaw executor handles it)
                tracing::info!("ApplyReview (approved: {})", approved);
                None
            }
            RitualAction::Cleanup => {
                self.cleanup();
                None
            }
        }
    }

    /// Execute all actions from a transition, returning the event-producing action's event.
    /// Fire-and-forget actions are executed first, then the event-producing action.
    pub async fn execute_actions(
        &self,
        actions: &[RitualAction],
        state: &RitualState,
    ) -> Option<RitualEvent> {
        let mut event = None;

        for action in actions {
            if action.is_fire_and_forget() {
                // Execute fire-and-forget immediately
                let _ = self.execute(action, state).await;
            } else {
                // Event-producing: execute and capture the event
                event = self.execute(action, state).await;
            }
        }

        event
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Event-producing actions
    // ═══════════════════════════════════════════════════════════════════════

    async fn detect_project(&self) -> RitualEvent {
        info!(project_root = %self.config.project_root.display(), "Detecting project state");

        let cs = ComposerProjectState::detect(&self.config.project_root);

        // Read verify command from .gid/config.yml if it exists
        let verify_command = self.read_verify_command();

        let ps = V2ProjectState {
            has_requirements: cs.has_requirements,
            has_design: cs.has_design,
            has_graph: cs.has_graph,
            has_source: cs.has_source_code,
            has_tests: cs.has_tests,
            language: cs.language.map(|l| format!("{:?}", l)),
            source_file_count: cs.source_file_count,
            verify_command,
        };

        info!(
            has_design = ps.has_design,
            has_graph = ps.has_graph,
            has_source = ps.has_source,
            source_files = ps.source_file_count,
            "Project state detected"
        );

        RitualEvent::ProjectDetected(ps)
    }

    async fn run_triage(&self, task: &str, state: &RitualState) -> RitualEvent {
        info!(task = task, "Running triage (haiku)");

        let llm = match &self.config.llm_client {
            Some(c) => c.clone(),
            None => {
                warn!("No LLM client — defaulting to full flow");
                return RitualEvent::TriageCompleted(super::state_machine::TriageResult {
                    clarity: "clear".into(),
                    clarify_questions: vec![],
                    size: "large".into(),
                    skip_design: false,
                    skip_graph: false,
                });
            }
        };

        // Build project context summary for triage
        let project_ctx = if let Some(ps) = &state.project {
            format!(
                "Project: lang={}, has_design={}, has_graph={}, source_files={}, has_tests={}",
                ps.language.as_deref().unwrap_or("unknown"),
                ps.has_design, ps.has_graph,
                ps.source_file_count, ps.has_tests
            )
        } else {
            "Project: unknown state".into()
        };

        let prompt = build_triage_prompt(task, &project_ctx);

        match llm.chat(&prompt, "haiku").await {
            Ok(response) => {
                // Parse JSON from response
                let json_str = extract_json(&response);
                match serde_json::from_str::<super::state_machine::TriageResult>(json_str) {
                    Ok(result) => {
                        info!(
                            clarity = result.clarity,
                            size = result.size,
                            skip_design = result.skip_design,
                            skip_graph = result.skip_graph,
                            "Triage complete"
                        );
                        RitualEvent::TriageCompleted(result)
                    }
                    Err(e) => {
                        warn!("Failed to parse triage JSON: {}. Defaulting to full flow.", e);
                        RitualEvent::TriageCompleted(super::state_machine::TriageResult {
                            clarity: "clear".into(),
                            clarify_questions: vec![],
                            size: "large".into(),
                            skip_design: false,
                            skip_graph: false,
                        })
                    }
                }
            }
            Err(e) => {
                warn!("Triage LLM call failed: {}. Defaulting to full flow.", e);
                RitualEvent::TriageCompleted(super::state_machine::TriageResult {
                    clarity: "clear".into(),
                    clarify_questions: vec![],
                    size: "large".into(),
                    skip_design: false,
                    skip_graph: false,
                })
            }
        }
    }

    async fn run_skill(&self, name: &str, context: &str) -> RitualEvent {
        info!(skill = name, "Running skill phase");

        let llm = match &self.config.llm_client {
            Some(c) => c.clone(),
            None => {
                error!("No LLM client configured for skill execution");
                return RitualEvent::SkillFailed {
                    phase: name.to_string(),
                    error: "No LLM client configured".to_string(),
                };
            }
        };

        // Load skill prompt
        let base_prompt = match self.load_skill_prompt(name) {
            Ok(p) => p,
            Err(e) => {
                return RitualEvent::SkillFailed {
                    phase: name.to_string(),
                    error: format!("Failed to load skill prompt: {}", e),
                };
            }
        };

        // Compose full prompt with context injection (§4)
        let full_prompt = if context.is_empty() {
            base_prompt
        } else {
            format!("## USER TASK\n{}\n\n## INSTRUCTIONS\n{}", context, base_prompt)
        };

        // Get tool scope for this phase
        let scope = default_scope_for_phase(name);
        let tools = self.scope_to_tool_definitions(&scope);

        match llm
            .run_skill(
                &full_prompt,
                tools,
                &self.config.skill_model,
                &self.config.project_root,
            )
            .await
        {
            Ok(result) => {
                info!(skill = name, "Skill completed successfully");
                RitualEvent::SkillCompleted {
                    phase: name.to_string(),
                    artifacts: result.artifacts_created.iter().map(|p| p.to_string_lossy().to_string()).collect(),
                }
            }
            Err(e) => {
                warn!(skill = name, error = %e, "Skill failed");
                RitualEvent::SkillFailed {
                    phase: name.to_string(),
                    error: e.to_string(),
                }
            }
        }
    }

    async fn run_shell(&self, command: &str) -> RitualEvent {
        info!(command = command, "Running shell command");

        match tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.config.project_root)
            .output()
            .await
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                if output.status.success() {
                    info!(exit_code, "Shell command completed successfully");
                    RitualEvent::ShellCompleted {
                        stdout: format!("{}{}", stdout, stderr),
                        exit_code,
                    }
                } else {
                    warn!(exit_code, "Shell command failed");
                    RitualEvent::ShellFailed {
                        stderr: format!("{}{}", stderr, stdout),
                        exit_code,
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to execute shell command");
                RitualEvent::ShellFailed {
                    stderr: e.to_string(),
                    exit_code: -1,
                }
            }
        }
    }

    async fn run_planning(&self, state: &RitualState) -> RitualEvent {
        info!("Running planning phase");

        let llm = match &self.config.llm_client {
            Some(c) => c.clone(),
            None => {
                warn!("No LLM client for planning, defaulting to SingleLlm");
                return RitualEvent::PlanDecided(ImplementStrategy::SingleLlm);
            }
        };

        // Read DESIGN.md
        let design_path = self.config.project_root.join("DESIGN.md");
        let design_content = match std::fs::read_to_string(&design_path) {
            Ok(c) => c,
            Err(_) => {
                info!("No DESIGN.md found, defaulting to SingleLlm");
                return RitualEvent::PlanDecided(ImplementStrategy::SingleLlm);
            }
        };

        // Truncate if too long (save tokens)
        let design_truncated = if design_content.len() > 15000 {
            format!("{}...\n[TRUNCATED — {} bytes total]", &design_content[..15000], design_content.len())
        } else {
            design_content
        };

        let prompt = format!(
            r#"You are a project planning assistant. Based on the DESIGN.md below and the task description, decide the implementation strategy.

## TASK
{}

## DESIGN.md
{}

## Instructions
Analyze the scope:
1. How many files need to change?
2. Are the changes independent enough for parallel work?
3. Is this a small fix or a large feature?

Output ONLY a JSON object (no markdown, no explanation):
- Small/focused change: {{"strategy": "single_llm"}}
- Large multi-file change with independent parts: {{"strategy": "multi_agent", "tasks": ["task description 1", "task description 2"]}}

Default to "single_llm" unless you're confident the work is large AND parallelizable."#,
            state.task,
            design_truncated
        );

        match llm
            .run_skill(
                &prompt,
                vec![], // No tools needed for planning
                &self.config.planning_model,
                &self.config.project_root,
            )
            .await
        {
            Ok(result) => self.parse_planning_result(&result.output),
            Err(e) => {
                warn!(error = %e, "Planning LLM call failed, defaulting to SingleLlm");
                RitualEvent::PlanDecided(ImplementStrategy::SingleLlm)
            }
        }
    }

    async fn run_harness(&self, tasks: &[String]) -> RitualEvent {
        // Harness execution is complex — for now, treat as a single skill call
        // with all tasks concatenated. Real harness support comes later.
        info!(task_count = tasks.len(), "Running harness (simplified)");

        let context = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| format!("{}. {}", i + 1, t))
            .collect::<Vec<_>>()
            .join("\n");

        self.run_skill("implement", &context).await
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Fire-and-forget actions
    // ═══════════════════════════════════════════════════════════════════════

    fn notify(&self, message: &str) {
        if let Some(ref notify_fn) = self.config.notify {
            notify_fn(message.to_string());
        } else {
            info!(message = message, "Ritual notification (no handler)");
        }
    }

    fn save_state(&self, state: &RitualState) {
        let state_path = self.config.project_root.join(".gid").join("ritual-state.json");

        // Ensure .gid/ exists
        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(state) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&state_path, &json) {
                    warn!(error = %e, "Failed to save ritual state");
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to serialize ritual state");
            }
        }
    }

    fn update_graph(&self, description: &str) {
        use crate::graph::{Graph, NodeStatus};

        let graph_path = self.config.project_root.join(".gid").join("graph.yml");
        if !graph_path.exists() {
            info!("No graph.yml found, skipping graph update");
            return;
        }

        // Load graph
        let content = match std::fs::read_to_string(&graph_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read graph.yml: {}", e);
                return;
            }
        };
        let mut graph: Graph = match serde_yaml::from_str(&content) {
            Ok(g) => g,
            Err(e) => {
                warn!("Failed to parse graph.yml: {}", e);
                return;
            }
        };

        // Find matching node by fuzzy description match
        // Strategy: check if any node's title or description contains the task text (or vice versa)
        let desc_lower = description.to_lowercase();
        let matched_id = graph.nodes.iter()
            .filter(|n| {
                matches!(n.status, NodeStatus::Todo | NodeStatus::InProgress)
            })
            .find(|n| {
                let title_lower = n.title.to_lowercase();
                let node_desc_lower = n.description.as_deref().unwrap_or("").to_lowercase();
                // Match if task description contains node title or vice versa
                desc_lower.contains(&title_lower)
                    || title_lower.contains(&desc_lower)
                    || (!node_desc_lower.is_empty() && (
                        desc_lower.contains(&node_desc_lower)
                        || node_desc_lower.contains(&desc_lower)
                    ))
            })
            .map(|n| n.id.clone());

        if let Some(id) = matched_id {
            if graph.mark_task_done(&id) {
                // Save back
                match serde_yaml::to_string(&graph) {
                    Ok(yaml) => {
                        if let Err(e) = std::fs::write(&graph_path, &yaml) {
                            warn!("Failed to write graph.yml: {}", e);
                        } else {
                            info!(node_id = %id, "Marked graph node as done");
                        }
                    }
                    Err(e) => warn!("Failed to serialize graph: {}", e),
                }
            }
        } else {
            info!(description = description, "No matching graph node found for task");
        }
    }

    fn cleanup(&self) {
        info!("Ritual cleanup");
        // Remove temporary files, ritual-state.json on success, etc.
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Helpers
    // ═══════════════════════════════════════════════════════════════════════

    fn load_skill_prompt(&self, skill_name: &str) -> Result<String> {
        // Priority: .gid/skills/{name}.md → built-in prompts

        let gid_skill = self
            .config
            .project_root
            .join(".gid")
            .join("skills")
            .join(format!("{}.md", skill_name));

        if gid_skill.exists() {
            return Ok(std::fs::read_to_string(&gid_skill)?);
        }

        // Check home-relative skills directories (RustClaw, etc.)
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            let rustclaw_skill = home
                .join("rustclaw")
                .join("skills")
                .join(skill_name)
                .join("SKILL.md");

            if rustclaw_skill.exists() {
                return Ok(std::fs::read_to_string(&rustclaw_skill)?);
            }
        }

        // Built-in fallback prompts
        match skill_name {
            "draft-design" => Ok(include_str!("prompts/draft_design.txt").to_string()),
            "update-design" => Ok(
                "Read the existing DESIGN.md and the user's task. Update the design document \
                 to incorporate the new requirements. Write the updated DESIGN.md."
                    .to_string(),
            ),
            "generate-graph" | "design-to-graph" => Ok(
                "Read DESIGN.md from the project root. Generate a GID graph in YAML format \
                 and write it to .gid/graph.yml.\n\n\
                 The graph has multiple node types:\n\
                 ```yaml\n\
                 nodes:\n\
                   # Feature/module nodes (semantic — the architecture)\n\
                   - id: feat-dashboard\n\
                     title: \"Dashboard Module\"\n\
                     type: feature\n\
                     status: todo\n\
                     tags: [module]\n\
                     description: \"HTTP dashboard server\"\n\
                   # File nodes (what gets changed)\n\
                   - id: file-dashboard-rs\n\
                     title: \"src/dashboard.rs\"\n\
                     type: file\n\
                     status: todo\n\
                   # Task nodes (concrete work items)\n\
                   - id: task-add-health-endpoint\n\
                     title: \"Add health check endpoint\"\n\
                     type: task\n\
                     status: todo\n\
                     tags: [implementation]\n\
                     description: \"Add health check endpoint returning uptime and stats\"\n\
                 edges:\n\
                   - from: task-add-health-endpoint\n\
                     to: feat-dashboard\n\
                     relation: implements\n\
                   - from: feat-dashboard\n\
                     to: file-dashboard-rs\n\
                     relation: contains\n\
                   - from: task-a\n\
                     to: task-b\n\
                     relation: depends_on\n\
                 ```\n\n\
                 Node types: feature, component, file, task, layer, doc\n\
                 Edge relations: depends_on, implements, modifies, contains, tests, related_to\n\n\
                 Rules:\n\
                 - Create feature/component nodes for modules and architectural units\n\
                 - Create file nodes for files being created/modified\n\
                 - Create task nodes for concrete work items (status: todo)\n\
                 - Link tasks to features they implement (relation: implements)\n\
                 - Link features to files they contain (relation: contains)\n\
                 - Link tasks to tasks they depend on (relation: depends_on)\n\
                 - Include metadata (design_ref, goals, file_path) on task nodes\n\
                 Use the Read tool to read DESIGN.md, then Write tool to create .gid/graph.yml."
                    .to_string(),
            ),
            "update-graph" => Ok(
                "Read the existing .gid/graph.yml and DESIGN.md. Update the graph to reflect \
                 any new tasks or changes from the design.\n\n\
                 CRITICAL RULES:\n\
                 - Read the existing graph FIRST\n\
                 - PRESERVE all existing nodes and edges — do NOT delete or modify them\n\
                 - Only ADD new nodes (task, feature, component, file) and edges for the new work\n\
                 - New task nodes should have status: todo\n\
                 - Link new tasks to features they implement (relation: implements)\n\
                 - Link tasks to tasks they depend on (relation: depends_on)\n\n\
                 Node types: feature, component, file, task, layer, doc\n\
                 Edge relations: depends_on, implements, modifies, contains, tests, related_to\n\n\
                 Use Read to load existing graph and DESIGN.md, then Write to update .gid/graph.yml."
                    .to_string(),
            ),
            "implement" => Ok(
                "Implement the described changes following the graph-driven layer approach:\n\n\
                 PROCESS:\n\
                 1. Read .gid/graph.yml to find all task nodes and their layer assignments\n\
                 2. Process layers in order (Layer 0 first, then Layer 1, etc.)\n\
                 3. Within each layer, implement each task node sequentially:\n\
                    a. Read the design doc section relevant to this task\n\
                    b. Read any dependency modules (from prior layers) to understand their public API\n\
                    c. Write the code for this task\n\
                    d. Update the task node's status to 'done' in graph.yml\n\
                 4. After completing ALL tasks in a layer, run the project's build/check command\n\
                    to verify compilation before proceeding to the next layer\n\
                 5. If build fails, fix the issues within the current layer before moving on\n\n\
                 RULES:\n\
                 - Follow existing patterns and conventions in the codebase\n\
                 - Only implement tasks that are status: todo — skip tasks already marked done\n\
                 - Layer order is mandatory: never implement a task before its dependencies are done\n\
                 - Update graph.yml status incrementally, not all at once at the end"
                    .to_string(),
            ),
            _ => anyhow::bail!("Unknown skill: {}", skill_name),
        }
    }

    fn read_verify_command(&self) -> Option<String> {
        let config_path = self.config.project_root.join(".gid").join("config.yml");
        if !config_path.exists() {
            // Default based on project type
            let composer_state = ComposerProjectState::detect(&self.config.project_root);
            return match composer_state.language {
                Some(super::composer::ProjectLanguage::Rust) => {
                    Some("cargo build 2>&1 && cargo test 2>&1".to_string())
                }
                Some(super::composer::ProjectLanguage::TypeScript) => {
                    Some("npm run build 2>&1 && npm test 2>&1".to_string())
                }
                Some(super::composer::ProjectLanguage::Python) => {
                    Some("python -m pytest 2>&1".to_string())
                }
                _ => None,
            };
        }

        // Parse .gid/config.yml for verify_command
        match std::fs::read_to_string(&config_path) {
            Ok(content) => {
                // Simple YAML parsing: look for verify_command: ...
                for line in content.lines() {
                    let trimmed = line.trim();
                    if let Some(cmd) = trimmed.strip_prefix("verify_command:") {
                        let cmd = cmd.trim().trim_matches('"').trim_matches('\'');
                        if !cmd.is_empty() {
                            return Some(cmd.to_string());
                        }
                    }
                }
                None
            }
            Err(_) => None,
        }
    }

    fn parse_planning_result(&self, output: &str) -> RitualEvent {
        // Try to extract JSON from the output
        let json_str = extract_json(output);

        match serde_json::from_str::<serde_json::Value>(json_str) {
            Ok(v) => {
                let strategy = v["strategy"].as_str().unwrap_or("single_llm");
                match strategy {
                    "multi_agent" => {
                        let tasks: Vec<String> = v["tasks"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();

                        if tasks.is_empty() {
                            RitualEvent::PlanDecided(ImplementStrategy::SingleLlm)
                        } else {
                            RitualEvent::PlanDecided(ImplementStrategy::MultiAgent { tasks })
                        }
                    }
                    _ => RitualEvent::PlanDecided(ImplementStrategy::SingleLlm),
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to parse planning JSON, defaulting to SingleLlm");
                RitualEvent::PlanDecided(ImplementStrategy::SingleLlm)
            }
        }
    }

    fn scope_to_tool_definitions(&self, scope: &super::scope::ToolScope) -> Vec<ToolDefinition> {
        scope
            .allowed_tools
            .iter()
            .map(|name| ToolDefinition {
                name: name.clone(),
                description: format!("{} tool", name),
                input_schema: serde_json::json!({"type": "object"}),
            })
            .collect()
    }
}

/// Extract JSON from LLM output (handles markdown code fences).
fn extract_json(output: &str) -> &str {
    // Try to find ```json ... ``` block
    if let Some(start) = output.find("```json") {
        let json_start = start + 7;
        if let Some(end) = output[json_start..].find("```") {
            return output[json_start..json_start + end].trim();
        }
    }
    // Try to find ``` ... ``` block
    if let Some(start) = output.find("```") {
        let json_start = start + 3;
        if let Some(end) = output[json_start..].find("```") {
            return output[json_start..json_start + end].trim();
        }
    }
    // Try to find { ... } directly
    if let Some(start) = output.find('{') {
        if let Some(end) = output.rfind('}') {
            return &output[start..=end];
        }
    }
    output.trim()
}

// ═══════════════════════════════════════════════════════════════════════════════
// V2 Engine Loop — drives the state machine to completion
// ═══════════════════════════════════════════════════════════════════════════════

/// Run the full ritual state machine to completion.
///
/// This is the main entry point: takes a task string, creates the initial state,
/// and runs transition() + executor in a loop until terminal state.
pub async fn run_ritual(
    task: &str,
    executor: &V2Executor,
) -> Result<RitualState> {
    use super::state_machine::transition;

    let mut state = RitualState::new();
    let (new_state, actions) = transition(&state, RitualEvent::Start { task: task.to_string() });
    state = new_state;

    // Execute initial actions
    let mut event = executor.execute_actions(&actions, &state).await;

    let max_iterations = 50; // Safety limit
    let mut iteration = 0;

    while let Some(ev) = event {
        iteration += 1;
        if iteration > max_iterations {
            error!("Ritual exceeded max iterations ({}), escalating", max_iterations);
            let (final_state, final_actions) = transition(
                &state,
                RitualEvent::SkillFailed {
                    phase: "engine".to_string(),
                    error: format!("Max iterations ({}) exceeded", max_iterations),
                },
            );
            state = final_state;
            executor.execute_actions(&final_actions, &state).await;
            break;
        }

        let (new_state, actions) = transition(&state, ev);
        state = new_state;

        if state.phase.is_terminal() || state.phase.is_paused() {
            // Execute remaining fire-and-forget actions (Notify, SaveState)
            executor.execute_actions(&actions, &state).await;
            break;
        }

        event = executor.execute_actions(&actions, &state).await;
    }

    info!(
        phase = ?state.phase,
        iterations = iteration,
        "Ritual completed"
    );

    Ok(state)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_bare() {
        let input = r#"{"strategy": "single_llm"}"#;
        assert_eq!(extract_json(input), r#"{"strategy": "single_llm"}"#);
    }

    #[test]
    fn test_extract_json_fenced() {
        let input = "Here's the plan:\n```json\n{\"strategy\": \"single_llm\"}\n```\n";
        assert_eq!(extract_json(input), r#"{"strategy": "single_llm"}"#);
    }

    #[test]
    fn test_extract_json_code_block() {
        let input = "```\n{\"strategy\": \"multi_agent\", \"tasks\": [\"a\"]}\n```";
        assert_eq!(
            extract_json(input),
            r#"{"strategy": "multi_agent", "tasks": ["a"]}"#
        );
    }

    #[test]
    fn test_extract_json_with_text() {
        let input = "I think single LLM is best.\n{\"strategy\": \"single_llm\"}\nDone.";
        assert_eq!(extract_json(input), r#"{"strategy": "single_llm"}"#);
    }

    #[test]
    fn test_parse_planning_single() {
        let executor = V2Executor::new(V2ExecutorConfig::default());
        let event = executor.parse_planning_result(r#"{"strategy": "single_llm"}"#);
        assert!(matches!(event, RitualEvent::PlanDecided(ImplementStrategy::SingleLlm)));
    }

    #[test]
    fn test_parse_planning_multi() {
        let executor = V2Executor::new(V2ExecutorConfig::default());
        let event = executor.parse_planning_result(
            r#"{"strategy": "multi_agent", "tasks": ["impl auth", "impl dashboard"]}"#,
        );
        match event {
            RitualEvent::PlanDecided(ImplementStrategy::MultiAgent { tasks }) => {
                assert_eq!(tasks.len(), 2);
                assert_eq!(tasks[0], "impl auth");
            }
            _ => panic!("Expected MultiAgent"),
        }
    }

    #[test]
    fn test_parse_planning_invalid_json() {
        let executor = V2Executor::new(V2ExecutorConfig::default());
        let event = executor.parse_planning_result("this is not json at all");
        assert!(matches!(event, RitualEvent::PlanDecided(ImplementStrategy::SingleLlm)));
    }

    #[test]
    fn test_parse_planning_multi_empty_tasks() {
        let executor = V2Executor::new(V2ExecutorConfig::default());
        let event = executor.parse_planning_result(r#"{"strategy": "multi_agent", "tasks": []}"#);
        // Empty tasks should fall back to SingleLlm
        assert!(matches!(event, RitualEvent::PlanDecided(ImplementStrategy::SingleLlm)));
    }
}
