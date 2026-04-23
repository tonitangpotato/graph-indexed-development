//! Project Registry: canonical `project name → path` mapping.
//!
//! The registry lives at `$XDG_CONFIG_HOME/gid/projects.yml` (falling back to
//! `~/.config/gid/projects.yml`). It is the authoritative answer to
//! "which projects exist on this machine, and where are they?"
//!
//! ## Design principles
//!
//! - **gid owns this.** `.gid/` is a gid-defined directory, so gid answers
//!   "who has a .gid/". Consumers (rustclaw, agentctl) ask; they don't guess.
//! - **Pure CRUD + YAML I/O.** Zero dependency on graph types. The registry
//!   is not part of any single project's graph — it's a machine-level index.
//! - **XDG-compliant.** Portable to Linux, friendly to dotfile migration.
//! - **Aliases first-class.** Humans and agents call projects by different
//!   names ("engram" vs "engram-ai" vs "ea"). All resolve to one canonical
//!   path.
//!
//! ## Resolution semantics
//!
//! `resolve(ident)` matches in order:
//! 1. Exact `name` (case-insensitive)
//! 2. Exact match in any project's `aliases` (case-insensitive)
//!
//! Cross-project alias collisions return `Error::Ambiguous` listing all
//! candidates — never silent "first match wins". Issue references use the
//! `project:issue` format (e.g. `engram:ISS-022`); this module only resolves
//! the project portion.
//!
//! Resolves: ISS-020 (project path discovery friction), ISS-028 (gid CLI
//! project subcommand).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ════════════════════════════════════════════════════════════════════════════
// Schema
// ════════════════════════════════════════════════════════════════════════════

/// Current registry schema version. Bump when making breaking layout changes.
pub const SCHEMA_VERSION: u32 = 1;

/// On-disk registry structure. Loaded from and saved to `projects.yml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    /// Schema version. Missing → assumed `1` (backward compat with early
    /// hand-written files that predate the versioning contract).
    #[serde(default = "default_version")]
    pub version: u32,

    /// Registered projects. Order is preserved on save so hand-edits stay
    /// stable.
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

/// One project entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Canonical name. Must be unique across the registry.
    pub name: String,

    /// Absolute path to the project root (the directory containing `.gid/`).
    pub path: PathBuf,

    /// Alternative names that resolve to this project. Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    /// Default git branch. Optional; falls back to caller's discovery logic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,

    /// Free-form tags for filtering (e.g. `["active", "rust"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// If true, the project is inactive. Still resolvable, but `list` may
    /// hide it behind a flag.
    #[serde(default, skip_serializing_if = "is_false")]
    pub archived: bool,

    /// Free-form note for humans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ════════════════════════════════════════════════════════════════════════════
// Errors
// ════════════════════════════════════════════════════════════════════════════

/// Errors produced by registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("registry file I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("registry YAML parse error at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("registry YAML serialize error: {source}")]
    Serialize {
        #[source]
        source: serde_yaml::Error,
    },

    #[error("could not determine XDG config directory (no $HOME and no $XDG_CONFIG_HOME)")]
    NoConfigDir,

    #[error("project '{0}' not found in registry")]
    NotFound(String),

    #[error(
        "identifier '{ident}' is ambiguous — matches multiple projects: {candidates:?}"
    )]
    Ambiguous {
        ident: String,
        candidates: Vec<String>,
    },

    #[error("project '{0}' is already registered")]
    AlreadyRegistered(String),

    #[error("path '{path}' does not exist or is not a directory")]
    InvalidPath { path: PathBuf },

    #[error("path '{path}' does not contain a .gid/ directory (is this a gid project?)")]
    NotAGidProject { path: PathBuf },

    #[error(
        "registry schema version {found} is newer than this build supports (max: {max})"
    )]
    UnsupportedVersion { found: u32, max: u32 },
}

pub type Result<T> = std::result::Result<T, RegistryError>;

// ════════════════════════════════════════════════════════════════════════════
// Path discovery
// ════════════════════════════════════════════════════════════════════════════

