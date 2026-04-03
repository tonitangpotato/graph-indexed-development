//! Phase Executors — Execute individual phases by delegating to backends.
//!
//! Each executor handles a different phase kind: skill, gid command, harness, or shell.
//! All executors implement the `PhaseExecutor` trait for uniform dispatch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tracing::{info, debug, warn};

use super::definition::{PhaseDefinition, PhaseKind, HarnessConfigOverride};
use super::llm::{LlmClient, ToolDefinition};
use super::scope::{ToolScope, BashPolicy, default_scope_for_phase};

/// Result of executing a single phase.
#[derive(Debug, Clone)]
pub struct PhaseResult {
    /// Whether the phase completed successfully.
    pub success: bool,
    /// Artifacts produced by this phase.
    pub artifacts: Vec<String>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Captured stdout/stderr output (for debugging).
    pub output: Option<String>,
    /// Duration in seconds.
    pub duration_secs: u64,
}

impl PhaseResult {
    /// Create a successful result.
    pub fn success() -> Self {
        Self {
            success: true,
            artifacts: vec![],
            error: None,
            output: None,
            duration_secs: 0,
        }
    }

    /// Create a failed result with an error message.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            artifacts: vec![],
            error: Some(error.into()),
            output: None,
            duration_secs: 0,
        }
    }

    /// Set the duration.
    pub fn with_duration(mut self, secs: u64) -> Self {
        self.duration_secs = secs;
        self
    }

    /// Set captured output.
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self
    }

    /// Set the artifacts.
    pub fn with_artifacts(mut self, artifacts: Vec<String>) -> Self {
        self.artifacts = artifacts;
        self
    }
}

/// Context passed to every phase executor.
#[derive(Debug, Clone)]
pub struct PhaseContext {
    /// Project root directory.
    pub project_root: PathBuf,
    /// GID directory (usually .gid/).
    pub gid_root: PathBuf,
    /// Artifacts from previous phases, keyed by phase ID.
    pub previous_artifacts: HashMap<String, Vec<PathBuf>>,
    /// Model to use for this phase.
    pub model: String,
    /// Name of the ritual.
    pub ritual_name: String,
    /// Index of the current phase.
    pub phase_index: usize,
    /// Task context injected by the ritual engine.
    /// Contains the user's task description and any retry/error context.
    /// When present, prepended to the skill prompt so the LLM knows what to do.
    pub task_context: Option<String>,
}

/// Trait for phase execution backends.
#[async_trait]
pub trait PhaseExecutor: Send + Sync {
    /// Execute the phase and return the result.
    async fn execute(
        &self,
        phase: &PhaseDefinition,
        context: &PhaseContext,
    ) -> Result<PhaseResult>;
}

// ═══════════════════════════════════════════════════════════════════════════════
// SkillExecutor — Runs skills via LlmClient
// ═══════════════════════════════════════════════════════════════════════════════

/// Runs a skill by spawning an LLM session with the skill's prompt.
pub struct SkillExecutor {
    project_root: PathBuf,
    llm_client: Arc<dyn LlmClient>,
}

