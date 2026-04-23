//! WorkUnit — explicit, typed specification of what a ritual targets.
//!
//! Replaces the old `target_root: PathBuf` parameter which relied on implicit
//! working-directory inheritance and was the root cause of ISS-027.
//!
//! A `WorkUnit` identifies *what* should be worked on (by project name + ID),
//! not *where* it lives. The concrete filesystem path is resolved at ritual
//! start time via the ProjectRegistry (ISS-028).
//!
//! Resolves: ISS-020 (project path discovery), ISS-027 (ritual workspace guard).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::project_registry::Registry as ProjectRegistry;

/// What a ritual targets. Serializable so it survives state-file round-trips.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkUnit {
    /// Work on a tracked issue (ISS-NNN) inside a registered project.
    Issue { project: String, id: String },
    /// Work on a named feature within a registered project.
    Feature { project: String, name: String },
    /// Work on a specific task node (T-NNN) within a project's graph.
    Task { project: String, task_id: String },
}

impl WorkUnit {
    /// Project identifier (name or alias) this work unit belongs to.
    pub fn project(&self) -> &str {
        match self {
            WorkUnit::Issue { project, .. }
            | WorkUnit::Feature { project, .. }
            | WorkUnit::Task { project, .. } => project,
        }
    }

    /// Short human label for logs/telemetry — e.g. "engram/ISS-022".
    pub fn label(&self) -> String {
        match self {
            WorkUnit::Issue { project, id } => format!("{}/{}", project, id),
            WorkUnit::Feature { project, name } => format!("{}/feature:{}", project, name),
            WorkUnit::Task { project, task_id } => format!("{}/task:{}", project, task_id),
        }
    }
}

/// Trait so the executor can accept a stub resolver in tests (ISS-029 dep note:
/// "hardcoded-map stub resolver is acceptable so ISS-029 can progress in parallel").
pub trait WorkUnitResolver: Send + Sync {
    fn resolve(&self, unit: &WorkUnit) -> Result<PathBuf>;
}

/// Production resolver — delegates to the user's registry at
/// `$XDG_CONFIG_HOME/gid/projects.yml`.
pub struct RegistryResolver {
    registry: ProjectRegistry,
}

impl RegistryResolver {
    pub fn load_default() -> Result<Self> {
        Ok(Self {
            registry: ProjectRegistry::load_default()
                .context("loading project registry (ISS-028)")?,
        })
    }

    pub fn from_registry(registry: ProjectRegistry) -> Self {
        Self { registry }
    }
}

impl WorkUnitResolver for RegistryResolver {
    fn resolve(&self, unit: &WorkUnit) -> Result<PathBuf> {
        let entry = self.registry.resolve(unit.project()).with_context(|| {
            format!(
                "resolving project '{}' for work unit {}",
                unit.project(),
                unit.label()
            )
        })?;
        Ok(entry.path.clone())
    }
}

/// Startup validation per ISS-029 §3:
/// - resolved path exists and contains `.gid/`
/// - no `DEPRECATED_DO_NOT_RITUAL` sentinel
///
/// Git tree-state check is intentionally separate (see ISS-027 design): it
/// lives in the executor pre-phase hook so `--force` can override it, while
/// these checks are hard gates that block ritual start unconditionally.
pub fn validate_resolved_root(root: &Path) -> Result<()> {
    if !root.exists() {
        bail!(
            "resolved project root does not exist: {}",
            root.display()
        );
    }
    if !root.is_dir() {
        bail!(
            "resolved project root is not a directory: {}",
            root.display()
        );
    }
    let gid_dir = root.join(".gid");
    if !gid_dir.is_dir() {
        bail!(
            "resolved project root has no .gid/ directory: {} (not a GID-managed project)",
            root.display()
        );
    }
    let sentinel = root.join("DEPRECATED_DO_NOT_RITUAL");
    if sentinel.exists() {
        bail!(
            "project is marked DEPRECATED_DO_NOT_RITUAL — refusing to start ritual: {}",
            root.display()
        );
    }
    Ok(())
}