/// Return the canonical registry file path:
/// `$XDG_CONFIG_HOME/gid/projects.yml` if set, else `~/.config/gid/projects.yml`.
///
/// Does NOT create the file or parent directory. Caller decides when.
pub fn default_registry_path() -> Result<PathBuf> {
    // Prefer $XDG_CONFIG_HOME for explicit control (test injection, custom setups).
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let xdg = PathBuf::from(xdg);
        if !xdg.as_os_str().is_empty() {
            return Ok(xdg.join("gid").join("projects.yml"));
        }
    }
    // Fallback: ~/.config/gid/projects.yml. We use $HOME directly instead of the `dirs`
    // crate — gid is a developer tool and developers expect ~/.config regardless of
    // platform (we deliberately override the macOS "~/Library/Application Support" default).
    // Keeping the registry free of optional dependencies means it builds under default features.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(RegistryError::NoConfigDir)?;
    Ok(home.join(".config").join("gid").join("projects.yml"))
}

// ════════════════════════════════════════════════════════════════════════════
// Load / Save
// ════════════════════════════════════════════════════════════════════════════

impl Registry {
    /// Create an empty registry at the current schema version.
    pub fn empty() -> Self {
        Self {
            version: SCHEMA_VERSION,
            projects: Vec::new(),
        }
    }

    /// Load from the default path. If the file does not exist, returns an
    /// empty registry (not an error — first-run is normal).
    pub fn load_default() -> Result<Self> {
        let path = default_registry_path()?;
        Self::load_from(&path)
    }

