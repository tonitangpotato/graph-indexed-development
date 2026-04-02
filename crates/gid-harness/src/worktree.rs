//! Git worktree manager — create, merge, and clean up isolated worktrees.
//!
//! Each task gets its own git worktree branched from `main`, providing
//! isolation for parallel sub-agent execution. Merges are serialized
//! via a mutex to prevent concurrent merge conflicts (GUARD-11).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::Mutex;
use tracing::{info, debug};

use gid_core::harness::types::WorktreeInfo;

/// Trait for managing git worktrees.
///
/// Worktrees provide isolation for parallel task execution:
/// each sub-agent works in its own branch without affecting others.
#[async_trait::async_trait]
pub trait WorktreeManager: Send + Sync {
    /// Create a worktree branched from latest main.
    /// Returns the path to the worktree directory.
    async fn create(&self, task_id: &str) -> Result<PathBuf>;

    /// Rebase worktree on latest main, then merge with `--no-ff`.
    /// Merges are serialized — only one merge at a time (GUARD-11).
    async fn merge(&self, task_id: &str) -> Result<()>;

    /// Remove worktree and delete its branch.
    async fn cleanup(&self, task_id: &str) -> Result<()>;

    /// List surviving worktrees (for crash recovery).
    async fn list_existing(&self) -> Result<Vec<WorktreeInfo>>;
}

/// Git-based worktree manager.
///
/// Creates worktrees in a temp directory, branched from main.
/// Branch naming: `gid/task-{task_id}`.
/// Worktree path: `{worktree_base}/gid-wt-{task_id}`.
#[derive(Debug)]
pub struct GitWorktreeManager {
    /// Path to the main git repository.
    repo_path: PathBuf,
    /// Base directory for worktrees (default: system temp).
    worktree_base: PathBuf,
    /// Mutex to serialize merge operations (GUARD-11).
    merge_lock: Arc<Mutex<()>>,
    /// Main branch name (default: "main").
    main_branch: String,
}

impl GitWorktreeManager {
    /// Create a new worktree manager for the given repository.
    pub fn new(repo_path: impl Into<PathBuf>) -> Self {
        let repo = repo_path.into();
        let worktree_base = std::env::temp_dir();
        Self {
            repo_path: repo,
            worktree_base,
            merge_lock: Arc::new(Mutex::new(())),
            main_branch: "main".to_string(),
        }
    }

    /// Set a custom worktree base directory.
    pub fn with_worktree_base(mut self, base: impl Into<PathBuf>) -> Self {
        self.worktree_base = base.into();
        self
    }

    /// Set the main branch name (default: "main").
    pub fn with_main_branch(mut self, branch: impl Into<String>) -> Self {
        self.main_branch = branch.into();
        self
    }

    /// Get the branch name for a task.
    fn branch_name(task_id: &str) -> String {
        format!("gid/task-{}", task_id)
    }

    /// Get the worktree path for a task.
    fn worktree_path(&self, task_id: &str) -> PathBuf {
        self.worktree_base.join(format!("gid-wt-{}", task_id))
    }

    /// Run a git command in the repo directory.
    async fn git(&self, args: &[&str]) -> Result<String> {
        self.git_in(&self.repo_path, args).await
    }

    /// Run a git command in a specific directory.
    async fn git_in(&self, dir: &Path, args: &[&str]) -> Result<String> {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .context("Failed to execute git")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            bail!("git {} failed (exit {}): {}", args.join(" "), output.status, stderr.trim());
        }

        debug!(cmd = %args.join(" "), "git command succeeded");
        Ok(stdout.trim().to_string())
    }
}

#[async_trait::async_trait]
impl WorktreeManager for GitWorktreeManager {
    async fn create(&self, task_id: &str) -> Result<PathBuf> {
        let branch = Self::branch_name(task_id);
        let wt_path = self.worktree_path(task_id);

        info!(task_id, branch = %branch, path = %wt_path.display(), "Creating worktree");

        // Ensure we're on latest main
        self.git(&["fetch", "origin", &self.main_branch]).await.ok(); // non-fatal if no remote

        // Create worktree with new branch from main
        self.git(&[
            "worktree", "add",
            wt_path.to_str().unwrap(),
            "-b", &branch,
            &self.main_branch,
        ]).await.context("Failed to create worktree")?;

        Ok(wt_path)
    }

