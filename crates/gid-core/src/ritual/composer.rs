//! Dynamic ritual composer — assembles ritual phases based on project state.
//!
//! Instead of picking a pre-defined template, the composer inspects the project
//! directory (existing files, graph, DESIGN.md, test infrastructure, etc.) and
//! builds a tailored sequence of phases.

use std::path::{Path, PathBuf};
use tracing::info;

use super::definition::{
    ApprovalRequirement, PhaseDefinition, PhaseKind, RitualConfig, RitualDefinition,
};

/// Project state detected by scanning the filesystem.
#[derive(Debug)]
pub struct ProjectState {
    /// Project root directory.
    pub root: PathBuf,
    /// .gid/ directory exists.
    pub has_gid_dir: bool,
    /// .gid/graph.yml exists and is non-empty.
    pub has_graph: bool,
    /// Requirements file exists (.gid/requirements-*.md or REQUIREMENTS.md).
    pub has_requirements: bool,
    /// DESIGN.md exists in project root.
    pub has_design: bool,
    /// Source code exists (src/, lib/, etc.).
    pub has_source_code: bool,
    /// Test infrastructure exists (tests/, *_test.*, Cargo.toml with [dev-dependencies]).
    pub has_tests: bool,
    /// Primary language detected.
    pub language: Option<ProjectLanguage>,
    /// Number of source files.
    pub source_file_count: usize,
}

/// Detected project language.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProjectLanguage {
    Rust,
    TypeScript,
    Python,
    Go,
    Other(String),
}

impl ProjectState {
    /// Scan the project directory and detect state.
    pub fn detect(project_root: &Path) -> Self {
        let gid_dir = project_root.join(".gid");
        let graph_path = gid_dir.join("graph.yml");
        let design_path = project_root.join("DESIGN.md");

        let has_graph = graph_path.exists()
            && std::fs::metadata(&graph_path)
                .map(|m| m.len() > 10)
                .unwrap_or(false);

        // Check for requirements files: .gid/requirements-*.md or REQUIREMENTS.md
        let has_requirements = project_root.join("REQUIREMENTS.md").exists()
            || gid_dir.is_dir() && std::fs::read_dir(&gid_dir)
                .map(|entries| entries
                    .filter_map(|e| e.ok())
                    .any(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        name.starts_with("requirements-") && name.ends_with(".md")
                    }))
                .unwrap_or(false);

        let has_source_code = project_root.join("src").is_dir()
            || project_root.join("lib").is_dir()
            || project_root.join("crates").is_dir();

        let has_tests = project_root.join("tests").is_dir()
            || has_file_matching(project_root, "_test.rs")
            || has_file_matching(project_root, "_test.ts")
            || has_file_matching(project_root, "_test.py");

        let language = detect_language(project_root);
        let source_file_count = count_source_files(project_root);

        Self {
            root: project_root.to_path_buf(),
            has_gid_dir: gid_dir.is_dir(),
            has_graph,
            has_requirements,
            has_design: design_path.exists(),
            has_source_code,
            has_tests,
            language,
            source_file_count,
        }
    }
}