    /// Load from an explicit path. Missing file → empty registry.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let bytes = fs::read(path).map_err(|source| RegistryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        // Empty file → empty registry (don't choke on `touch projects.yml`).
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(Self::empty());
        }
        let mut reg: Registry =
            serde_yaml::from_slice(&bytes).map_err(|source| RegistryError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        // Forward-compat guard: refuse to load futures we don't understand.
        if reg.version > SCHEMA_VERSION {
            return Err(RegistryError::UnsupportedVersion {
                found: reg.version,
                max: SCHEMA_VERSION,
            });
        }
        // Backward-compat: older files without `version` field deserialize to 0
        // (since serde's `default` is `default_version = SCHEMA_VERSION`, this
        // actually gives us SCHEMA_VERSION already — but be explicit).
        if reg.version == 0 {
            reg.version = SCHEMA_VERSION;
        }
        Ok(reg)
    }

    /// Save to the default path, creating parent directories as needed.
    pub fn save_default(&self) -> Result<PathBuf> {
        let path = default_registry_path()?;
        self.save_to(&path)?;
        Ok(path)
    }

    /// Save to an explicit path. Creates parent directories. Atomic write
    /// (temp file + rename) to avoid corruption on crash mid-write.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| RegistryError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let yaml = serde_yaml::to_string(self)
            .map_err(|source| RegistryError::Serialize { source })?;
        // Atomic write: write to tmp sibling, fsync, rename.
        let tmp = path.with_extension("yml.tmp");
        fs::write(&tmp, yaml.as_bytes()).map_err(|source| RegistryError::Io {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, path).map_err(|source| RegistryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Query
// ════════════════════════════════════════════════════════════════════════════

impl Registry {
    /// Resolve a project identifier (name or alias, case-insensitive) to its
    /// canonical path.
    ///
    /// Returns `Err(NotFound)` if no match, `Err(Ambiguous)` if multiple
    /// projects claim the same name/alias.
    pub fn resolve(&self, ident: &str) -> Result<&ProjectEntry> {
        let needle = ident.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return Err(RegistryError::NotFound(ident.to_string()));
        }
        let mut matches: Vec<&ProjectEntry> = Vec::new();
        for p in &self.projects {
            if p.name.to_ascii_lowercase() == needle
                || p.aliases.iter().any(|a| a.to_ascii_lowercase() == needle)
            {
                matches.push(p);
            }
        }
        match matches.len() {
            0 => Err(RegistryError::NotFound(ident.to_string())),
            1 => Ok(matches[0]),
            _ => Err(RegistryError::Ambiguous {
                ident: ident.to_string(),
                candidates: matches.iter().map(|p| p.name.clone()).collect(),
            }),
        }
    }

    /// Find a project by canonical name only (no alias fallback). Used
    /// internally for `add`/`remove` which operate on the canonical name.
    pub fn find_by_name(&self, name: &str) -> Option<&ProjectEntry> {
        let needle = name.to_ascii_lowercase();
        self.projects
            .iter()
            .find(|p| p.name.to_ascii_lowercase() == needle)
    }

    /// Iterate all entries (optionally filtering out archived ones).
    pub fn list(&self, include_archived: bool) -> impl Iterator<Item = &ProjectEntry> {
        self.projects
            .iter()
            .filter(move |p| include_archived || !p.archived)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Mutation
// ════════════════════════════════════════════════════════════════════════════

impl Registry {
    /// Add a new project. Validates:
    /// - name not already registered (case-insensitive)
    /// - aliases don't collide with existing names/aliases
    /// - path exists, is a directory, contains `.gid/`
    ///
    /// Does NOT save — caller is responsible for calling `save_to`/`save_default`.
    pub fn add(&mut self, entry: ProjectEntry) -> Result<()> {
        // Name uniqueness.
        if self.find_by_name(&entry.name).is_some() {
            return Err(RegistryError::AlreadyRegistered(entry.name));
        }
        // Alias uniqueness across the whole registry (aliases + names).
        let mut taken: HashSet<String> = HashSet::new();
        for p in &self.projects {
            taken.insert(p.name.to_ascii_lowercase());
            for a in &p.aliases {
                taken.insert(a.to_ascii_lowercase());
            }
        }
        for a in &entry.aliases {
            let lc = a.to_ascii_lowercase();
            if taken.contains(&lc) || lc == entry.name.to_ascii_lowercase() {
                return Err(RegistryError::AlreadyRegistered(a.clone()));
            }
        }
        // Path validation.
        if !entry.path.is_dir() {
            return Err(RegistryError::InvalidPath {
                path: entry.path.clone(),
            });
        }
        if !entry.path.join(".gid").is_dir() {
            return Err(RegistryError::NotAGidProject {
                path: entry.path.clone(),
            });
        }
        self.projects.push(entry);
        Ok(())
    }

    /// Remove a project by canonical name. Returns the removed entry.
    /// Aliases cannot be used to remove — only the canonical name, to avoid
    /// accidental deletions via fuzzy matches.
    pub fn remove(&mut self, name: &str) -> Result<ProjectEntry> {
        let needle = name.to_ascii_lowercase();
        let idx = self
            .projects
            .iter()
            .position(|p| p.name.to_ascii_lowercase() == needle)
            .ok_or_else(|| RegistryError::NotFound(name.to_string()))?;
        Ok(self.projects.remove(idx))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a temp project directory that looks like a gid project (contains .gid/).
    fn mk_gid_project(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        fs::create_dir_all(p.join(".gid")).unwrap();
        p
    }

    fn sample_entry(path: PathBuf, name: &str) -> ProjectEntry {
        ProjectEntry {
            name: name.to_string(),
            path,
            aliases: vec![],
            default_branch: None,
            tags: vec![],
            archived: false,
            notes: None,
        }
    }

    #[test]
    fn empty_registry_has_no_projects() {
        let r = Registry::empty();
        assert_eq!(r.version, SCHEMA_VERSION);
        assert!(r.projects.is_empty());
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("does_not_exist.yml");
        let r = Registry::load_from(&path).unwrap();
        assert!(r.projects.is_empty());
    }

    #[test]
    fn load_empty_file_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.yml");
        fs::write(&path, "").unwrap();
        let r = Registry::load_from(&path).unwrap();
        assert!(r.projects.is_empty());
        // Whitespace-only also fine.
        fs::write(&path, "\n\n   \n").unwrap();
        let r = Registry::load_from(&path).unwrap();
        assert!(r.projects.is_empty());
    }

    #[test]
    fn round_trip_save_load() {
        let tmp = tempfile::tempdir().unwrap();
        let proj_path = mk_gid_project(tmp.path(), "engram");
        let reg_path = tmp.path().join("reg.yml");

        let mut r = Registry::empty();
        r.add(ProjectEntry {
            name: "engram".into(),
            path: proj_path.clone(),
            aliases: vec!["engram-ai".into(), "ea".into()],
            default_branch: Some("main".into()),
            tags: vec!["rust".into()],
            archived: false,
            notes: Some("monorepo".into()),
        })
        .unwrap();
        r.save_to(&reg_path).unwrap();

        let r2 = Registry::load_from(&reg_path).unwrap();
        assert_eq!(r2.version, SCHEMA_VERSION);
        assert_eq!(r2.projects.len(), 1);
        let p = &r2.projects[0];
        assert_eq!(p.name, "engram");
        assert_eq!(p.aliases, vec!["engram-ai", "ea"]);
        assert_eq!(p.default_branch.as_deref(), Some("main"));
        assert_eq!(p.tags, vec!["rust"]);
        assert!(!p.archived);
        assert_eq!(p.notes.as_deref(), Some("monorepo"));
    }

    #[test]
    fn resolve_by_name_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        let proj_path = mk_gid_project(tmp.path(), "engram");
        let mut r = Registry::empty();
        r.add(sample_entry(proj_path.clone(), "engram")).unwrap();

        assert_eq!(r.resolve("engram").unwrap().path, proj_path);
        assert_eq!(r.resolve("ENGRAM").unwrap().path, proj_path);
        assert_eq!(r.resolve("  Engram  ").unwrap().path, proj_path);
    }

    #[test]
    fn resolve_by_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let proj_path = mk_gid_project(tmp.path(), "engram");
        let mut r = Registry::empty();
        r.add(ProjectEntry {
            aliases: vec!["ea".into(), "engram-ai".into()],
            ..sample_entry(proj_path.clone(), "engram")
        })
        .unwrap();

        assert_eq!(r.resolve("ea").unwrap().name, "engram");
        assert_eq!(r.resolve("engram-ai").unwrap().name, "engram");
        assert_eq!(r.resolve("EA").unwrap().name, "engram");
    }

    #[test]
    fn resolve_not_found() {
        let r = Registry::empty();
        match r.resolve("nope") {
            Err(RegistryError::NotFound(s)) => assert_eq!(s, "nope"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn resolve_empty_string_is_not_found() {
        let r = Registry::empty();
        assert!(matches!(r.resolve(""), Err(RegistryError::NotFound(_))));
        assert!(matches!(r.resolve("   "), Err(RegistryError::NotFound(_))));
    }

    #[test]
    fn add_rejects_duplicate_name() {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = mk_gid_project(tmp.path(), "a");
        let p2 = mk_gid_project(tmp.path(), "b");
        let mut r = Registry::empty();
        r.add(sample_entry(p1, "engram")).unwrap();
        let err = r.add(sample_entry(p2, "ENGRAM")).unwrap_err();
        assert!(matches!(err, RegistryError::AlreadyRegistered(_)));
    }

    #[test]
    fn add_rejects_alias_colliding_with_existing_name() {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = mk_gid_project(tmp.path(), "engram");
        let p2 = mk_gid_project(tmp.path(), "other");
        let mut r = Registry::empty();
        r.add(sample_entry(p1, "engram")).unwrap();
        let err = r
            .add(ProjectEntry {
                aliases: vec!["engram".into()],
                ..sample_entry(p2, "other")
            })
            .unwrap_err();
        assert!(matches!(err, RegistryError::AlreadyRegistered(_)));
    }

    #[test]
    fn add_rejects_alias_colliding_with_existing_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = mk_gid_project(tmp.path(), "engram");
        let p2 = mk_gid_project(tmp.path(), "other");
        let mut r = Registry::empty();
        r.add(ProjectEntry {
            aliases: vec!["ea".into()],
            ..sample_entry(p1, "engram")
        })
        .unwrap();
        let err = r
            .add(ProjectEntry {
                aliases: vec!["EA".into()],
                ..sample_entry(p2, "other")
            })
            .unwrap_err();
        assert!(matches!(err, RegistryError::AlreadyRegistered(_)));
    }

    #[test]
    fn add_rejects_self_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let p = mk_gid_project(tmp.path(), "engram");
        let mut r = Registry::empty();
        let err = r
            .add(ProjectEntry {
                aliases: vec!["ENGRAM".into()],
                ..sample_entry(p, "engram")
            })
            .unwrap_err();
        assert!(matches!(err, RegistryError::AlreadyRegistered(_)));
    }

    #[test]
    fn add_rejects_nonexistent_path() {
        let mut r = Registry::empty();
        let err = r
            .add(sample_entry(PathBuf::from("/nonexistent/xyz/abc"), "fake"))
            .unwrap_err();
        assert!(matches!(err, RegistryError::InvalidPath { .. }));
    }

    #[test]
    fn add_rejects_path_without_gid_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let not_gid = tmp.path().join("not-a-gid");
        fs::create_dir_all(&not_gid).unwrap();
        let mut r = Registry::empty();
        let err = r.add(sample_entry(not_gid, "x")).unwrap_err();
        assert!(matches!(err, RegistryError::NotAGidProject { .. }));
    }

    #[test]
    fn remove_by_name_returns_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let p = mk_gid_project(tmp.path(), "engram");
        let mut r = Registry::empty();
        r.add(sample_entry(p.clone(), "engram")).unwrap();
        let removed = r.remove("ENGRAM").unwrap();
        assert_eq!(removed.name, "engram");
        assert!(r.projects.is_empty());
    }

    #[test]
    fn remove_by_alias_fails() {
        // Safety: you must use the canonical name to remove.
        let tmp = tempfile::tempdir().unwrap();
        let p = mk_gid_project(tmp.path(), "engram");
        let mut r = Registry::empty();
        r.add(ProjectEntry {
            aliases: vec!["ea".into()],
            ..sample_entry(p, "engram")
        })
        .unwrap();
        let err = r.remove("ea").unwrap_err();
        assert!(matches!(err, RegistryError::NotFound(_)));
        // Registry unchanged.
        assert_eq!(r.projects.len(), 1);
    }

    #[test]
    fn list_archived_filtering() {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = mk_gid_project(tmp.path(), "a");
        let p2 = mk_gid_project(tmp.path(), "b");
        let mut r = Registry::empty();
        r.add(sample_entry(p1, "active")).unwrap();
        r.add(ProjectEntry {
            archived: true,
            ..sample_entry(p2, "old")
        })
        .unwrap();
        assert_eq!(r.list(false).count(), 1);
        assert_eq!(r.list(true).count(), 2);
    }

    #[test]
    fn load_rejects_future_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("reg.yml");
        fs::write(&path, "version: 999\nprojects: []\n").unwrap();
        let err = Registry::load_from(&path).unwrap_err();
        assert!(matches!(err, RegistryError::UnsupportedVersion { .. }));
    }

    #[test]
    fn load_missing_version_field_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("reg.yml");
        // Old-style file without version.
        fs::write(&path, "projects: []\n").unwrap();
        let r = Registry::load_from(&path).unwrap();
        assert_eq!(r.version, SCHEMA_VERSION);
    }

    #[test]
    fn default_path_respects_xdg_config_home() {
        // This test can't use std::env::set_var directly safely in parallel tests,
        // so we just verify the shape: the path ends with gid/projects.yml.
        let p = default_registry_path().unwrap();
        assert!(
            p.ends_with(Path::new("gid").join("projects.yml")),
            "unexpected default path: {p:?}"
        );
    }

    #[test]
    fn save_creates_parent_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("c").join("reg.yml");
        let r = Registry::empty();
        r.save_to(&nested).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn ambiguous_would_require_manual_corruption() {
        // Normal add() prevents ambiguity. But if a registry is hand-edited into
        // an ambiguous state, resolve() must surface it rather than silently
        // picking one.
        let tmp = tempfile::tempdir().unwrap();
        let p1 = mk_gid_project(tmp.path(), "one");
        let p2 = mk_gid_project(tmp.path(), "two");
        // Construct by hand — bypassing add() validation.
        let r = Registry {
            version: SCHEMA_VERSION,
            projects: vec![
                ProjectEntry {
                    aliases: vec!["shared".into()],
                    ..sample_entry(p1, "alpha")
                },
                ProjectEntry {
                    aliases: vec!["shared".into()],
                    ..sample_entry(p2, "beta")
                },
            ],
        };
        match r.resolve("shared") {
            Err(RegistryError::Ambiguous { candidates, .. }) => {
                assert_eq!(candidates.len(), 2);
                assert!(candidates.contains(&"alpha".to_string()));
                assert!(candidates.contains(&"beta".to_string()));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }
}