impl SkillExecutor {
    /// Create a new SkillExecutor with an LLM client.
    pub fn new(project_root: &Path, llm_client: Arc<dyn LlmClient>) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            llm_client,
        }
    }

    /// Load the skill prompt from the feature directory or template.
    fn load_skill_prompt(&self, skill_name: &str, context: &PhaseContext) -> Result<String> {
        // Try feature-specific skill first
        let feature_skill = context.gid_root
            .join("features")
            .join(&context.ritual_name)
            .join(format!("{}.md", skill_name));

        if feature_skill.exists() {
            return std::fs::read_to_string(&feature_skill)
                .with_context(|| format!("Failed to read skill prompt: {}", feature_skill.display()));
        }

        // Try template skills
        let template_skill = context.gid_root
            .join("skills")
            .join(format!("{}.md", skill_name));

        if template_skill.exists() {
            return std::fs::read_to_string(&template_skill)
                .with_context(|| format!("Failed to read skill template: {}", template_skill.display()));
        }

        // Fall back to built-in skill prompts, then generic
        match skill_name {
            // "quick-design" is a legacy alias for "draft-design"
            "quick-design" => self.load_skill_prompt("draft-design", context),
            "design-to-graph" => Ok(
                "You are a project graph generator. Read DESIGN.md from the project root.\n\
                 Generate a GID graph in YAML format and write it to .gid/graph.yml.\n\n\
                 The graph has multiple node types:\n\
                 ```yaml\n\
                 nodes:\n\
                   # Component/module nodes (the architecture)\n\
                   - id: mod-dashboard\n\
                     title: \"Dashboard Module\"\n\
                     type: component\n\
                     status: done\n\
                     tags: [module]\n\
                     description: \"HTTP dashboard server\"\n\
                   # File nodes (what gets changed)\n\
                   - id: file-dashboard-rs\n\
                     title: \"src/dashboard.rs\"\n\
                     type: file\n\
                     status: done\n\
                     tags: [source]\n\
                   # Task nodes (what to do)\n\
                   - id: task-add-health-endpoint\n\
                     title: \"Add /api/health endpoint\"\n\
                     type: task\n\
                     status: todo\n\
                     tags: [implementation]\n\
                     description: \"Add health check endpoint returning uptime and stats\"\n\
                 edges:\n\
                   - from: task-add-health-endpoint\n\
                     to: mod-dashboard\n\
                     relation: modifies\n\
                   - from: mod-dashboard\n\
                     to: file-dashboard-rs\n\
                     relation: contains\n\
                   - from: task-a\n\
                     to: task-b\n\
                     relation: depends_on\n\
                 ```\n\n\
                 Node types: component, file, task, feature, layer, doc\n\
                 Edge relations: depends_on, modifies, contains, tests, implements, related_to\n\n\
                 Rules:\n\
                 - Create component nodes for modules being touched\n\
                 - Create file nodes for files being created/modified\n\
                 - Create task nodes for concrete work items (status: todo)\n\
                 - Link tasks to components they modify, components to files they contain\n\
                 - Read existing .gid/graph.yml first — merge new nodes, don't delete existing ones\n\
                 Use the Read tool to read DESIGN.md and existing graph, then Write tool to update .gid/graph.yml.".to_string()
            ),
            "update-design" => Ok(
                "You are a software architect updating an existing design document.\n\
                 Read the current DESIGN.md and the user's new task/feature request.\n\
                 Update DESIGN.md to include the new feature while preserving existing content.\n\
                 Add a new section for the new feature with:\n\
                 - What's being added/changed\n\
                 - Files to modify\n\
                 - Key design decisions\n\
                 Do NOT delete existing design content — append the new feature section.\n\
                 Use the Read tool to read DESIGN.md, then Write tool to update it.".to_string()
            ),
            "update-graph" => Ok(
                "You are updating an existing project graph with new task nodes.\n\
                 Read the current .gid/graph.yml AND DESIGN.md.\n\
                 Add new nodes and edges for the new feature/task described in DESIGN.md.\n\n\
                 CRITICAL RULES:\n\
                 - Read the existing graph FIRST\n\
                 - PRESERVE all existing nodes and edges — do NOT delete or modify them\n\
                 - Only ADD new nodes (task, component, file) and edges for the new work\n\
                 - New task nodes should have status: todo\n\
                 - Link new tasks to existing components they modify\n\n\
                 Node types: component, file, task, feature, layer, doc\n\
                 Edge relations: depends_on, modifies, contains, tests, implements, related_to\n\n\
                 Use Read to load existing graph and DESIGN.md, then Write to update .gid/graph.yml.".to_string()
            ),
            "implement" => Ok(
                "You are a coding agent implementing changes on an existing codebase.\n\
                 Read DESIGN.md and .gid/graph.yml to understand what needs to be done.\n\
                 Look at task nodes with status: todo — these are your work items.\n\n\
                 Workflow:\n\
                 1. Read the relevant existing source files to understand patterns and style\n\
                 2. Implement each task, maintaining consistency with existing code\n\
                 3. All changes happen in this single session — coordinate across files\n\
                 4. After all changes, run the build command to verify compilation\n\n\
                 Rules:\n\
                 - Match existing code style (naming, formatting, patterns)\n\
                 - Use Edit for modifying existing files, Write for new files\n\
                 - Run `cargo check` (Rust), `npm run build` (TS), or equivalent after changes\n\
                 - If build fails, fix the errors before finishing\n\
                 - Keep changes minimal and focused on the task\n\n\
                 Use Read, Write, Edit, and Bash tools.".to_string()
            ),
            "draft-design" => Ok(
                "You are a software architect. Read the user's request and any existing code context.\n\
                 Create a concise DESIGN.md in the project root with:\n\
                 - Problem statement\n\
                 - Proposed solution (files to create/modify)\n\
                 - Key design decisions\n\
                 Keep it brief — this is a quick implementation, not a full RFC.\n\
                 Use the Write tool to create DESIGN.md.".to_string()
            ),
            _ => Ok(format!(
                "You are executing the '{}' skill for ritual '{}'. Complete the task described in the phase definition.",
                skill_name, context.ritual_name
            )),
        }
    }

    /// Build tool definitions based on the scope.
    fn build_tool_definitions(&self, scope: &ToolScope) -> Vec<ToolDefinition> {
        // Base tool definitions
        let all_tools = vec![
            ToolDefinition::new(
                "Read",
                "Read the contents of a file at the specified path.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file to read" }
                    },
                    "required": ["path"]
                }),
            ),
            ToolDefinition::new(
                "Write",
                "Write content to a file. Creates the file if it doesn't exist.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to write to" },
                        "content": { "type": "string", "description": "Content to write" }
                    },
                    "required": ["path", "content"]
                }),
            ),
            ToolDefinition::new(
                "Edit",
                "Make a precise edit to a file by replacing exact text.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file" },
                        "old_text": { "type": "string", "description": "Exact text to find" },
                        "new_text": { "type": "string", "description": "Replacement text" }
                    },
                    "required": ["path", "old_text", "new_text"]
                }),
            ),
            ToolDefinition::new(
                "Bash",
                "Execute a shell command.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to run" }
                    },
                    "required": ["command"]
                }),
            ),
            ToolDefinition::new(
                "WebSearch",
                "Search the web for information.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" }
                    },
                    "required": ["query"]
                }),
            ),
            ToolDefinition::new(
                "WebFetch",
                "Fetch and extract content from a URL.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "URL to fetch" }
                    },
                    "required": ["url"]
                }),
            ),
        ];

        // Filter tools based on scope
        scope.filter_tools(all_tools, |t| &t.name)
    }

    /// Execute a skill phase (legacy method for backward compatibility).
    pub async fn execute_skill(
        &self,
        phase: &PhaseDefinition,
        context: &PhaseContext,
        skill_name: &str,
    ) -> Result<PhaseResult> {
        let start = std::time::Instant::now();

        info!(
            "Executing skill phase '{}' with skill '{}'",
            phase.id, skill_name
        );

        // Load skill prompt and inject task context
        let base_prompt = self.load_skill_prompt(skill_name, context)?;
        let skill_prompt = match &context.task_context {
            Some(ctx) => format!(
                "## USER TASK\n{}\n\n## INSTRUCTIONS\n{}",
                ctx, base_prompt
            ),
            None => base_prompt,
        };

        // Get scope for this phase
        let scope = default_scope_for_phase(&phase.id);

        // Build filtered tool definitions
        let tools = self.build_tool_definitions(&scope);

        debug!(
            "Running skill '{}' with {} tools and model '{}'",
            skill_name, tools.len(), context.model
        );

        // Run the skill via LLM client
        let skill_result = self.llm_client.run_skill(
            &skill_prompt,
            tools,
            &context.model,
            &self.project_root,
        ).await?;

        // Collect output artifacts
        let mut artifacts = Vec::new();
        for output in &phase.output {
            let path = self.project_root.join(&output.path);
            if path.exists() {
                artifacts.push(output.path.clone());
            } else if output.required {
                return Ok(PhaseResult::failure(format!(
                    "Required output artifact not found: {}", output.path
                )).with_duration(start.elapsed().as_secs()));
            }
        }

        // Also include artifacts from skill result
        for artifact in &skill_result.artifacts_created {
            if let Some(s) = artifact.to_str() {
                if !artifacts.contains(&s.to_string()) {
                    artifacts.push(s.to_string());
                }
            }
        }

        Ok(PhaseResult::success()
            .with_artifacts(artifacts)
            .with_duration(start.elapsed().as_secs()))
    }
}

