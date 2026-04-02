//! Scheduler — drives execution of the plan, managing task lifecycle.
//!
//! The scheduler processes layers sequentially, spawning tasks in parallel
//! (up to `max_concurrent`). It coordinates between the executor, worktree
//! manager, verifier, and telemetry logger.
//!
//! Task state machine: `todo` → `in_progress` → `done` | `failed` | `blocked`
//!
//! After each task completes, state is persisted to `graph.yml` for
//! crash recovery (GUARD-7).

use std::collections::HashMap;
use std::time::Instant;

use anyhow::Result;
use chrono::Utc;
use tracing::{info, warn, error};

use std::path::{Path, PathBuf};

use gid_core::graph::{Graph, NodeStatus};
use gid_core::harness::types::{
    ExecutionPlan, ExecutionResult, HarnessConfig,
    ExecutionEvent, VerifyResult,
};
use gid_core::code_graph::CodeGraph;
use gid_core::unified::build_unified_graph;
use gid_core::advise::analyze as advise_analyze;
use gid_core::save_graph;

use crate::executor::TaskExecutor;
use crate::replanner::Replanner;
use crate::verifier::Verifier;
use crate::worktree::WorktreeManager;
use crate::telemetry::TelemetryLogger;

/// Execute a plan by driving the full task lifecycle.
///
/// Processes layers sequentially. Within each layer, spawns tasks
/// in parallel up to `config.max_concurrent`. After each layer,
/// runs the layer checkpoint.
///
/// # Arguments
/// - `plan` — the execution plan from `gid_core::harness::create_plan()`
/// - `graph` — mutable graph for updating task statuses
/// - `config` — harness configuration
/// - `executor` — sub-agent spawner (trait object)
/// - `worktree_mgr` — git worktree manager (trait object)
///
/// # Returns
/// An [`ExecutionResult`] summarizing the execution.
pub async fn execute_plan(
    plan: &ExecutionPlan,
    graph: &mut Graph,
    config: &HarnessConfig,
    executor: &dyn TaskExecutor,
    worktree_mgr: &dyn WorktreeManager,
    gid_root: &Path,
) -> Result<ExecutionResult> {
    let graph_path = gid_root.join("graph.yml");
    let start = Instant::now();

    info!(
        total_tasks = plan.total_tasks,
        layers = plan.layers.len(),
        max_concurrent = config.max_concurrent,
        "Starting plan execution"
    );

    // Clean up stale worktrees from previous runs
    match worktree_mgr.cleanup_stale().await {
        Ok(0) => {},
        Ok(n) => info!(count = n, "Cleaned up stale worktrees from previous run"),
        Err(e) => warn!(error = %e, "Failed to clean up stale worktrees"),
    }

    // Initialize telemetry — log path would come from config in production
    // For now, we skip telemetry if no path is configured
    let telemetry = TelemetryLogger::new(".gid/execution-log.jsonl");
    telemetry.log_event(&ExecutionEvent::Plan {
        total_tasks: plan.total_tasks,
        layers: plan.layers.len(),
        timestamp: Utc::now(),
    }).ok(); // Non-fatal if telemetry fails

    let mut total_turns: u32 = 0;
    let mut total_tokens: u64 = 0;
    let mut tasks_completed: usize = 0;
    let mut tasks_failed: usize = 0;
    let mut retry_counts: HashMap<String, u32> = HashMap::new();
    let mut replanner = Replanner::new(config.max_replans);

    // Initialize verifier
    let verifier = Verifier::new(".")
        .with_checkpoint(
            config.default_checkpoint.clone().unwrap_or_default()
        );

    for layer in &plan.layers {
        info!(layer = layer.index, task_count = layer.tasks.len(), "Processing layer");

        // Process tasks in parallel within the layer (up to max_concurrent)
        let mut layer_results = Vec::new();

        // Phase 1: Filter eligible tasks and prepare worktrees
        let mut eligible_tasks = Vec::new();
        for task in &layer.tasks {
            // Skip already-done tasks (idempotent execution — GUARD-7)
            if let Some(node) = graph.get_node(&task.id) {
                if node.status == NodeStatus::Done {
                    info!(task_id = %task.id, "Task already done, skipping");
                    continue;
                }
            }

            // Check all dependencies are done
            let deps_satisfied = task.depends_on.iter().all(|dep_id| {
                graph.get_node(dep_id)
                    .map(|n| n.status == NodeStatus::Done)
                    .unwrap_or(true)
            });

            if !deps_satisfied {
                warn!(task_id = %task.id, "Dependencies not satisfied, marking blocked");
                if let Some(node) = graph.get_node_mut(&task.id) {
                    node.status = NodeStatus::Blocked;
                }
                save_graph(graph, &graph_path).ok();
                tasks_failed += 1;
                continue;
            }

            eligible_tasks.push(task.clone());
        }

        // Phase 2: Process in chunks of max_concurrent, spawn in parallel
        for chunk in eligible_tasks.chunks(config.max_concurrent) {
            // 2a: Mark all tasks in chunk as in-progress and create worktrees
            let mut prepared: Vec<(gid_core::harness::types::TaskInfo, PathBuf, gid_core::harness::types::TaskContext)> = Vec::new();

            for task in chunk {
                if let Some(node) = graph.get_node_mut(&task.id) {
                    node.status = NodeStatus::InProgress;
                }
                save_graph(graph, &graph_path).ok();

                telemetry.log_event(&ExecutionEvent::TaskStart {
                    task_id: task.id.clone(),
                    layer: layer.index,
                    timestamp: Utc::now(),
                }).ok();

                let wt_path = match worktree_mgr.create(&task.id).await {
                    Ok(path) => path,
                    Err(e) => {
                        error!(task_id = %task.id, error = %e, "Failed to create worktree");
                        if let Some(node) = graph.get_node_mut(&task.id) {
                            node.status = NodeStatus::Failed;
                        }
                        save_graph(graph, &graph_path).ok();
                        tasks_failed += 1;
                        continue;
                    }
                };

                // Build full context via assemble_task_context (resolves design docs, goals, guards)
                let context = match gid_core::harness::assemble_task_context(graph, &task.id, gid_root) {
                    Ok(ctx) => ctx,
                    Err(e) => {
                        warn!(task_id = %task.id, error = %e, "Context assembly failed, using basic context");
                        gid_core::harness::types::TaskContext {
                            task_info: task.clone(),
                            goals_text: task.goals.clone(),
                            design_excerpt: None,
                            dependency_interfaces: vec![],
                            guards: vec![],
                        }
                    }
                };

                prepared.push((task.clone(), wt_path, context));
            }

            // 2b: Spawn all sub-agents in parallel
            let task_start = Instant::now();
            let spawn_futures: Vec<_> = prepared.iter().map(|(_, wt_path, context)| {
                executor.spawn(context, wt_path, config)
            }).collect();
            let results = futures::future::join_all(spawn_futures).await;

            // 2c: Process results sequentially (verify, merge, update graph)
            for (i, result) in results.into_iter().enumerate() {
                let (ref task, ref wt_path, _) = prepared[i];
                let duration = task_start.elapsed();

                match result {
                    Ok(task_result) => {
                        total_turns += task_result.turns_used;
                        total_tokens += task_result.tokens_used;

                        if task_result.success {
                            let verify_result = verifier.verify_task(task, wt_path).await
                                .unwrap_or(VerifyResult::Fail {
                                    output: "Verify command failed to execute".to_string(),
                                    exit_code: -1,
                                });

                            match verify_result {
                                VerifyResult::Pass => {
                                    match worktree_mgr.merge(&task.id).await {
                                        Ok(()) => {
                                            if let Some(node) = graph.get_node_mut(&task.id) {
                                                node.status = NodeStatus::Done;
                                            }
                                            tasks_completed += 1;
                                            telemetry.log_event(&ExecutionEvent::TaskDone {
                                                task_id: task.id.clone(),
                                                turns: task_result.turns_used,
                                                tokens: task_result.tokens_used,
                                                duration_s: duration.as_secs(),
                                                verify: "pass".to_string(),
                                                timestamp: Utc::now(),
                                            }).ok();
                                        }
                                        Err(e) => {
                                            warn!(task_id = %task.id, error = %e, "Merge failed");
                                            if let Some(node) = graph.get_node_mut(&task.id) {
                                                node.status = NodeStatus::NeedsResolution;
                                            }
                                            tasks_failed += 1;
                                        }
                                    }
                                }
                                VerifyResult::Fail { ref output, exit_code } => {
                                    warn!(task_id = %task.id, exit_code, "Task verification failed");
                                    worktree_mgr.cleanup(&task.id).await.ok();
                                    if let Some(node) = graph.get_node_mut(&task.id) {
                                        node.status = NodeStatus::Failed;
                                    }
                                    tasks_failed += 1;
                                    telemetry.log_event(&ExecutionEvent::TaskFailed {
                                        task_id: task.id.clone(),
                                        reason: format!("Verify failed (exit {}): {}", exit_code, truncate(output, 200)),
                                        turns: task_result.turns_used,
                                        timestamp: Utc::now(),
                                    }).ok();
                                }
                            }
                        } else {
                            // Sub-agent failed — use replanner to decide action
                            worktree_mgr.cleanup(&task.id).await.ok();
                            let retries = retry_counts.entry(task.id.clone()).or_insert(0);
                            let decision = replanner.analyze_failure(
                                task, &task_result, *retries, config.max_retries,
                            );

                            match decision {
                                gid_core::harness::types::ReplanDecision::Retry => {
                                    *retries += 1;
                                    warn!(task_id = %task.id, retry = *retries, "Replanner: retry");
                                    if let Some(node) = graph.get_node_mut(&task.id) {
                                        node.status = NodeStatus::Todo;
                                    }
                                }
                                gid_core::harness::types::ReplanDecision::Escalate(reason) => {
                                    warn!(task_id = %task.id, reason = %reason, "Replanner: escalate");
                                    if let Some(node) = graph.get_node_mut(&task.id) {
                                        node.status = NodeStatus::Failed;
                                    }
                                    tasks_failed += 1;
                                    telemetry.log_event(&ExecutionEvent::TaskFailed {
                                        task_id: task.id.clone(),
                                        reason,
                                        turns: task_result.turns_used,
                                        timestamp: Utc::now(),
                                    }).ok();
                                }
                                gid_core::harness::types::ReplanDecision::AddTasks(new_tasks) => {
                                    // Future: add new tasks to graph and re-plan
                                    info!(task_id = %task.id, new = new_tasks.len(), "Replanner: add tasks (not yet implemented)");
                                    if let Some(node) = graph.get_node_mut(&task.id) {
                                        node.status = NodeStatus::Failed;
                                    }
                                    tasks_failed += 1;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(task_id = %task.id, error = %e, "Executor spawn error");
                        worktree_mgr.cleanup(&task.id).await.ok();
                        if let Some(node) = graph.get_node_mut(&task.id) {
                            node.status = NodeStatus::Failed;
                        }
                        tasks_failed += 1;
                    }
                }

                save_graph(graph, &graph_path).ok(); // GUARD-7
                layer_results.push(task.id.clone());
            }
        }

        // Run layer checkpoint
        let checkpoint_result = verifier.verify_layer(layer).await
            .unwrap_or(VerifyResult::Pass);

        let checkpoint_str = match &checkpoint_result {
            VerifyResult::Pass => "pass".to_string(),
            VerifyResult::Fail { output, .. } => format!("fail: {}", truncate(output, 200)),
        };

        if let Some(ref cmd) = layer.checkpoint {
            telemetry.log_event(&ExecutionEvent::Checkpoint {
                layer: layer.index,
                command: cmd.clone(),
                result: checkpoint_str,
                timestamp: Utc::now(),
            }).ok();
        }

        if matches!(checkpoint_result, VerifyResult::Fail { .. }) {
            warn!(layer = layer.index, "Layer checkpoint failed");
            // Continue to next layer — failed tasks are already marked
        }

        // Run guard checks
        let guard_checks: Vec<(&str, &gid_core::harness::types::GuardCheck)> = config.invariant_checks.iter()
            .map(|(id, check)| (id.as_str(), check))
            .collect();

        if !guard_checks.is_empty() {
            let guard_results = verifier.verify_guards(&guard_checks).await?;
            for gr in &guard_results {
                if !gr.passed {
                    warn!(
                        guard = %gr.guard_id,
                        expected = %gr.expected_output,
                        actual = %gr.actual_output,
                        "Guard check failed after layer {}", layer.index
                    );
                }
            }
        }

        // Post-layer code graph extraction (GOAL-2.18, GOAL-2.19, GOAL-2.20)
        // Extract code nodes from source directory and merge into graph
        info!(layer = layer.index, "Running post-layer extract");
        if let Err(e) = post_layer_extract(graph).await {
            warn!(layer = layer.index, error = %e, "Post-layer extract failed (non-fatal)");
        }
    }

    let duration = start.elapsed();

    // Post-execution quality check (GOAL-2.21, GOAL-2.22)
    info!("Running post-execution advise");
    if let Err(e) = post_execution_advise(graph, &telemetry).await {
        warn!(error = %e, "Post-execution advise failed (non-fatal)");
    }

    // Log completion
    telemetry.log_event(&ExecutionEvent::Complete {
        total_turns,
        total_tokens,
        duration_s: duration.as_secs(),
        failed: tasks_failed,
        timestamp: Utc::now(),
    }).ok();

    info!(
        tasks_completed,
        tasks_failed,
        total_turns,
        duration_secs = duration.as_secs(),
        "Plan execution complete"
    );

    Ok(ExecutionResult {
        tasks_completed,
        tasks_failed,
        total_turns,
        total_tokens,
        duration_secs: duration.as_secs(),
    })
}

/// Truncate a string to max_len characters.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

/// Post-layer code graph extraction.
///
/// Extracts code nodes from the project source directory and merges them into
/// the graph, preserving semantic nodes (feature, task) and updating only
/// structural nodes (file, class, function).
///
/// Satisfies: GOAL-2.18, GOAL-2.19, GOAL-2.20
async fn post_layer_extract(graph: &mut Graph) -> Result<()> {
    // Determine project root — look for Cargo.toml, package.json, etc.
    // For now, assume current directory (caller should set cwd appropriately)
    let project_root = std::env::current_dir()?;
    let src_dir = project_root.join("src");
    
    if !src_dir.exists() {
        // No src/ directory — skip extract (might be a non-code project)
        info!("No src/ directory found, skipping extract");
        return Ok(());
    }

    info!(project_root = %project_root.display(), "Extracting code graph");
    let code_graph = CodeGraph::extract_from_dir(&src_dir);
    
    // Merge code nodes into existing graph, preserving semantic nodes
    let unified = build_unified_graph(&code_graph, graph);
    *graph = unified;
    
    info!(
        code_nodes = code_graph.nodes.len(),
        "Code graph extraction complete"
    );
    
    Ok(())
}

/// Post-execution quality check via advise.
///
/// Runs graph quality analysis and logs the result to telemetry.
/// Failures are logged as warnings but do not block or revert work.
///
/// Satisfies: GOAL-2.21, GOAL-2.22
async fn post_execution_advise(
    graph: &Graph,
    telemetry: &TelemetryLogger,
) -> Result<()> {
    let result = advise_analyze(graph);
    
    telemetry.log_event(&ExecutionEvent::Advise {
        passed: result.passed,
        score: result.health_score,
        issues: result.items.len(),
        timestamp: Utc::now(),
    }).ok();
    
    if result.passed {
        info!(score = result.health_score, "Graph quality check passed");
    } else {
        warn!(
            score = result.health_score,
            issues = result.items.len(),
            "Graph quality check failed (non-fatal)"
        );
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use async_trait::async_trait;

    use gid_core::graph::{Node, Edge};
    use gid_core::harness::types::*;
    use crate::executor::TaskExecutor;
    use crate::worktree::WorktreeManager;

    /// Mock executor that always succeeds.
    struct MockSuccessExecutor;

    #[async_trait]
    impl TaskExecutor for MockSuccessExecutor {
        async fn spawn(&self, _ctx: &TaskContext, _wt: &Path, _cfg: &HarnessConfig) -> Result<TaskResult> {
            Ok(TaskResult {
                success: true,
                output: "Done".to_string(),
                turns_used: 5,
                tokens_used: 1000,
                blocker: None,
            })
        }
    }

    /// Mock executor that always fails.
    struct MockFailExecutor;

    #[async_trait]
    impl TaskExecutor for MockFailExecutor {
        async fn spawn(&self, _ctx: &TaskContext, _wt: &Path, _cfg: &HarnessConfig) -> Result<TaskResult> {
            Ok(TaskResult {
                success: false,
                output: "Error: compilation failed".to_string(),
                turns_used: 3,
                tokens_used: 500,
                blocker: None,
            })
        }
    }

    /// Mock executor that counts spawns.
    struct MockCountExecutor {
        count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl TaskExecutor for MockCountExecutor {
        async fn spawn(&self, _ctx: &TaskContext, _wt: &Path, _cfg: &HarnessConfig) -> Result<TaskResult> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(TaskResult {
                success: true,
                output: "Done".to_string(),
                turns_used: 1,
                tokens_used: 100,
                blocker: None,
            })
        }
    }

    /// Mock worktree manager that uses temp dirs.
    struct MockWorktreeManager;

    #[async_trait]
    impl WorktreeManager for MockWorktreeManager {
        async fn create(&self, task_id: &str) -> Result<PathBuf> {
            let path = std::env::temp_dir().join(format!("gid-test-wt-{}", task_id));
            std::fs::create_dir_all(&path).ok();
            Ok(path)
        }
        async fn merge(&self, _task_id: &str) -> Result<()> {
            Ok(())
        }
        async fn cleanup(&self, task_id: &str) -> Result<()> {
            let path = std::env::temp_dir().join(format!("gid-test-wt-{}", task_id));
            std::fs::remove_dir_all(&path).ok();
            Ok(())
        }
        async fn list_existing(&self) -> Result<Vec<WorktreeInfo>> {
            Ok(vec![])
        }
        async fn cleanup_stale(&self) -> Result<usize> {
            Ok(0)
        }
    }

    fn make_task(id: &str, title: &str) -> Node {
        let mut n = Node::new(id, title);
        n.node_type = Some("task".to_string());
        n
    }

    fn make_plan(tasks: Vec<TaskInfo>, layers_spec: Vec<Vec<usize>>) -> ExecutionPlan {
        let mut layers = Vec::new();
        for (idx, task_indices) in layers_spec.iter().enumerate() {
            let layer_tasks: Vec<TaskInfo> = task_indices.iter()
                .map(|&i| tasks[i].clone())
                .collect();
            layers.push(ExecutionLayer {
                index: idx,
                tasks: layer_tasks,
                checkpoint: None,
            });
        }
        ExecutionPlan {
            total_tasks: tasks.len(),
            layers,
            critical_path: vec![],
            estimated_total_turns: tasks.iter().map(|t| t.estimated_turns).sum(),
        }
    }

    fn simple_task_info(id: &str) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            title: format!("Task {}", id),
            description: String::new(),
            goals: vec![],
            verify: None, // No verify = always pass
            estimated_turns: 10,
            depends_on: vec![],
            design_ref: None,
            satisfies: vec![],
        }
    }

    #[tokio::test]
    async fn test_execute_plan_single_task() {
        let mut graph = Graph::new();
        graph.add_node(make_task("a", "Task A"));

        let task = simple_task_info("a");
        let plan = make_plan(vec![task], vec![vec![0]]);
        let config = HarnessConfig::default();

        let result = execute_plan(
            &plan,
            &mut graph,
            &config,
            &MockSuccessExecutor,
            &MockWorktreeManager,
            &std::env::temp_dir().join("gid-test-root").join(".gid"),
        ).await.unwrap();

        assert_eq!(result.tasks_completed, 1);
        assert_eq!(result.tasks_failed, 0);
        assert_eq!(graph.get_node("a").unwrap().status, NodeStatus::Done);
    }

    #[tokio::test]
    async fn test_execute_plan_failed_task() {
        let mut graph = Graph::new();
        graph.add_node(make_task("a", "Task A"));

        let task = simple_task_info("a");
        let plan = make_plan(vec![task], vec![vec![0]]);
        let config = HarnessConfig { max_retries: 0, ..Default::default() };

        let result = execute_plan(
            &plan,
            &mut graph,
            &config,
            &MockFailExecutor,
            &MockWorktreeManager,
            &std::env::temp_dir().join("gid-test-root").join(".gid"),
        ).await.unwrap();

        assert_eq!(result.tasks_completed, 0);
        assert_eq!(result.tasks_failed, 1);
        assert_eq!(graph.get_node("a").unwrap().status, NodeStatus::Failed);
    }

    #[tokio::test]
    async fn test_execute_plan_skips_done_tasks() {
        let mut graph = Graph::new();
        let mut done = make_task("a", "Already Done");
        done.status = NodeStatus::Done;
        graph.add_node(done);
        graph.add_node(make_task("b", "Task B"));

        let tasks = vec![simple_task_info("a"), simple_task_info("b")];
        let plan = make_plan(tasks, vec![vec![0, 1]]);

        let count = Arc::new(AtomicUsize::new(0));
        let executor = MockCountExecutor { count: count.clone() };
        let config = HarnessConfig::default();

        let result = execute_plan(
            &plan,
            &mut graph,
            &config,
            &executor,
            &MockWorktreeManager,
            &std::env::temp_dir().join("gid-test-root").join(".gid"),
        ).await.unwrap();

        // Only task "b" should have been spawned
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(result.tasks_completed, 1);
    }

    #[tokio::test]
    async fn test_execute_plan_multi_layer() {
        let mut graph = Graph::new();
        graph.add_node(make_task("a", "Base"));
        graph.add_node(make_task("b", "Depends on A"));
        graph.add_edge(Edge::depends_on("b", "a"));

        let task_a = simple_task_info("a");
        let mut task_b = simple_task_info("b");
        task_b.depends_on = vec!["a".to_string()];

        let plan = make_plan(vec![task_a, task_b], vec![vec![0], vec![1]]);
        let config = HarnessConfig::default();

        let result = execute_plan(
            &plan,
            &mut graph,
            &config,
            &MockSuccessExecutor,
            &MockWorktreeManager,
            &std::env::temp_dir().join("gid-test-root").join(".gid"),
        ).await.unwrap();

        assert_eq!(result.tasks_completed, 2);
        assert_eq!(graph.get_node("a").unwrap().status, NodeStatus::Done);
        assert_eq!(graph.get_node("b").unwrap().status, NodeStatus::Done);
    }

    #[tokio::test]
    async fn test_execute_empty_plan() {
        let mut graph = Graph::new();
        let plan = ExecutionPlan {
            total_tasks: 0,
            layers: vec![],
            critical_path: vec![],
            estimated_total_turns: 0,
        };
        let config = HarnessConfig::default();

        let result = execute_plan(
            &plan,
            &mut graph,
            &config,
            &MockSuccessExecutor,
            &MockWorktreeManager,
            &std::env::temp_dir().join("gid-test-root").join(".gid"),
        ).await.unwrap();

        assert_eq!(result.tasks_completed, 0);
        assert_eq!(result.tasks_failed, 0);
    }
}
