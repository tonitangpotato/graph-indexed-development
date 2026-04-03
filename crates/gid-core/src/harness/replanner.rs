//! Adaptive re-planner — analyze failures and decide recovery actions.
//!
//! The re-planner inspects task failures and chooses between:
//! - **Retry**: simple/transient failures (timeout, flaky test)
//! - **AddTasks**: structural issues (missing dependency, wrong interface)
//! - **Escalate**: unresolvable problems (notify human)
//!
//! The main agent (LLM) makes the actual decision; this module provides
//! the analysis framework and enforces limits.
//!
//! ## LLM-Powered Analysis
//!
//! When an auth pool is available, the replanner can use Claude to analyze
//! failures and make more nuanced decisions:
//!
//! ```ignore
//! let action = replanner.analyze_failure_with_llm(
//!     &task,
//!     "compilation error: missing module",
//!     "graph has 5 tasks, 3 completed",
//!     &auth_pool_path,
//! ).await?;
//! ```

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{info, warn, debug};

use super::types::{TaskInfo, TaskResult, ReplanDecision, NewTask};

/// Adaptive re-planner for handling task failures.
///
/// Tracks re-plan attempts and enforces the maximum limit.
/// When the limit is exceeded, all failures escalate to human intervention.
pub struct Replanner {
    /// Maximum number of re-plans allowed before escalation.
    pub max_replans: u32,
    /// Current re-plan count.
    pub replan_count: u32,
}

impl Replanner {
    /// Create a new replanner with the given max re-plan limit.
    pub fn new(max_replans: u32) -> Self {
        Self {
            max_replans,
            replan_count: 0,
        }
    }

    /// Analyze a task failure and decide the recovery action.
    ///
    /// Heuristic-based decision:
    /// - Empty output or timeout → Retry (transient)
    /// - Blocker reported → Escalate (needs human/LLM intervention)
    /// - Re-plan limit exceeded → Escalate
    /// - Other failures → Retry (first attempt), Escalate (subsequent)
    ///
    /// In a full implementation, the main agent LLM would analyze the
    /// failure context and potentially return `AddTasks` with new graph nodes.
    pub fn analyze_failure(
        &mut self,
        task: &TaskInfo,
        result: &TaskResult,
        retry_count: u32,
        max_retries: u32,
    ) -> ReplanDecision {
        info!(
            task_id = %task.id,
            retry_count,
            replan_count = self.replan_count,
            "Analyzing task failure"
        );

        // Check if re-plan limit is exhausted
        if self.replan_count >= self.max_replans {
            warn!(
                task_id = %task.id,
                max_replans = self.max_replans,
                "Re-plan limit exceeded, escalating"
            );
            return ReplanDecision::Escalate(format!(
                "Re-plan limit ({}) exceeded for task '{}'. Manual intervention required.",
                self.max_replans, task.id
            ));
        }

        // If sub-agent reported a blocker, escalate (don't count against replan budget)
        if let Some(ref blocker) = result.blocker {
            warn!(task_id = %task.id, blocker = %blocker, "Task has blocker, escalating");
            return ReplanDecision::Escalate(format!(
                "Task '{}' blocked: {}",
                task.id, blocker
            ));
        }

        // Empty output → likely transient (timeout, crash), retry if possible
        if result.output.trim().is_empty() && retry_count < max_retries {
            info!(task_id = %task.id, "Empty output, retrying");
            return ReplanDecision::Retry;
        }

        // Has retries left → retry
        if retry_count < max_retries {
            info!(task_id = %task.id, "Retrying (attempt {}/{})", retry_count + 1, max_retries);
            return ReplanDecision::Retry;
        }

        // Out of retries → escalate
        self.replan_count += 1;
        warn!(
            task_id = %task.id,
            "All retries exhausted, escalating"
        );
        ReplanDecision::Escalate(format!(
            "Task '{}' failed after {} retries. Output: {}",
            task.id,
            max_retries,
            truncate(&result.output, 500)
        ))
    }