#[async_trait]
impl PhaseExecutor for SkillExecutor {
    async fn execute(
        &self,
        phase: &PhaseDefinition,
        context: &PhaseContext,
    ) -> Result<PhaseResult> {
        match &phase.kind {
            PhaseKind::Skill { name } => {
                self.execute_skill(phase, context, name).await
            }
            _ => bail!("SkillExecutor can only execute Skill phases"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// GidCommandExecutor — Runs gid CLI commands
// ═══════════════════════════════════════════════════════════════════════════════

/// Runs a gid CLI command (design, extract, advise, etc.).
pub struct GidCommandExecutor {
}

impl GidCommandExecutor {
    pub fn new() -> Self {
        Self {}
    }

    /// Execute a gid command in-process using gid-core APIs.
    async fn execute_in_process(
        &self,
        command: &str,
        args: &[String],
        context: &PhaseContext,
    ) -> Result<String> {
        let graph_path = context.gid_root.join("graph.yml");

        match command {
            "advise" => {
                let graph = crate::load_graph(&graph_path)?;
                let results = crate::advise::analyze(&graph);
                let mut output = String::new();
                for advice in &results.items {
                    output.push_str(&format!("[{:?}] {:?}: {}\n", advice.severity, advice.advice_type, advice.message));
                }
                if results.items.is_empty() {
                    output.push_str("No issues found. Graph looks healthy.\n");
                }
                output.push_str(&format!("\nHealth score: {}%\n", results.health_score));
                Ok(output)
            }
            "extract" => {
                let src_dir = if args.is_empty() {
                    context.project_root.join("src")
                } else {
                    context.project_root.join(&args[0])
                };
                let code_graph = crate::code_graph::CodeGraph::extract_from_dir(&src_dir);
                let graph = crate::load_graph(&graph_path).unwrap_or_default();
                let unified = crate::unified::build_unified_graph(&code_graph, &graph);
                let stats = unified.summary();
                crate::save_graph(&unified, &graph_path)?;
                Ok(format!("Extracted code graph: {} total nodes, {} edges", stats.total_nodes, stats.total_edges))
            }
            "plan" => {
                let graph = crate::load_graph(&graph_path)?;
                let plan = crate::harness::create_plan(&graph)?;
                Ok(format!(
                    "Plan: {} tasks, {} layers, ~{} estimated turns\nCritical path: {}",
                    plan.total_tasks,
                    plan.layers.len(),
                    plan.estimated_total_turns,
                    plan.critical_path.join(" → ")
                ))
            }
            "validate" => {
                let graph = crate::load_graph(&graph_path)?;
                let summary = graph.summary();
                let health = graph.health();
                Ok(format!(
                    "Graph: {} nodes, {} edges\nHealth: {:.0}%\nDone: {}/{} tasks",
                    summary.total_nodes, summary.total_edges,
                    health * 100.0, summary.done, summary.total_nodes
                ))
            }
            "design" => {
                // design --parse needs LLM — use Skill phase instead of GidCommand
                Err(anyhow::anyhow!(
                    "gid design requires LLM and should be a Skill phase, not a GidCommand. \
                     Use PhaseKind::Skill {{ name: \"design-to-graph\" }} in your ritual template."
                ))
            }
            other => {
                Err(anyhow::anyhow!(
                    "Unknown gid command '{}'. Supported in-process commands: advise, extract, plan, validate. \
                     If you need LLM, use a Skill phase instead.",
                    other
                ))
            }
        }
    }

    /// Execute a gid command phase.
    pub async fn execute_command(
        &self,
        phase: &PhaseDefinition,
        context: &PhaseContext,
        command: &str,
        args: &[String],
    ) -> Result<PhaseResult> {
        let start = std::time::Instant::now();

        info!(
            "Executing gid command phase '{}': {} {}",
            phase.id, command, args.join(" ")
        );

        // Execute in-process using gid-core APIs (no shell out)
        let result = self.execute_in_process(command, args, context).await;

        let (success, stdout, stderr) = match result {
            Ok(output) => (true, output, String::new()),
            Err(e) => (false, String::new(), e.to_string()),
        };

        if !success {
            return Ok(PhaseResult::failure(format!(
                "gid {} failed:\nstdout: {}\nstderr: {}", command, stdout, stderr
            )).with_duration(start.elapsed().as_secs()));
        }

        debug!("gid {} completed: {}", command, stdout.trim());

        // Collect output artifacts
        let mut artifacts = Vec::new();
        for output_spec in &phase.output {
            let path = context.project_root.join(&output_spec.path);
            if path.exists() {
                artifacts.push(output_spec.path.clone());
            } else if output_spec.required {
                return Ok(PhaseResult::failure(format!(
                    "Required output artifact not found: {}", output_spec.path
                )).with_duration(start.elapsed().as_secs()));
            }
        }

        Ok(PhaseResult::success()
            .with_artifacts(artifacts)
            .with_duration(start.elapsed().as_secs()))
    }
}

impl Default for GidCommandExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PhaseExecutor for GidCommandExecutor {
    async fn execute(
        &self,
        phase: &PhaseDefinition,
        context: &PhaseContext,
    ) -> Result<PhaseResult> {
        match &phase.kind {
            PhaseKind::GidCommand { command, args } => {
                self.execute_command(phase, context, command, args).await
            }
            _ => bail!("GidCommandExecutor can only execute GidCommand phases"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// HarnessExecutor — Runs the task harness
// ═══════════════════════════════════════════════════════════════════════════════

/// Runs the task harness (gid execute).
///
/// The HarnessExecutor bridges ritual phases to the harness scheduler.
/// It wraps an LlmClient to provide TaskExecutor functionality.
pub struct HarnessExecutor {
    #[allow(dead_code)] // Used for future expansion
    project_root: PathBuf,
    llm_client: Arc<dyn LlmClient>,
}

impl HarnessExecutor {
    pub fn new(project_root: &Path, llm_client: Arc<dyn LlmClient>) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            llm_client,
        }
    }

    /// Execute a harness phase (legacy method for backward compatibility).
    pub async fn execute_harness(
        &self,
        phase: &PhaseDefinition,
        context: &PhaseContext,
        config_overrides: Option<&HarnessConfigOverride>,
    ) -> Result<PhaseResult> {
        use crate::harness::{
            create_plan, execute_plan, load_config,
            GitWorktreeManager,
        };
        use crate::graph::Graph;

        let start = std::time::Instant::now();

        info!("Executing harness phase '{}'", phase.id);

        // Load graph
        let graph_path = context.gid_root.join("graph.yml");
        let mut graph = if graph_path.exists() {
            let content = std::fs::read_to_string(&graph_path)
                .with_context(|| format!("Failed to read graph: {}", graph_path.display()))?;
            serde_yaml::from_str(&content)
                .with_context(|| "Failed to parse graph.yml")?
        } else {
            warn!("No graph.yml found, creating empty graph");
            Graph::new()
        };

        // Load base config (no CLI overrides, use execution.yml if exists)
        let execution_yml = context.gid_root.join("execution.yml");
        let mut config = load_config(
            None,
            if execution_yml.exists() { Some(execution_yml.as_path()) } else { None },
            None,
        )?;

        if let Some(overrides) = config_overrides {
            if let Some(max_concurrent) = overrides.max_concurrent {
                config.max_concurrent = max_concurrent;
            }
            if let Some(max_retries) = overrides.max_retries {
                config.max_retries = max_retries;
            }
            if let Some(ref model) = overrides.model {
                config.model = model.clone();
            }
            debug!(
                "Applied harness config overrides: max_concurrent={}, max_retries={}, model={}",
                config.max_concurrent, config.max_retries, config.model
            );
        }

        // Create execution plan
        let plan = create_plan(&graph)?;

        if plan.total_tasks == 0 {
            info!("No tasks to execute in harness phase");
            return Ok(PhaseResult::success()
                .with_duration(start.elapsed().as_secs()));
        }

        // Create executor bridge from LlmClient to TaskExecutor
        let task_executor = LlmTaskExecutor::new(self.llm_client.clone());

        // Create worktree manager
        let worktree_mgr = GitWorktreeManager::new(&context.project_root);

        // Execute the plan
        let result = execute_plan(
            &plan,
            &mut graph,
            &config,
            &task_executor,
            &worktree_mgr,
            &context.gid_root,
        ).await?;

        // Save updated graph
        let yaml = serde_yaml::to_string(&graph)?;
        std::fs::write(&graph_path, yaml)?;

        if result.tasks_failed > 0 {
            return Ok(PhaseResult::failure(format!(
                "Harness completed with {} failed tasks out of {}",
                result.tasks_failed, result.tasks_completed + result.tasks_failed
            )).with_duration(start.elapsed().as_secs()));
        }

        Ok(PhaseResult::success()
            .with_duration(start.elapsed().as_secs()))
    }
}

#[async_trait]
impl PhaseExecutor for HarnessExecutor {
    async fn execute(
        &self,
        phase: &PhaseDefinition,
        context: &PhaseContext,
    ) -> Result<PhaseResult> {
        match &phase.kind {
            PhaseKind::Harness { config_overrides } => {
                // Also check the phase-level harness_config
                let overrides = config_overrides.as_ref().or(phase.harness_config.as_ref());
                self.execute_harness(phase, context, overrides).await
            }
            _ => bail!("HarnessExecutor can only execute Harness phases"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// LlmTaskExecutor — Bridge from LlmClient to harness TaskExecutor
// ═══════════════════════════════════════════════════════════════════════════════

/// Bridge that adapts LlmClient to the harness TaskExecutor trait.
///
/// This allows ritual phases to use LlmClient while the harness scheduler
/// uses TaskExecutor internally.
pub struct LlmTaskExecutor {
    llm_client: Arc<dyn LlmClient>,
}

impl LlmTaskExecutor {
    pub fn new(llm_client: Arc<dyn LlmClient>) -> Self {
        Self { llm_client }
    }

    /// Build a task-specific prompt from TaskContext.
    fn build_task_prompt(context: &crate::harness::TaskContext) -> String {
        let mut prompt = String::new();

        prompt.push_str("You are a focused coding agent executing a single task.\n\n");

        // Task
        prompt.push_str(&format!("## Your Task\n{}\n\n", context.task_info.title));

        // Description
        if !context.task_info.description.is_empty() {
            prompt.push_str(&format!("## Description\n{}\n\n", context.task_info.description));
        }

        // Goals
        if !context.goals_text.is_empty() {
            prompt.push_str("## Goals\n");
            for goal in &context.goals_text {
                prompt.push_str(&format!("- {}\n", goal));
            }
            prompt.push('\n');
        }

        // Design context
        if let Some(ref excerpt) = context.design_excerpt {
            prompt.push_str(&format!("## Design Context\n{}\n\n", excerpt));
        }

        // Dependency interfaces
        if !context.dependency_interfaces.is_empty() {
            prompt.push_str("## Dependency Interfaces\n");
            for iface in &context.dependency_interfaces {
                prompt.push_str(&format!("- {}\n", iface));
            }
            prompt.push('\n');
        }

        // Guards
        if !context.guards.is_empty() {
            prompt.push_str("## Project Guards (must never be violated)\n");
            for guard in &context.guards {
                prompt.push_str(&format!("- {}\n", guard));
            }
            prompt.push('\n');
        }

        // Verify command
        if let Some(ref verify) = context.task_info.verify {
            prompt.push_str(&format!("## Verify Command\n{}\n\n", verify));
        }

        // Rules
        prompt.push_str("## Rules\n");
        prompt.push_str("1. Stay focused — only implement what's described above\n");
        prompt.push_str("2. Be efficient — write code directly, don't read files unless needed\n");
        prompt.push_str("3. Don't modify .gid/ — graph is managed by the harness\n");
        prompt.push_str("4. Self-test — run the verify command yourself before finishing\n");
        prompt.push_str("5. Report blockers — if you can't complete due to missing dependency, say so clearly\n");

        prompt
    }

    /// Build tool definitions for task execution.
    fn build_task_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::new(
                "Read",
                "Read the contents of a file at the specified path.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file" }
                    },
                    "required": ["path"]
                }),
            ),
            ToolDefinition::new(
                "Write",
                "Write content to a file. Creates parent directories as needed.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to write to" },
                        "content": { "type": "string", "description": "Content to write" }
                    },
                    "required": ["path", "content"]
                }),
            ),
            ToolDefinition::new(
                "Edit",
                "Make a precise edit to a file by replacing exact text.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file" },
                        "old_text": { "type": "string", "description": "Exact text to find" },
                        "new_text": { "type": "string", "description": "Replacement text" }
                    },
                    "required": ["path", "old_text", "new_text"]
                }),
            ),
            ToolDefinition::new(
                "Bash",
                "Execute a shell command in the project directory.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command" }
                    },
                    "required": ["command"]
                }),
            ),
        ]
    }
}

