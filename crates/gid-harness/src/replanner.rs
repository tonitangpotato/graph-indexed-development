//! Adaptive re-planner — analyze failures and decide recovery actions.
//!
//! The re-planner inspects task failures and chooses between:
//! - **Retry**: simple/transient failures (timeout, flaky test)
//! - **AddTasks**: structural issues (missing dependency, wrong interface)
//! - **Escalate**: unresolvable problems (notify human)
//!
//! The main agent (LLM) makes the actual decision; this module provides
//! the analysis framework and enforces limits.

use tracing::{info, warn};

use gid_core::harness::types::{TaskInfo, TaskResult, ReplanDecision};

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

        // If sub-agent reported a blocker, escalate
        if let Some(ref blocker) = result.blocker {
            self.replan_count += 1;
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
}

/// Truncate a string to max_len characters, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
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
}