    /// Reset the replan counter (e.g., after successful recovery).
    pub fn reset_count(&mut self) {
        self.replan_count = 0;
    }

    /// Check if the replan limit has been reached.
    pub fn limit_reached(&self) -> bool {
        self.replan_count >= self.max_replans
    }

    /// Analyze failure using LLM for more nuanced decisions.
    ///
    /// This method uses Claude (via agentctl-auth) to analyze the failure context
    /// and decide the best recovery action. It's more sophisticated than the
    /// heuristic-based `analyze_failure()` method.
    ///
    /// # Arguments
    /// - `task`: The failed task information
    /// - `error`: The error output from the task
    /// - `graph_context`: Summary of current graph state (completed tasks, dependencies)
    /// - `pool_path`: Path to the agentctl auth.toml file
    ///
    /// # Returns
    /// A `ReplanDecision` based on LLM analysis. Falls back to heuristics on LLM failure.
    pub async fn analyze_failure_with_llm(
        &mut self,
        task: &TaskInfo,
        error: &str,
        graph_context: &str,
        pool_path: &Path,
    ) -> Result<ReplanAction> {
        // Check replan limit first
        if self.replan_count >= self.max_replans {
            warn!(
                task_id = %task.id,
                max_replans = self.max_replans,
                "Re-plan limit exceeded, escalating"
            );
            return Ok(ReplanAction {
                action: ActionType::Escalate,
                reason: format!(
                    "Re-plan limit ({}) exceeded for task '{}'. Manual intervention required.",
                    self.max_replans, task.id
                ),
                new_tasks: vec![],
            });
        }

        info!(
            task_id = %task.id,
            replan_count = self.replan_count,
            "Analyzing task failure with LLM"
        );

        // Load auth pool and build Claude client
        let pool = match agentctl_auth::AuthPool::load(pool_path) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to load auth pool, falling back to heuristics");
                return Ok(self.fallback_analysis(task, error));
            }
        };

        let client = match agentctl_auth::claude::Client::builder()
            .pool(&pool)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "Failed to build Claude client, falling back to heuristics");
                return Ok(self.fallback_analysis(task, error));
            }
        };

        // Build the prompt
        let prompt = build_analysis_prompt(task, error, graph_context);
        debug!(prompt_len = prompt.len(), "Built analysis prompt");

        // Send to Claude with timeout
        let messages = vec![agentctl_auth::claude::Message::user(&prompt)];
        let response = tokio::time::timeout(
            Duration::from_secs(30),
            client.message_with_system(
                "claude-sonnet-4-20250514",
                &messages,
                4096,
                Some(ANALYSIS_SYSTEM_PROMPT),
            ),
        )
        .await;

        let response = match response {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                warn!(error = %e, "LLM call failed, falling back to heuristics");
                return Ok(self.fallback_analysis(task, error));
            }
            Err(_) => {
                warn!("LLM call timed out (30s), falling back to heuristics");
                return Ok(self.fallback_analysis(task, error));
            }
        };

        // Extract text from response
        let response_text: String = response
            .content
            .iter()
            .filter_map(|block| block.as_text())
            .collect::<Vec<_>>()
            .join("");

        debug!(response_len = response_text.len(), "Got LLM response");

        // Parse the JSON response
        match parse_llm_response(&response_text) {
            Ok(action) => {
                info!(
                    task_id = %task.id,
                    action = ?action.action,
                    reason = %action.reason,
                    new_tasks = action.new_tasks.len(),
                    "LLM analysis complete"
                );
                
                // Increment replan count for non-retry actions
                if !matches!(action.action, ActionType::Retry) {
                    self.replan_count += 1;
                }
                
                Ok(action)
            }
            Err(e) => {
                warn!(error = %e, "Failed to parse LLM response, falling back to heuristics");
                Ok(self.fallback_analysis(task, error))
            }
        }
    }

    /// Fallback to heuristic-based analysis when LLM is unavailable.
    fn fallback_analysis(&mut self, task: &TaskInfo, error: &str) -> ReplanAction {
        // Simple heuristics based on error patterns
        let error_lower = error.to_lowercase();
        
        if error.trim().is_empty() 
            || error_lower.contains("timeout")
            || error_lower.contains("connection reset")
            || error_lower.contains("network")
        {
            ReplanAction {
                action: ActionType::Retry,
                reason: "Transient error detected (empty output, timeout, or network issue)".to_string(),
                new_tasks: vec![],
            }
        } else if error_lower.contains("missing") && error_lower.contains("module")
            || error_lower.contains("import error")
            || error_lower.contains("cannot find")
        {
            // Could add tasks but we don't have enough context without LLM
            self.replan_count += 1;
            ReplanAction {
                action: ActionType::Escalate,
                reason: format!("Missing dependency detected in task '{}'. Manual intervention required.", task.id),
                new_tasks: vec![],
            }
        } else {
            self.replan_count += 1;
            ReplanAction {
                action: ActionType::Escalate,
                reason: format!("Task '{}' failed with unrecoverable error. Output: {}", task.id, truncate(error, 200)),
                new_tasks: vec![],
            }
        }
    }
}