#[async_trait]
impl crate::harness::TaskExecutor for LlmTaskExecutor {
    async fn spawn(
        &self,
        context: &crate::harness::TaskContext,
        worktree_path: &Path,
        config: &crate::harness::HarnessConfig,
    ) -> Result<crate::harness::TaskResult> {
        let prompt = Self::build_task_prompt(context);
        let tools = Self::build_task_tools();

        info!(
            task_id = %context.task_info.id,
            worktree = %worktree_path.display(),
            model = %config.model,
            "Spawning task via LlmClient"
        );

        let result = self.llm_client.run_skill(
            &prompt,
            tools,
            &config.model,
            worktree_path,
        ).await;

        match result {
            Ok(skill_result) => {
                // Auto-commit changes in worktree
                let has_changes = tokio::process::Command::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(worktree_path)
                    .output()
                    .await
                    .map(|o| !o.stdout.is_empty())
                    .unwrap_or(false);

                if has_changes {
                    let _ = tokio::process::Command::new("git")
                        .args(["add", "-A"])
                        .current_dir(worktree_path)
                        .output()
                        .await;
                    let _ = tokio::process::Command::new("git")
                        .args(["commit", "-m", &format!("gid: task {} implementation", context.task_info.id)])
                        .current_dir(worktree_path)
                        .output()
                        .await;
                }

                // Check for blockers
                let blocker = detect_blocker(&skill_result.output);

                Ok(crate::harness::TaskResult {
                    success: true,
                    output: skill_result.output,
                    turns_used: skill_result.tool_calls_made as u32,
                    tokens_used: skill_result.tokens_used,
                    blocker,
                })
            }
            Err(e) => {
                warn!(task_id = %context.task_info.id, error = %e, "Task failed");
                Ok(crate::harness::TaskResult {
                    success: false,
                    output: format!("LLM error: {}", e),
                    turns_used: 0,
                    tokens_used: 0,
                    blocker: Some(format!("LLM error: {}", e)),
                })
            }
        }
    }
}