/// Top-level entry: resolve a work unit to a validated path.
pub fn resolve_and_validate(
    resolver: &dyn WorkUnitResolver,
    unit: &WorkUnit,
) -> Result<PathBuf> {
    let root = resolver.resolve(unit)?;
    validate_resolved_root(&root).with_context(|| {
        format!("validating path for work unit {}", unit.label())
    })?;
    Ok(root)
}

/// Explicit rejection helper — callers that previously passed `target_root`
/// should get a clear error pointing them to `work_unit`.
///
/// Used by any legacy shim/adapter layer during the migration window.
pub fn reject_target_root() -> anyhow::Error {
    anyhow!(
        "target_root is no longer supported. Pass work_unit instead. \
         See ISS-029 for migration: \
         start_ritual(WorkUnit::Issue {{ project, id }}) \
         or WorkUnit::Feature / WorkUnit::Task."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Stub resolver: hardcoded map, no registry load.
    /// Per ISS-029: "a hardcoded-map stub resolver is acceptable".
    struct StubResolver {
        map: HashMap<String, PathBuf>,
    }

    impl WorkUnitResolver for StubResolver {
        fn resolve(&self, unit: &WorkUnit) -> Result<PathBuf> {
            self.map
                .get(unit.project())
                .cloned()
                .ok_or_else(|| anyhow!("stub: unknown project '{}'", unit.project()))
        }
    }

    fn mk_project(dir: &Path) {
        std::fs::create_dir_all(dir.join(".gid")).unwrap();
    }

    #[test]
    fn work_unit_label_format() {
        assert_eq!(
            WorkUnit::Issue { project: "engram".into(), id: "ISS-022".into() }.label(),
            "engram/ISS-022"
        );
        assert_eq!(
            WorkUnit::Feature { project: "rustclaw".into(), name: "voice".into() }.label(),
            "rustclaw/feature:voice"
        );
        assert_eq!(
            WorkUnit::Task { project: "gid-rs".into(), task_id: "T-042".into() }.label(),
            "gid-rs/task:T-042"
        );
    }

    #[test]
    fn resolve_issue_maps_to_project_path() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("engram");
        mk_project(&proj);

        let mut map = HashMap::new();
        map.insert("engram".to_string(), proj.clone());
        let resolver = StubResolver { map };

        let unit = WorkUnit::Issue { project: "engram".into(), id: "ISS-022".into() };
        let got = resolve_and_validate(&resolver, &unit).unwrap();
        assert_eq!(got, proj);
    }

    #[test]
    fn resolve_nonexistent_project_errors_cleanly() {
        let resolver = StubResolver { map: HashMap::new() };
        let unit = WorkUnit::Issue { project: "ghost".into(), id: "ISS-1".into() };

        let err = resolve_and_validate(&resolver, &unit).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("ghost"), "error should name the project: {}", msg);
    }

    #[test]
    fn validate_fails_without_gid_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("no-gid");
        std::fs::create_dir_all(&proj).unwrap(); // no .gid/

        let err = validate_resolved_root(&proj).unwrap_err();
        assert!(format!("{:#}", err).contains(".gid/"));
    }

    #[test]
    fn validate_fails_on_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("deprecated");
        mk_project(&proj);
        std::fs::write(proj.join("DEPRECATED_DO_NOT_RITUAL"), "").unwrap();

        let err = validate_resolved_root(&proj).unwrap_err();
        assert!(format!("{:#}", err).contains("DEPRECATED_DO_NOT_RITUAL"));
    }

    #[test]
    fn validate_fails_when_path_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("ghost");
        // intentionally do not create

        let err = validate_resolved_root(&proj).unwrap_err();
        assert!(format!("{:#}", err).contains("does not exist"));
    }

    #[test]
    fn reject_target_root_error_message() {
        let err = reject_target_root();
        let msg = format!("{}", err);
        assert!(msg.contains("target_root"));
        assert!(msg.contains("work_unit"));
    }

    #[test]
    fn work_unit_serde_roundtrip() {
        let u = WorkUnit::Issue { project: "engram".into(), id: "ISS-022".into() };
        let json = serde_json::to_string(&u).unwrap();
        assert!(json.contains(r#""kind":"issue""#));
        let back: WorkUnit = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);

        let f = WorkUnit::Feature { project: "rc".into(), name: "voice".into() };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""kind":"feature""#));
        let back: WorkUnit = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }
}