/// Truncate a string to max_len characters, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// LLM Analysis Types and Helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// System prompt for the LLM failure analyzer.
const ANALYSIS_SYSTEM_PROMPT: &str = r#"You are a task failure analyzer for an automated coding system.
Your job is to analyze why a coding task failed and recommend the best recovery action.

You must respond with ONLY valid JSON (no markdown, no explanation outside the JSON).
The JSON must match this schema:
{
  "action": "retry" | "add_tasks" | "escalate",
  "reason": "brief explanation of your decision",
  "new_tasks": []  // only populated if action is "add_tasks"
}

For "add_tasks", new_tasks should be an array of:
{
  "id": "task-id",
  "title": "Task title",
  "description": "What needs to be done",
  "depends_on": ["other-task-id"]
}

Guidelines:
- RETRY: transient errors (network, timeout, flaky tests, rate limits)
- ADD_TASKS: missing prerequisites (module not created, config not set up, dependency not implemented)
- ESCALATE: fundamental issues (wrong architecture, unclear requirements, security concerns)"#;

/// The action type from LLM analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Retry,
    AddTasks,
    Escalate,
}

/// Structured response from LLM failure analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplanAction {
    pub action: ActionType,
    pub reason: String,
    #[serde(default)]
    pub new_tasks: Vec<LlmNewTask>,
}

/// New task definition from LLM (converted to NewTask for graph insertion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmNewTask {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl From<LlmNewTask> for NewTask {
    fn from(llm_task: LlmNewTask) -> Self {
        NewTask {
            id: llm_task.id,
            title: llm_task.title,
            description: llm_task.description,
            depends_on: llm_task.depends_on,
            metadata: std::collections::HashMap::new(),
        }
    }
}

impl ReplanAction {
    /// Convert this action to a ReplanDecision for the scheduler.
    pub fn into_decision(self) -> ReplanDecision {
        match self.action {
            ActionType::Retry => ReplanDecision::Retry,
            ActionType::AddTasks => {
                let tasks: Vec<NewTask> = self.new_tasks.into_iter().map(Into::into).collect();
                ReplanDecision::AddTasks(tasks)
            }
            ActionType::Escalate => ReplanDecision::Escalate(self.reason),
        }
    }
}

/// Build the analysis prompt for the LLM.
pub fn build_analysis_prompt(task: &TaskInfo, error: &str, graph_context: &str) -> String {
    let deps_str = if task.depends_on.is_empty() {
        "None".to_string()
    } else {
        task.depends_on.join(", ")
    };

    format!(
        r#"A coding task failed during automated execution.

Task ID: {id}
Task Title: {title}
Description: {description}

Error output:
```
{error}
```

Dependencies: {deps}
Current graph state: {context}

Analyze this failure and decide the best recovery action."#,
        id = task.id,
        title = task.title,
        description = if task.description.is_empty() { "(no description)" } else { &task.description },
        error = truncate(error, 2000),
        deps = deps_str,
        context = graph_context,
    )
}

