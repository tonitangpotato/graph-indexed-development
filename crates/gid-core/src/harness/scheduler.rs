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
use tracing::{info, warn, error, debug};

use std::path::{Path, PathBuf};

use crate::graph::{Graph, Node, Edge, NodeStatus};
use super::types::{
    ExecutionPlan, ExecutionResult, HarnessConfig,
    ExecutionEvent, VerifyResult, TaskInfo, TaskResult, NewTask,
};
use crate::code_graph::CodeGraph;

use crate::advise::analyze as advise_analyze;
use crate::save_graph;

use super::executor::TaskExecutor;
use super::replanner::Replanner;
use super::verifier::Verifier;
use super::worktree::WorktreeManager;
use super::telemetry::TelemetryLogger;
use super::execution_state::ExecutionState;

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

    // Load or create execution state
    let mut exec_state = ExecutionState::load(gid_root).unwrap_or_default();
    exec_state.start_running();
    exec_state.save(gid_root).ok();

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
        // Check for cancellation at the start of each layer (GOAL-6.18)
        exec_state = ExecutionState::load(gid_root).unwrap_or(exec_state);
        if exec_state.is_cancel_requested() {
            warn!("Cancellation requested, stopping execution gracefully");
            // Mark any in-progress tasks back to todo (not failed)
            for node in graph.nodes.iter_mut() {
                if node.status == NodeStatus::InProgress {
                    node.status = NodeStatus::Todo;
                }
            }
            save_graph(graph, &graph_path).ok();
            exec_state.mark_cancelled();
            exec_state.save(gid_root).ok();

            telemetry.log_event(&ExecutionEvent::Cancel {
                tasks_done: tasks_completed,
                tasks_remaining: plan.total_tasks - tasks_completed - tasks_failed,
                timestamp: Utc::now(),
            }).ok();

            return Ok(ExecutionResult {
                tasks_completed,
                tasks_failed,
                total_turns,
                total_tokens,
                duration_secs: start.elapsed().as_secs(),
            });
        }

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
            let mut prepared: Vec<(super::types::TaskInfo, PathBuf, super::types::TaskContext)> = Vec::new();

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
                let context = match super::assemble_task_context(graph, &task.id, gid_root) {
                    Ok(ctx) => ctx,
                    Err(e) => {
                        warn!(task_id = %task.id, error = %e, "Context assembly failed, using basic context");
                        super::types::TaskContext {
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

            // Update execution state with active tasks (GOAL-6.3)
            let active_task_ids: Vec<String> = prepared.iter()
                .map(|(task, _, _)| task.id.clone())
                .collect();
            exec_state.set_active_tasks(active_task_ids);
            exec_state.save(gid_root).ok();

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

                            // Try LLM-powered analysis if auth pool is available
                            let decision = analyze_task_failure(
                                &mut replanner,
                                task,
                                &task_result,
                                *retries,
                                config,
                                graph,
                            ).await;

                            match decision {
                                super::types::ReplanDecision::Retry => {
                                    *retries += 1;
                                    warn!(task_id = %task.id, retry = *retries, "Replanner: retry");
                                    if let Some(node) = graph.get_node_mut(&task.id) {
                                        node.status = NodeStatus::Todo;
                                    }
                                }
                                super::types::ReplanDecision::Escalate(reason) => {
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
                                super::types::ReplanDecision::AddTasks(new_tasks) => {
                                    // Add new tasks to graph and mark original as blocked
                                    info!(
                                        task_id = %task.id,
                                        new_count = new_tasks.len(),
                                        "Replanner: adding prerequisite tasks"
                                    );

                                    // Add the new tasks as nodes
                                    for new_task in &new_tasks {
                                        add_task_to_graph(graph, new_task, &task.id);
                                    }

                                    // Log the replan event
                                    telemetry.log_event(&ExecutionEvent::Replan {
                                        new_tasks: new_tasks.iter().map(|t| t.id.clone()).collect(),
                                        new_edges: new_tasks.iter()
                                            .flat_map(|t| t.depends_on.iter().map(|d| (t.id.clone(), d.clone())))
                                            .collect(),
                                        timestamp: Utc::now(),
                                    }).ok();

                                    // Mark original task as blocked (will be retried after new tasks complete)
                                    if let Some(node) = graph.get_node_mut(&task.id) {
                                        node.status = NodeStatus::Blocked;
                                    }

                                    // Save graph with new tasks
                                    save_graph(graph, &graph_path).ok();
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
        let guard_checks: Vec<(&str, &super::types::GuardCheck)> = config.invariant_checks.iter()
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

    // Mark execution as completed (GOAL-6.3)
    exec_state.complete();
    exec_state.save(gid_root).ok();

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
    
    // Convert CodeGraph → graph nodes/edges and merge into existing graph
    let (code_nodes, code_edges) = crate::unify::codegraph_to_graph_nodes(&code_graph, &project_root);
    crate::unify::merge_code_layer(graph, code_nodes, code_edges);
    
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

// ═══════════════════════════════════════════════════════════════════════════════
// LLM-Powered Replanning Helpers
// ═══════════════════════════════════════════════════════════════════════════════

/// Analyze a task failure, using LLM if auth pool is available.
///
/// This function:
/// 1. Checks if auth pool exists (at config path or default ~/.agentctl/auth.toml)
/// 2. If yes, calls `analyze_failure_with_llm` for nuanced analysis
/// 3. If no or LLM fails, falls back to heuristic `analyze_failure`
async fn analyze_task_failure(
    replanner: &mut Replanner,
    task: &TaskInfo,
    result: &TaskResult,
    retry_count: u32,
    config: &HarnessConfig,
    graph: &Graph,
) -> super::types::ReplanDecision {
    // Determine auth pool path
    let pool_path = config.auth_pool_path.clone().unwrap_or_else(|| {
        dirs::home_dir()
            .map(|h| h.join(".agentctl").join("auth.toml"))
            .unwrap_or_else(|| PathBuf::from(".agentctl/auth.toml"))
    });

    // Check if auth pool exists before trying LLM
    if pool_path.exists() {
        debug!(pool_path = %pool_path.display(), "Auth pool found, using LLM analysis");

        let graph_context = build_graph_context(graph);

        match replanner.analyze_failure_with_llm(
            task,
            &result.output,
            &graph_context,
            &pool_path,
        ).await {
            Ok(action) => {
                return action.into_decision();
            }
            Err(e) => {
                warn!(error = %e, "LLM analysis failed, falling back to heuristics");
            }
        }
    } else {
        debug!(pool_path = %pool_path.display(), "No auth pool, using heuristic analysis");
    }

    // Fallback to heuristic-based analysis
    replanner.analyze_failure(task, result, retry_count, config.max_retries)
}

/// Build a summary of the current graph state for LLM context.
fn build_graph_context(graph: &Graph) -> String {
    let total_tasks = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("task"))
        .count();

    let completed = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("task") && n.status == NodeStatus::Done)
        .count();

    let failed = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("task") && n.status == NodeStatus::Failed)
        .count();

    let in_progress = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("task") && n.status == NodeStatus::InProgress)
        .count();

    let blocked = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("task") && n.status == NodeStatus::Blocked)
        .count();

    let completed_ids: Vec<String> = graph.nodes.iter()
        .filter(|n| n.node_type.as_deref() == Some("task") && n.status == NodeStatus::Done)
        .take(10) // Limit to avoid huge prompts
        .map(|n| n.id.clone())
        .collect();

    let completed_str = if completed_ids.is_empty() {
        "none".to_string()
    } else if completed_ids.len() < completed {
        format!("{} (and {} more)", completed_ids.join(", "), completed - completed_ids.len())
    } else {
        completed_ids.join(", ")
    };

    format!(
        "{} tasks total: {} completed ({}), {} in progress, {} failed, {} blocked",
        total_tasks, completed, completed_str, in_progress, failed, blocked
    )
}

/// Add a new task to the graph with proper edges.
fn add_task_to_graph(graph: &mut Graph, new_task: &NewTask, blocked_by: &str) {
    // Create the node
    let mut node = Node::new(&new_task.id, &new_task.title);
    node.node_type = Some("task".to_string());
    node.description = Some(new_task.description.clone());
    node.status = NodeStatus::Todo;

    // Add metadata if any
    if !new_task.metadata.is_empty() {
        node.metadata = new_task.metadata.clone();
    }

    graph.add_node(node);

    // Add dependency edges (new task depends on these)
    for dep in &new_task.depends_on {
        graph.add_edge(Edge::depends_on(&new_task.id, dep));
    }

    // The blocked task should now depend on this new task
    graph.add_edge(Edge::depends_on(blocked_by, &new_task.id));

    info!(
        new_task_id = %new_task.id,
        blocked_task = %blocked_by,
        deps = ?new_task.depends_on,
        "Added new prerequisite task to graph"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use async_trait::async_trait;

    use crate::graph::{Node, Edge};
    use crate::harness::types::*;
    use crate::harness::executor::TaskExecutor;
    use crate::harness::worktree::WorktreeManager;

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

    // ═══════════════════════════════════════════════════════════════════════════════
    // Tests for LLM Replanning Helpers
    // ═══════════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_build_graph_context_empty() {
        let graph = Graph::new();
        let ctx = build_graph_context(&graph);
        assert!(ctx.contains("0 tasks total"));
        assert!(ctx.contains("0 completed"));
    }

    #[test]
    fn test_build_graph_context_with_tasks() {
        let mut graph = Graph::new();
        
        let mut done1 = make_task("task-1", "Task 1");
        done1.status = NodeStatus::Done;
        graph.add_node(done1);
        
        let mut done2 = make_task("task-2", "Task 2");
        done2.status = NodeStatus::Done;
        graph.add_node(done2);
        
        let mut in_progress = make_task("task-3", "Task 3");
        in_progress.status = NodeStatus::InProgress;
        graph.add_node(in_progress);
        
        let mut failed = make_task("task-4", "Task 4");
        failed.status = NodeStatus::Failed;
        graph.add_node(failed);

        let ctx = build_graph_context(&graph);
        assert!(ctx.contains("4 tasks total"));
        assert!(ctx.contains("2 completed"));
        assert!(ctx.contains("1 in progress"));
        assert!(ctx.contains("1 failed"));
        // Check that task IDs are included
        assert!(ctx.contains("task-1") || ctx.contains("task-2"));
    }

    #[test]
    fn test_add_task_to_graph() {
        let mut graph = Graph::new();
        
        // Add existing task that will be blocked
        graph.add_node(make_task("existing-task", "Existing"));
        
        let new_task = NewTask {
            id: "new-prereq".to_string(),
            title: "New Prerequisite".to_string(),
            description: "Must complete before existing task".to_string(),
            depends_on: vec![],
            metadata: std::collections::HashMap::new(),
        };

        add_task_to_graph(&mut graph, &new_task, "existing-task");

        // Check new task was added
        let added = graph.get_node("new-prereq").unwrap();
        assert_eq!(added.title, "New Prerequisite");
        assert_eq!(added.status, NodeStatus::Todo);
        assert_eq!(added.node_type.as_deref(), Some("task"));
        assert_eq!(added.description.as_deref(), Some("Must complete before existing task"));

        // Check dependency edge was added
        let edges: Vec<_> = graph.edges.iter()
            .filter(|e| e.from == "existing-task" && e.to == "new-prereq")
            .collect();
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn test_add_task_with_dependencies() {
        let mut graph = Graph::new();
        
        // Add setup task
        graph.add_node(make_task("setup", "Setup"));
        graph.add_node(make_task("main-task", "Main Task"));
        
        let new_task = NewTask {
            id: "intermediate".to_string(),
            title: "Intermediate Step".to_string(),
            description: "Depends on setup".to_string(),
            depends_on: vec!["setup".to_string()],
            metadata: std::collections::HashMap::new(),
        };

        add_task_to_graph(&mut graph, &new_task, "main-task");

        // Check edges:
        // intermediate depends on setup
        // main-task depends on intermediate
        let edge1: Vec<_> = graph.edges.iter()
            .filter(|e| e.from == "intermediate" && e.to == "setup")
            .collect();
        assert_eq!(edge1.len(), 1);

        let edge2: Vec<_> = graph.edges.iter()
            .filter(|e| e.from == "main-task" && e.to == "intermediate")
            .collect();
        assert_eq!(edge2.len(), 1);
    }
}
