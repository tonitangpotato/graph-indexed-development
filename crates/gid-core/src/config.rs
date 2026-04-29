//! Project-level config loaded from `.gid/config.yml`.
//!
//! ISS-059 introduces a `drift:` section. Existing top-level fields
//! (currently just `verify_command`) are preserved unchanged so older
//! configs keep working.
//!
//! # Schema
//!
//! ```yaml
//! verify_command: "cargo test -p gid-core"   # legacy top-level field
//!
//! drift:
//!   enabled: true                  # default true if `drift:` block present
//!   severity_filter: warn          # min severity surfaced: error|warn|info
//!   ignore:
//!     - "ISS-901"                  # artifact ids excluded from checks
//!     - "ISS-902"
//!   checks:
//!     a1_orphan_artifacts: true    # artifact-on-disk, no graph node
//!     a2_orphan_nodes: true        # graph node, no artifact-on-disk
//!     a3_status_mismatch: true     # frontmatter status != node status
//! ```
//!
//! All fields are optional — every unset key falls back to the `Default`
//! shown in [`DriftConfig::default`]. The loader is intentionally lenient:
//! parse errors degrade to a default config + a warning, never a hard error,
//! because `gid validate` must remain runnable on a half-broken project
//! (drift detection itself is meant to *find* such half-broken state).

use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{debug, warn};

/// Top-level `.gid/config.yml` schema, minimal subset relevant outside
/// the ritual feature.
///
/// The ritual gating layer has its own `GidConfig` in
/// `crate::ritual::gating` that covers the `ritual:` subsection. That
/// type and this one both deserialize from the same YAML file but
/// look at disjoint keys. We could unify them later — for now keeping
/// them separate avoids forcing a dependency on the `ritual` feature
/// for plain `gid validate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Legacy: command run by ritual `verify` phase. Kept for read
    /// compatibility; not consumed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,

    /// Drift-detection settings (ISS-059).
    #[serde(default)]
    pub drift: DriftConfig,
}

/// Drift-detection knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftConfig {
    /// Master switch. When false, `--check-drift` runs but reports
    /// nothing and exits 0. Default: true.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Minimum severity printed and considered for non-zero exit.
    /// `Info` shows everything; `Error` shows only the fatal items.
    /// Default: `Warn`.
    #[serde(default)]
    pub severity_filter: SeverityFilter,

    /// Artifact ids excluded from every check (e.g., known historical
    /// stubs that nobody intends to clean up). Bare ids only — no
    /// project prefix. Default: empty.
    #[serde(default)]
    pub ignore: Vec<String>,

    /// Per-check enable flags. Lets users silence one noisy check
    /// without disabling drift entirely.
    #[serde(default)]
    pub checks: CheckToggles,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            severity_filter: SeverityFilter::default(),
            ignore: Vec::new(),
            checks: CheckToggles::default(),
        }
    }
}

/// Per-check enable flags. New checks added in later phases get appended
/// here with `#[serde(default = "default_true")]` so unknown configs
/// continue to opt them in by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckToggles {
    /// A.1: artifact file exists on disk but no matching graph node.
    #[serde(default = "default_true")]
    pub a1_orphan_artifacts: bool,
    /// A.2: graph node references an artifact path that is missing on disk.
    #[serde(default = "default_true")]
    pub a2_orphan_nodes: bool,
    /// A.3: artifact frontmatter `status:` differs from the node's status.
    #[serde(default = "default_true")]
    pub a3_status_mismatch: bool,
}

impl Default for CheckToggles {
    fn default() -> Self {
        Self {
            a1_orphan_artifacts: true,
            a2_orphan_nodes: true,
            a3_status_mismatch: true,
        }
    }
}

/// Severity levels for drift findings. Ordering matters:
/// `Error > Warn > Info`. The `Ord` derive uses declaration order.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum SeverityFilter {
    Info,
    Warn,
    Error,
}

impl Default for SeverityFilter {
    fn default() -> Self {
        SeverityFilter::Warn
    }
}

fn default_true() -> bool {
    true
}