/// Compose a ritual definition dynamically based on project state and task description.
pub fn compose_ritual(project_root: &Path, task: &str) -> RitualDefinition {
    let state = ProjectState::detect(project_root);

    info!(
        has_design = state.has_design,
        has_graph = state.has_graph,
        has_source = state.has_source_code,
        has_tests = state.has_tests,
        language = ?state.language,
        source_files = state.source_file_count,
        "Composing ritual from project state"
    );

    let mut phases = Vec::new();

    // ── Phase 1: Design ──────────────────────────────────────────────────
    if !state.has_design {
        // No DESIGN.md → generate one
        phases.push(PhaseDefinition {
            id: "draft-design".to_string(),
            kind: PhaseKind::Skill {
                name: "draft-design".to_string(),
            },
            model: Some("sonnet".to_string()),
            approval: ApprovalRequirement::Auto,
            on_failure: super::definition::FailureStrategy::Escalate,
            ..Default::default()
        });
    } else {
        // DESIGN.md exists → update it with the new task
        phases.push(PhaseDefinition {
            id: "update-design".to_string(),
            kind: PhaseKind::Skill {
                name: "update-design".to_string(),
            },
            model: Some("sonnet".to_string()),
            approval: ApprovalRequirement::Auto,
            on_failure: super::definition::FailureStrategy::Escalate,
            ..Default::default()
        });
    }

    // ── Phase 2: Graph ───────────────────────────────────────────────────
    if !state.has_graph {
        // No graph → generate from scratch
        phases.push(PhaseDefinition {
            id: "generate-graph".to_string(),
            kind: PhaseKind::Skill {
                name: "generate-graph".to_string(),
            },
            model: Some("sonnet".to_string()),
            approval: ApprovalRequirement::Auto,
            on_failure: super::definition::FailureStrategy::Escalate,
            ..Default::default()
        });
    } else {
        // Graph exists → incremental update
        phases.push(PhaseDefinition {
            id: "update-graph".to_string(),
            kind: PhaseKind::Skill {
                name: "update-graph".to_string(),
            },
            model: Some("sonnet".to_string()),
            approval: ApprovalRequirement::Auto,
            on_failure: super::definition::FailureStrategy::Escalate,
            ..Default::default()
        });
    }

    // ── Phase 3: Implement ───────────────────────────────────────────────
    // Always use Skill (single LLM session with full context) for existing codebases.
    // Harness (multi-agent worktrees) only for greenfield with many independent tasks.
    if state.has_source_code {
        phases.push(PhaseDefinition {
            id: "implement".to_string(),
            kind: PhaseKind::Skill {
                name: "implement".to_string(),
            },
            model: Some("opus".to_string()),
            approval: ApprovalRequirement::Auto,
            on_failure: super::definition::FailureStrategy::Escalate,
            ..Default::default()
        });
    } else {
        // Greenfield — use harness for parallel task execution
        phases.push(PhaseDefinition {
            id: "execute-tasks".to_string(),
            kind: PhaseKind::Harness {
                config_overrides: None,
            },
            model: Some("opus".to_string()),
            approval: ApprovalRequirement::Auto,
            on_failure: super::definition::FailureStrategy::Escalate,
            ..Default::default()
        });
    }

    // ── Phase 4: Verify ──────────────────────────────────────────────────
    let verify_cmd = match &state.language {
        Some(ProjectLanguage::Rust) => "cargo build 2>&1 && cargo test 2>&1".to_string(),
        Some(ProjectLanguage::TypeScript) => "npm run build 2>&1 && npm test 2>&1".to_string(),
        Some(ProjectLanguage::Python) => "python -m pytest 2>&1".to_string(),
        Some(ProjectLanguage::Go) => "go build ./... 2>&1 && go test ./... 2>&1".to_string(),
        _ => "echo 'No test command detected — manual verification needed'".to_string(),
    };

    phases.push(PhaseDefinition {
        id: "verify".to_string(),
        kind: PhaseKind::Shell {
            command: verify_cmd,
        },
        approval: ApprovalRequirement::Auto,
        on_failure: super::definition::FailureStrategy::Escalate,
        ..Default::default()
    });

    let ritual_name = format!("auto-{}", sanitize_name(task));

    info!(
        name = %ritual_name,
        phase_count = phases.len(),
        phase_ids = ?phases.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        "Ritual composed"
    );

    RitualDefinition {
        name: ritual_name,
        description: Some(format!("Auto-composed ritual for: {}", task)),
        extends: None,
        phases,
        config: RitualConfig {
            default_model: "opus".to_string(),
            ..Default::default()
        },
        task_context: Some(task.to_string()),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn detect_language(root: &Path) -> Option<ProjectLanguage> {
    if root.join("Cargo.toml").exists() {
        Some(ProjectLanguage::Rust)
    } else if root.join("package.json").exists() {
        Some(ProjectLanguage::TypeScript)
    } else if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
        Some(ProjectLanguage::Python)
    } else if root.join("go.mod").exists() {
        Some(ProjectLanguage::Go)
    } else {
        None
    }
}

fn count_source_files(root: &Path) -> usize {
    let src_dir = root.join("src");
    if !src_dir.is_dir() {
        return 0;
    }
    walkdir_count(&src_dir)
}

fn walkdir_count(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name != "target" && name != "node_modules" && name != ".git" {
                        count += walkdir_count(&path);
                    }
                }
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "rs" | "ts" | "tsx" | "js" | "py" | "go") {
                    count += 1;
                }
            }
        }
    }
    count
}