/// Parse the LLM response JSON into a ReplanAction.
pub fn parse_llm_response(response: &str) -> Result<ReplanAction> {
    // Try to extract JSON from the response (handle markdown code blocks)
    let json_str = extract_json(response);
    
    serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse LLM response as JSON: {}", e))
}

/// Extract JSON from a response that might be wrapped in markdown.
fn extract_json(response: &str) -> &str {
    let trimmed = response.trim();
    
    // Check for ```json ... ``` wrapper
    if let Some(start) = trimmed.find("```json") {
        let start = start + 7; // skip "```json"
        if let Some(end) = trimmed[start..].find("```") {
            return trimmed[start..start + end].trim();
        }
    }
    
    // Check for ``` ... ``` wrapper
    if let Some(start) = trimmed.find("```") {
        let start = start + 3; // skip "```"
        if let Some(end) = trimmed[start..].find("```") {
            return trimmed[start..start + end].trim();
        }
    }
    
    // Assume raw JSON
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task() -> TaskInfo {
        TaskInfo {
            id: "auth-impl".to_string(),
            title: "Implement auth".to_string(),
            description: String::new(),
            goals: vec![],
            verify: Some("cargo test".to_string()),
            estimated_turns: 15,
            depends_on: vec![],
            design_ref: None,
            satisfies: vec![],
        }
    }

    fn failed_result(output: &str) -> TaskResult {
        TaskResult {
            success: false,
            output: output.to_string(),
            turns_used: 10,
            tokens_used: 5000,
            blocker: None,
        }
    }

    #[test]
    fn test_retry_on_first_failure() {
        let mut rp = Replanner::new(3);
        let task = sample_task();
        let result = failed_result("compilation error");

        let decision = rp.analyze_failure(&task, &result, 0, 1);
        assert!(matches!(decision, ReplanDecision::Retry));
    }

    #[test]
    fn test_escalate_after_retries_exhausted() {
        let mut rp = Replanner::new(3);
        let task = sample_task();
        let result = failed_result("still failing");

        let decision = rp.analyze_failure(&task, &result, 1, 1);
        assert!(matches!(decision, ReplanDecision::Escalate(_)));
    }

    #[test]
    fn test_escalate_on_blocker() {
        let mut rp = Replanner::new(3);
        let task = sample_task();
        let result = TaskResult {
            success: false,
            output: "Blocker: missing config module".to_string(),
            turns_used: 5,
            tokens_used: 2000,
            blocker: Some("missing config module".to_string()),
        };

        let decision = rp.analyze_failure(&task, &result, 0, 1);
        assert!(matches!(decision, ReplanDecision::Escalate(_)));
    }

    #[test]
    fn test_escalate_when_replan_limit_reached() {
        let mut rp = Replanner::new(2);
        rp.replan_count = 2; // Already at limit

        let task = sample_task();
        let result = failed_result("error");

        let decision = rp.analyze_failure(&task, &result, 0, 3);
        assert!(matches!(decision, ReplanDecision::Escalate(_)));
    }

    #[test]
    fn test_retry_on_empty_output() {
        let mut rp = Replanner::new(3);
        let task = sample_task();
        let result = failed_result("");

        let decision = rp.analyze_failure(&task, &result, 0, 1);
        assert!(matches!(decision, ReplanDecision::Retry));
    }

    #[test]
    fn test_replan_count_increments() {
        let mut rp = Replanner::new(5);
        let task = sample_task();

        // Exhaust retries to trigger escalation (which increments replan_count)
        let result = failed_result("error");
        rp.analyze_failure(&task, &result, 1, 1);
        assert_eq!(rp.replan_count, 1);

        rp.analyze_failure(&task, &result, 1, 1);
        assert_eq!(rp.replan_count, 2);
    }

    #[test]
    fn test_reset_count() {
        let mut rp = Replanner::new(3);
        rp.replan_count = 2;
        rp.reset_count();
        assert_eq!(rp.replan_count, 0);
        assert!(!rp.limit_reached());
    }

    #[test]
    fn test_limit_reached() {
        let rp = Replanner { max_replans: 3, replan_count: 3 };
        assert!(rp.limit_reached());

        let rp2 = Replanner { max_replans: 3, replan_count: 2 };
        assert!(!rp2.limit_reached());
    }

    // ═══════════════════════════════════════════════════════════════════════════════
    // LLM Analysis Tests
    // ═══════════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_build_analysis_prompt() {
        let task = TaskInfo {
            id: "impl-auth".to_string(),
            title: "Implement authentication".to_string(),
            description: "Add JWT-based auth to the API".to_string(),
            goals: vec![],
            verify: Some("cargo test".to_string()),
            estimated_turns: 20,
            depends_on: vec!["setup-db".to_string()],
            design_ref: None,
            satisfies: vec![],
        };

        let prompt = super::build_analysis_prompt(
            &task,
            "error[E0433]: failed to resolve: use of undeclared crate",
            "5 tasks total, 2 completed (setup-db, init-project)",
        );

        assert!(prompt.contains("impl-auth"));
        assert!(prompt.contains("Implement authentication"));
        assert!(prompt.contains("JWT-based auth"));
        assert!(prompt.contains("E0433"));
        assert!(prompt.contains("setup-db"));
        assert!(prompt.contains("5 tasks total"));
    }

    #[test]
    fn test_build_analysis_prompt_no_deps() {
        let task = TaskInfo {
            id: "first-task".to_string(),
            title: "First task".to_string(),
            description: String::new(),
            goals: vec![],
            verify: None,
            estimated_turns: 10,
            depends_on: vec![],
            design_ref: None,
            satisfies: vec![],
        };

        let prompt = super::build_analysis_prompt(&task, "error", "initial state");
        assert!(prompt.contains("Dependencies: None"));
        assert!(prompt.contains("(no description)"));
    }

    #[test]
    fn test_parse_llm_response_retry() {
        let response = r#"{"action": "retry", "reason": "Network timeout detected"}"#;
        let action = super::parse_llm_response(response).unwrap();
        
        assert_eq!(action.action, super::ActionType::Retry);
        assert_eq!(action.reason, "Network timeout detected");
        assert!(action.new_tasks.is_empty());
    }

    #[test]
    fn test_parse_llm_response_escalate() {
        let response = r#"{
            "action": "escalate",
            "reason": "Security vulnerability in design",
            "new_tasks": []
        }"#;
        let action = super::parse_llm_response(response).unwrap();
        
        assert_eq!(action.action, super::ActionType::Escalate);
        assert!(action.reason.contains("Security"));
    }

    #[test]
    fn test_parse_llm_response_add_tasks() {
        let response = r#"{
            "action": "add_tasks",
            "reason": "Missing config module",
            "new_tasks": [
                {
                    "id": "create-config",
                    "title": "Create config module",
                    "description": "Set up the config.rs module with settings struct",
                    "depends_on": []
                },
                {
                    "id": "impl-auth-fixed",
                    "title": "Implement auth (retry)",
                    "description": "Re-attempt auth implementation after config is ready",
                    "depends_on": ["create-config"]
                }
            ]
        }"#;
        let action = super::parse_llm_response(response).unwrap();
        
        assert_eq!(action.action, super::ActionType::AddTasks);
        assert_eq!(action.new_tasks.len(), 2);
        assert_eq!(action.new_tasks[0].id, "create-config");
        assert_eq!(action.new_tasks[1].depends_on, vec!["create-config"]);
    }

    #[test]
    fn test_parse_llm_response_with_markdown() {
        let response = r#"Here's my analysis:

```json
{
    "action": "retry",
    "reason": "Flaky test failure"
}
```

The test appeared to be flaky."#;
        
        let action = super::parse_llm_response(response).unwrap();
        assert_eq!(action.action, super::ActionType::Retry);
        assert_eq!(action.reason, "Flaky test failure");
    }

    #[test]
    fn test_parse_llm_response_with_code_block() {
        let response = "```\n{\"action\": \"escalate\", \"reason\": \"Complex issue\"}\n```";
        let action = super::parse_llm_response(response).unwrap();
        assert_eq!(action.action, super::ActionType::Escalate);
    }

    #[test]
    fn test_parse_llm_response_invalid() {
        let response = "This is not valid JSON";
        let result = super::parse_llm_response(response);
        assert!(result.is_err());
    }

    #[test]
    fn test_replan_action_into_decision() {
        // Retry
        let action = super::ReplanAction {
            action: super::ActionType::Retry,
            reason: "transient".to_string(),
            new_tasks: vec![],
        };
        assert!(matches!(action.into_decision(), super::ReplanDecision::Retry));

        // Escalate
        let action = super::ReplanAction {
            action: super::ActionType::Escalate,
            reason: "need human".to_string(),
            new_tasks: vec![],
        };
        match action.into_decision() {
            super::ReplanDecision::Escalate(msg) => assert!(msg.contains("need human")),
            _ => panic!("Expected Escalate"),
        }

        // AddTasks
        let action = super::ReplanAction {
            action: super::ActionType::AddTasks,
            reason: "missing dep".to_string(),
            new_tasks: vec![super::LlmNewTask {
                id: "new-task".to_string(),
                title: "New".to_string(),
                description: "Desc".to_string(),
                depends_on: vec![],
            }],
        };
        match action.into_decision() {
            super::ReplanDecision::AddTasks(tasks) => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].id, "new-task");
            }
            _ => panic!("Expected AddTasks"),
        }
    }

    #[test]
    fn test_llm_new_task_into_new_task() {
        let llm_task = super::LlmNewTask {
            id: "task-1".to_string(),
            title: "Task One".to_string(),
            description: "Do something".to_string(),
            depends_on: vec!["task-0".to_string()],
        };

        let new_task: super::NewTask = llm_task.into();
        assert_eq!(new_task.id, "task-1");
        assert_eq!(new_task.title, "Task One");
        assert_eq!(new_task.description, "Do something");
        assert_eq!(new_task.depends_on, vec!["task-0"]);
        assert!(new_task.metadata.is_empty());
    }

    #[test]
    fn test_fallback_analysis_transient() {
        let mut rp = Replanner::new(3);
        let task = sample_task();
        
        let action = rp.fallback_analysis(&task, "");
        assert_eq!(action.action, super::ActionType::Retry);
        
        let action = rp.fallback_analysis(&task, "Connection timeout occurred");
        assert_eq!(action.action, super::ActionType::Retry);
        
        let action = rp.fallback_analysis(&task, "Network connection reset");
        assert_eq!(action.action, super::ActionType::Retry);
    }

    #[test]
    fn test_fallback_analysis_escalate() {
        let mut rp = Replanner::new(3);
        let task = sample_task();
        
        let action = rp.fallback_analysis(&task, "Missing module 'config' not found");
        assert_eq!(action.action, super::ActionType::Escalate);
        assert_eq!(rp.replan_count, 1);
        
        let action = rp.fallback_analysis(&task, "Generic error that we don't recognize");
        assert_eq!(action.action, super::ActionType::Escalate);
        assert_eq!(rp.replan_count, 2);
    }

    #[test]
    fn test_extract_json() {
        // Raw JSON
        assert_eq!(super::extract_json(r#"{"a": 1}"#), r#"{"a": 1}"#);
        
        // With json code block
        let input = "```json\n{\"a\": 1}\n```";
        assert_eq!(super::extract_json(input), r#"{"a": 1}"#);
        
        // With plain code block
        let input = "```\n{\"b\": 2}\n```";
        assert_eq!(super::extract_json(input), r#"{"b": 2}"#);
        
        // With surrounding text
        let input = "Here's the JSON:\n```json\n{\"c\": 3}\n```\nThat's it.";
        assert_eq!(super::extract_json(input), r#"{"c": 3}"#);
    }
}