/// Detect blockers in task output.
fn detect_blocker(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    if lower.contains("blocker:") || lower.contains("blocked by") || lower.contains("cannot proceed") {
        for line in output.lines() {
            let ll = line.to_lowercase();
            if ll.contains("blocker:") || ll.contains("blocked by") || ll.contains("cannot proceed") {
                return Some(line.trim().to_string());
            }
        }
        Some("Sub-agent reported a blocker (details in output)".to_string())
    } else {
        None
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// ShellExecutor — Runs shell commands with ToolScope validation
// ═══════════════════════════════════════════════════════════════════════════════

/// Runs an arbitrary shell command with ToolScope validation.
pub struct ShellExecutor {
    working_dir: PathBuf,
}

impl ShellExecutor {
    pub fn new(working_dir: &Path) -> Self {
        Self {
            working_dir: working_dir.to_path_buf(),
        }
    }

    /// Execute a shell command phase (legacy method for backward compatibility).
    pub async fn execute_shell(
        &self,
        phase: &PhaseDefinition,
        _context: &PhaseContext,
        command: &str,
    ) -> Result<PhaseResult> {
        let start = std::time::Instant::now();

        // Get scope for this phase and validate bash policy
        let scope = default_scope_for_phase(&phase.id);

        match &scope.bash_policy {
            BashPolicy::Deny => {
                return Ok(PhaseResult::failure(format!(
                    "Shell commands are not allowed in phase '{}' (bash_policy: Deny)",
                    phase.id
                )).with_duration(start.elapsed().as_secs()));
            }
            BashPolicy::AllowList(allowed) => {
                let trimmed = command.trim();
                let allowed_match = allowed.iter().any(|prefix| trimmed.starts_with(prefix));
                if !allowed_match {
                    return Ok(PhaseResult::failure(format!(
                        "Command '{}' not in allowlist for phase '{}'. Allowed: {:?}",
                        command, phase.id, allowed
                    )).with_duration(start.elapsed().as_secs()));
                }
            }
            BashPolicy::AllowAll => {
                // Proceed
            }
        }

        info!("Executing shell phase '{}': {}", phase.id, command);

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.working_dir)
            .output()
            .await
            .with_context(|| format!("Failed to execute shell command: {}", command))?;

        let success = output.status.success();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !success {
            return Ok(PhaseResult::failure(format!(
                "Shell command failed with exit code {:?}:\nstdout: {}\nstderr: {}",
                output.status.code(), stdout, stderr
            )).with_duration(start.elapsed().as_secs()));
        }

        if !stdout.is_empty() {
            debug!("Shell stdout: {}", stdout.trim());
        }
        if !stderr.is_empty() {
            debug!("Shell stderr: {}", stderr.trim());
        }

        // Collect output artifacts
        let mut artifacts = Vec::new();
        for output_spec in &phase.output {
            let path = self.working_dir.join(&output_spec.path);
            if path.exists() {
                artifacts.push(output_spec.path.clone());
            } else if output_spec.required {
                return Ok(PhaseResult::failure(format!(
                    "Required output artifact not found: {}", output_spec.path
                )).with_duration(start.elapsed().as_secs()));
            }
        }

        // Capture output for debugging
        let combined = format!("{}{}", stdout, stderr);
        let mut result = PhaseResult::success()
            .with_artifacts(artifacts)
            .with_duration(start.elapsed().as_secs());
        if !combined.trim().is_empty() {
            result = result.with_output(combined);
        }
        Ok(result)
    }
}

#[async_trait]
impl PhaseExecutor for ShellExecutor {
    async fn execute(
        &self,
        phase: &PhaseDefinition,
        context: &PhaseContext,
    ) -> Result<PhaseResult> {
        match &phase.kind {
            PhaseKind::Shell { command } => {
                self.execute_shell(phase, context, command).await
            }
            _ => bail!("ShellExecutor can only execute Shell phases"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_phase() -> PhaseDefinition {
        PhaseDefinition {
            id: "test".to_string(),
            kind: PhaseKind::Shell {
                command: "echo test".to_string(),
            },
            model: None,
            approval: super::super::definition::ApprovalRequirement::Auto,
            skip_if: None,
            timeout_minutes: None,
            input: vec![],
            output: vec![],
            hooks: super::super::definition::PhaseHooks::default(),
            on_failure: super::super::definition::FailureStrategy::Escalate,
            harness_config: None,
        }
    }

    fn create_test_context(project_root: &Path) -> PhaseContext {
        PhaseContext {
            project_root: project_root.to_path_buf(),
            gid_root: project_root.join(".gid"),
            previous_artifacts: HashMap::new(),
            model: "sonnet".to_string(),
            ritual_name: "test".to_string(),
            phase_index: 0,
            task_context: None,
        }
    }

    #[tokio::test]
    async fn test_shell_executor_success() {
        let temp_dir = TempDir::new().unwrap();
        let executor = ShellExecutor::new(temp_dir.path());

        // Create a phase with an ID that gets full access
        let mut phase = create_test_phase();
        phase.id = "execute-tasks".to_string(); // This gets full scope

        let context = create_test_context(temp_dir.path());

        let result = executor.execute_shell(&phase, &context, "echo hello").await.unwrap();

        assert!(result.success);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn test_shell_executor_failure() {
        let temp_dir = TempDir::new().unwrap();
        let executor = ShellExecutor::new(temp_dir.path());

        let mut phase = create_test_phase();
        phase.id = "execute-tasks".to_string();

        let context = create_test_context(temp_dir.path());

        let result = executor.execute_shell(&phase, &context, "exit 1").await.unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn test_shell_executor_with_output() {
        let temp_dir = TempDir::new().unwrap();
        let executor = ShellExecutor::new(temp_dir.path());

        let mut phase = create_test_phase();
        phase.id = "execute-tasks".to_string();
        phase.output = vec![
            super::super::definition::ArtifactSpec {
                path: "output.txt".to_string(),
                required: true,
            },
        ];

        let context = create_test_context(temp_dir.path());

        // Create the output file
        std::fs::write(temp_dir.path().join("output.txt"), "test").unwrap();

        let result = executor.execute_shell(&phase, &context, "echo done").await.unwrap();

        assert!(result.success);
        assert_eq!(result.artifacts, vec!["output.txt"]);
    }

    #[tokio::test]
    async fn test_shell_executor_missing_required_output() {
        let temp_dir = TempDir::new().unwrap();
        let executor = ShellExecutor::new(temp_dir.path());

        let mut phase = create_test_phase();
        phase.id = "execute-tasks".to_string();
        phase.output = vec![
            super::super::definition::ArtifactSpec {
                path: "missing.txt".to_string(),
                required: true,
            },
        ];

        let context = create_test_context(temp_dir.path());

        let result = executor.execute_shell(&phase, &context, "echo done").await.unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn test_shell_executor_bash_policy_deny() {
        let temp_dir = TempDir::new().unwrap();
        let executor = ShellExecutor::new(temp_dir.path());

        // Create a phase with an ID that denies bash
        let mut phase = create_test_phase();
        phase.id = "research".to_string(); // Research phase denies bash

        let context = create_test_context(temp_dir.path());

        let result = executor.execute_shell(&phase, &context, "echo hello").await.unwrap();

        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.error.unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn test_shell_executor_bash_policy_allowlist() {
        let temp_dir = TempDir::new().unwrap();
        let executor = ShellExecutor::new(temp_dir.path());

        // Create a phase with verify scope (allows cargo test, etc.)
        let mut phase = create_test_phase();
        phase.id = "verify-quality".to_string();

        let context = create_test_context(temp_dir.path());

        // This should fail - not in allowlist
        let result = executor.execute_shell(&phase, &context, "rm -rf /").await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not in allowlist"));

        // This should succeed - in allowlist (though the command itself may fail)
        let result2 = executor.execute_shell(&phase, &context, "cargo test --help").await.unwrap();
        // The command may fail if cargo isn't installed, but it shouldn't be blocked by policy
        // Actually checking the error shows if it was policy-blocked or execution-failed
        if !result2.success {
            // Should be execution failure, not policy failure
            assert!(!result2.error.as_ref().unwrap().contains("not in allowlist"));
        }
    }

    #[test]
    fn test_phase_result_builder() {
        let result = PhaseResult::success()
            .with_artifacts(vec!["a.txt".to_string()])
            .with_duration(10);

        assert!(result.success);
        assert_eq!(result.artifacts, vec!["a.txt"]);
        assert_eq!(result.duration_secs, 10);
    }

    #[test]
    fn test_phase_result_failure() {
        let result = PhaseResult::failure("Something went wrong");

        assert!(!result.success);
        assert_eq!(result.error, Some("Something went wrong".to_string()));
    }

    #[test]
    fn test_detect_blocker() {
        assert!(detect_blocker("BLOCKER: missing dependency").is_some());
        assert!(detect_blocker("Cannot proceed without config").is_some());
        assert!(detect_blocker("Blocked by missing API key").is_some());
        assert!(detect_blocker("Task completed successfully").is_none());
    }
}