fn has_file_matching(root: &Path, suffix: &str) -> bool {
    let src = root.join("src");
    if !src.is_dir() {
        return false;
    }
    has_file_matching_recursive(&src, suffix)
}

fn has_file_matching_recursive(dir: &Path, suffix: &str) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if has_file_matching_recursive(&path, suffix) {
                    return true;
                }
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(suffix) {
                    return true;
                }
            }
        }
    }
    false
}

fn sanitize_name(task: &str) -> String {
    task.chars()
        .take(40)
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_compose_greenfield() {
        let tmp = TempDir::new().unwrap();
        let ritual = compose_ritual(tmp.path(), "build a CLI tool");

        // No DESIGN.md → draft-design
        assert_eq!(ritual.phases[0].id, "draft-design");
        // No graph → generate-graph
        assert_eq!(ritual.phases[1].id, "generate-graph");
        // No source → harness (greenfield)
        assert_eq!(ritual.phases[2].id, "execute-tasks");
        // Verify
        assert_eq!(ritual.phases[3].id, "verify");
    }

    #[test]
    fn test_compose_existing_rust_project() {
        let tmp = TempDir::new().unwrap();
        // Simulate existing Rust project
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(tmp.path().join(".gid")).unwrap();
        fs::write(tmp.path().join(".gid/graph.yml"), "nodes:\n  - id: test\n    title: test\n    status: done\n    type: component").unwrap();
        fs::write(tmp.path().join("DESIGN.md"), "# Design\nSome design").unwrap();

        let ritual = compose_ritual(tmp.path(), "add /tools command");

        // DESIGN.md exists → update-design
        assert_eq!(ritual.phases[0].id, "update-design");
        // Graph exists → update-graph
        assert_eq!(ritual.phases[1].id, "update-graph");
        // Has source → implement (Skill, not Harness)
        assert_eq!(ritual.phases[2].id, "implement");
        // Rust → cargo build && cargo test
        assert_eq!(ritual.phases[3].id, "verify");
        if let PhaseKind::Shell { command } = &ritual.phases[3].kind {
            assert!(command.contains("cargo"));
        } else {
            panic!("verify should be Shell phase");
        }
    }

    #[test]
    fn test_compose_no_design_with_graph() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        fs::create_dir_all(tmp.path().join(".gid")).unwrap();
        fs::write(tmp.path().join(".gid/graph.yml"), "nodes:\n  - id: x\n    title: x\n    status: done\n    type: file").unwrap();

        let ritual = compose_ritual(tmp.path(), "fix a bug");

        // No DESIGN → draft-design
        assert_eq!(ritual.phases[0].id, "draft-design");
        // Graph exists → update-graph
        assert_eq!(ritual.phases[1].id, "update-graph");
        // Has source → implement
        assert_eq!(ritual.phases[2].id, "implement");
    }

    #[test]
    fn test_project_state_detect() {
        let tmp = TempDir::new().unwrap();
        let state = ProjectState::detect(tmp.path());
        assert!(!state.has_gid_dir);
        assert!(!state.has_graph);
        assert!(!state.has_design);
        assert!(!state.has_source_code);
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("add /tools command"), "add--tools-command");
        assert_eq!(sanitize_name("fix bug #123"), "fix-bug--123");
    }
}