/// Load `ProjectConfig` from `<project_root>/.gid/config.yml`.
///
/// Returns a default config in any of these cases:
/// - file missing (treated as fresh project)
/// - file unreadable (logged at WARN)
/// - YAML parse error (logged at WARN; never propagates)
///
/// This *intentionally* never returns `Err`. A half-broken config must
/// not block `gid validate` — that command is precisely the tool you'd
/// reach for when things are broken.
pub fn load_project_config(project_root: &Path) -> ProjectConfig {
    let config_path = project_root.join(".gid").join("config.yml");
    if !config_path.exists() {
        debug!(path = %config_path.display(), "no .gid/config.yml — using default ProjectConfig");
        return ProjectConfig::default();
    }
    match std::fs::read_to_string(&config_path) {
        Ok(content) => match serde_yaml::from_str::<ProjectConfig>(&content) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!(path = %config_path.display(), error = %e, "failed to parse .gid/config.yml — using defaults");
                ProjectConfig::default()
            }
        },
        Err(e) => {
            warn!(path = %config_path.display(), error = %e, "failed to read .gid/config.yml — using defaults");
            ProjectConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_cfg(dir: &TempDir, body: &str) {
        let gid = dir.path().join(".gid");
        std::fs::create_dir_all(&gid).unwrap();
        std::fs::write(gid.join("config.yml"), body).unwrap();
    }

    #[test]
    fn missing_file_returns_default() {
        let tmp = TempDir::new().unwrap();
        let cfg = load_project_config(tmp.path());
        assert!(cfg.drift.enabled);
        assert!(cfg.drift.checks.a1_orphan_artifacts);
        assert_eq!(cfg.drift.severity_filter, SeverityFilter::Warn);
        assert!(cfg.verify_command.is_none());
    }

    #[test]
    fn legacy_verify_command_only() {
        // Pre-ISS-059 config files have only `verify_command:` — must keep working.
        let tmp = TempDir::new().unwrap();
        write_cfg(&tmp, "verify_command: \"cargo test\"\n");
        let cfg = load_project_config(tmp.path());
        assert_eq!(cfg.verify_command.as_deref(), Some("cargo test"));
        // Drift defaults applied.
        assert!(cfg.drift.enabled);
        assert!(cfg.drift.ignore.is_empty());
    }

    #[test]
    fn full_drift_block_parses() {
        let tmp = TempDir::new().unwrap();
        write_cfg(
            &tmp,
            r#"
drift:
  enabled: false
  severity_filter: error
  ignore:
    - ISS-901
    - ISS-902
  checks:
    a1_orphan_artifacts: false
    a2_orphan_nodes: true
    a3_status_mismatch: false
"#,
        );
        let cfg = load_project_config(tmp.path());
        assert!(!cfg.drift.enabled);
        assert_eq!(cfg.drift.severity_filter, SeverityFilter::Error);
        assert_eq!(cfg.drift.ignore, vec!["ISS-901", "ISS-902"]);
        assert!(!cfg.drift.checks.a1_orphan_artifacts);
        assert!(cfg.drift.checks.a2_orphan_nodes);
        assert!(!cfg.drift.checks.a3_status_mismatch);
    }

    #[test]
    fn unknown_keys_ignored() {
        let tmp = TempDir::new().unwrap();
        write_cfg(
            &tmp,
            r#"
verify_command: "cargo build"
unknown_top_level: 42
drift:
  enabled: true
  future_field: "from a later version"
"#,
        );
        let cfg = load_project_config(tmp.path());
        assert!(cfg.drift.enabled);
        assert_eq!(cfg.verify_command.as_deref(), Some("cargo build"));
    }

    #[test]
    fn malformed_yaml_yields_default_not_error() {
        let tmp = TempDir::new().unwrap();
        write_cfg(&tmp, "this is :: not :: valid : yaml ::: at all\n  foo: [unclosed\n");
        let cfg = load_project_config(tmp.path());
        // No panic, no error — drift defaults present.
        assert!(cfg.drift.enabled);
    }

    #[test]
    fn severity_filter_ordering() {
        assert!(SeverityFilter::Error > SeverityFilter::Warn);
        assert!(SeverityFilter::Warn > SeverityFilter::Info);
    }
}