    async fn merge(&self, task_id: &str) -> Result<()> {
        let branch = Self::branch_name(task_id);
        let wt_path = self.worktree_path(task_id);

        info!(task_id, branch = %branch, "Merging worktree (acquiring lock)");

        // Serialize merges (GUARD-11)
        let _lock = self.merge_lock.lock().await;

        // Rebase on latest main in the worktree
        let rebase_result = self.git_in(&wt_path, &["rebase", &self.main_branch]).await;
        if let Err(e) = rebase_result {
            // Abort rebase on conflict
            self.git_in(&wt_path, &["rebase", "--abort"]).await.ok();
            bail!("Rebase conflict for task {}: {}", task_id, e);
        }

        // Merge into main (--no-ff for clear history)
        self.git(&["checkout", &self.main_branch]).await?;
        self.git(&["merge", "--no-ff", &branch, "-m", &format!("gid: merge task {}", task_id)]).await
            .context(format!("Merge failed for task {}", task_id))?;

        info!(task_id, "Merge successful");
        Ok(())
    }

    async fn cleanup(&self, task_id: &str) -> Result<()> {
        let branch = Self::branch_name(task_id);
        let wt_path = self.worktree_path(task_id);

        info!(task_id, "Cleaning up worktree");

        // Remove worktree
        self.git(&["worktree", "remove", "--force", wt_path.to_str().unwrap()]).await.ok();

        // Delete branch
        self.git(&["branch", "-D", &branch]).await.ok();

        // Clean up directory if it still exists
        if wt_path.exists() {
            tokio::fs::remove_dir_all(&wt_path).await.ok();
        }

        Ok(())
    }

    async fn list_existing(&self) -> Result<Vec<WorktreeInfo>> {
        let output = self.git(&["worktree", "list", "--porcelain"]).await?;
        let mut worktrees = Vec::new();

        let mut current_path = None;
        let mut current_branch = None;

        for line in output.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                current_path = Some(PathBuf::from(path));
            } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                current_branch = Some(branch.to_string());
            } else if line.is_empty() {
                // End of entry
                if let (Some(path), Some(branch)) = (current_path.take(), current_branch.take()) {
                    if let Some(task_id) = branch.strip_prefix("gid/task-") {
                        worktrees.push(WorktreeInfo {
                            task_id: task_id.to_string(),
                            path,
                            branch,
                        });
                    }
                }
                current_path = None;
                current_branch = None;
            }
        }

        // Handle last entry if no trailing newline
        if let (Some(path), Some(branch)) = (current_path, current_branch) {
            if let Some(task_id) = branch.strip_prefix("gid/task-") {
                worktrees.push(WorktreeInfo {
                    task_id: task_id.to_string(),
                    path,
                    branch,
                });
            }
        }

        Ok(worktrees)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_branch_name() {
        assert_eq!(GitWorktreeManager::branch_name("auth-impl"), "gid/task-auth-impl");
        assert_eq!(GitWorktreeManager::branch_name("123"), "gid/task-123");
    }

    #[test]
    fn test_worktree_path() {
        let mgr = GitWorktreeManager::new("/repo")
            .with_worktree_base("/tmp/test");
        assert_eq!(mgr.worktree_path("auth"), PathBuf::from("/tmp/test/gid-wt-auth"));
    }

    #[tokio::test]
    async fn test_list_existing_empty_repo() {
        // Create a temp git repo
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        tokio::process::Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();

        // Need at least one commit for worktree list to work
        tokio::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();

        let mgr = GitWorktreeManager::new(repo);
        let existing = mgr.list_existing().await.unwrap();
        assert!(existing.is_empty(), "New repo should have no gid worktrees");
    }

    #[tokio::test]
    async fn test_create_and_cleanup_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let wt_base = tempfile::tempdir().unwrap();

        // Init repo with a commit
        tokio::process::Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(repo)
            .output()
            .await
            .unwrap();

        let mgr = GitWorktreeManager::new(repo)
            .with_worktree_base(wt_base.path());

        // Create worktree
        let wt_path = mgr.create("test-task").await.unwrap();
        assert!(wt_path.exists(), "Worktree directory should exist");

        // Verify it shows up in list
        let existing = mgr.list_existing().await.unwrap();
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].task_id, "test-task");

        // Cleanup
        mgr.cleanup("test-task").await.unwrap();
        assert!(!wt_path.exists(), "Worktree should be removed after cleanup");
    }
}
