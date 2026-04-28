//! [`ArtifactStore`] — kind-agnostic CRUD over project artifacts (ISS-053 §4.6).
//!
//! Every operation is kind-agnostic at the type level. Behavior differences
//! live in [`Layout`] (§4.4). The store is the only Phase D module that
//! touches the filesystem directly; lower layers (id / metadata / layout /
//! relation) are pure functions on already-loaded data.
//!
//! ## Concurrency
//!
//! - `&self` for all public methods; the relation index uses `Mutex` for
//!   interior mutability.
//! - Daemon callers wrap the store in `Arc<...>`. gid-core does not impose
//!   a sharing model.
//! - Index invalidation: query methods compare the project root's mtime
//!   against the cache's last build; if newer, the cache is rebuilt by
//!   walking all artifact files. With <1000 artifacts per project this is
//!   millisecond-cheap.
//!
//! ## Atomic writes (D3 / §4.6 "Atomic writes")
//!
//! `create` / `update` write via a sibling tempfile in the target directory
//! followed by `rename(2)` onto the final path — readers either observe
//! the old file or the new file, never a torn write. There is no advisory
//! locking; concurrent writers to the same file are last-writer-wins (the
//! design accepts this because artifact files are human-edited markdown
//! with low contention; conflicts get resolved by git, not by gid-core).
//!
//! ## No relations DB (D3)
//!
//! Relations are 100% derived from artifact files. The `RelationIndex` is
//! pure cache; it is rebuilt from disk on demand and never persisted.
//! `relate(A, kind, B)` is implemented as `update(A)` after a frontmatter
//! merge — there is no `relations.yml` / `relations.db` to keep in sync.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use thiserror::Error;

use super::artifact::{Artifact, ArtifactError};
use super::id::{ArtifactId, ArtifactIdError};
use super::layout::{Layout, LayoutError, SeqScope, SlotMap};
use super::metadata::{Metadata, MetadataError};
use super::relation::{discover, Relation, RelationError};
use crate::project_registry::{Registry, RegistryError};

/// Lazy mtime-invalidated cache of all relations in a project.
///
/// Keys are the *source* artifact id (the `from` side of a relation); the
/// value is every outgoing relation discovered from that artifact's content.
/// Reverse lookups (`relations_to`) scan the values.
pub type RelationIndex = BTreeMap<ArtifactId, Vec<Relation>>;

/// Internal cache state guarded by `Mutex`.
struct CacheState {
    /// mtime of the project root the last time we rebuilt the index. We use
    /// the deepest `.gid/` mtime as a cheap "did anything change" signal.
    last_built_at: Option<SystemTime>,
    index: RelationIndex,
}

impl CacheState {
    fn empty() -> Self {
        Self {
            last_built_at: None,
            index: RelationIndex::new(),
        }
    }
}

/// Errors produced by `ArtifactStore` operations.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("project '{name}' not found in registry: {source}")]
    Registry {
        name: String,
        #[source]
        source: RegistryError,
    },

    #[error("project root does not exist: {path}")]
    MissingProjectRoot { path: PathBuf },

    #[error("layout error: {0}")]
    Layout(#[from] LayoutError),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("metadata error: {0}")]
    Metadata(#[from] MetadataError),

    #[error("artifact error: {0}")]
    Artifact(#[from] ArtifactError),

    #[error("artifact id error: {0}")]
    Id(#[from] ArtifactIdError),

    #[error("relation discovery error: {0}")]
    Relation(#[from] RelationError),

    #[error("file already exists at {path}; refusing to overwrite")]
    AlreadyExists { path: PathBuf },

    #[error(
        "kind '{kind}' is a slug-style kind and requires caller-supplied slot '{required_slot}'"
    )]
    MissingSlotForSlugKind {
        kind: String,
        required_slot: String,
    },

    #[error(
        "kind '{kind}' has parent-scoped sequence allocation but no parent ArtifactId was supplied"
    )]
    MissingParentForKind { kind: String },

    #[error(
        "parent ArtifactId '{parent}' is not located under the expected scope path for kind '{kind}'"
    )]
    ParentScopeMismatch { kind: String, parent: String },
}

/// Kind-agnostic CRUD store over a single project's artifacts (§4.6).
///
/// Construct via [`ArtifactStore::open`]. Cross-project resolution lives in
/// the free functions [`resolve`] and [`find_references_to`] (D5).
pub struct ArtifactStore {
    project: String,
    project_root: PathBuf,
    layout: Layout,
    cache: Mutex<CacheState>,
}

impl ArtifactStore {
    /// Project name (as registered in the project registry).
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Absolute filesystem path of the project root (parent of `.gid/`).
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Layout this store is using (default + optional `.gid/layout.yml`
    /// override). Exposed so callers can render paths or inspect kinds.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    // ------------------------------------------------------------------
    // open
    // ------------------------------------------------------------------

    /// Open a store for the named project (resolved via the default
    /// project registry, `~/.config/gid/projects.yml`).
    ///
    /// The project's `.gid/layout.yml`, if present, is layered on top of
    /// the built-in default Layout (§4.4 "Default layout"). For the v1 of
    /// Phase D the override file is **not yet** consulted — only the
    /// built-in default is used. (Layout override loading is a follow-up
    /// kept out of this Phase to keep the surface area auditable; tests
    /// in the binding scalability test of §6 will drive it.)
    pub fn open(project: &str) -> Result<Self, StoreError> {
        let registry = Registry::load_default().map_err(|e| StoreError::Registry {
            name: project.to_string(),
            source: e,
        })?;
        let entry = registry
            .resolve(project)
            .map_err(|e| StoreError::Registry {
                name: project.to_string(),
                source: e,
            })?;
        Self::open_at(entry.name.clone(), entry.path.clone())
    }

    /// Lower-level constructor used by [`Self::open`] and tests: takes the
    /// project name and absolute root path directly, bypassing the
    /// registry. Public so test fixtures can construct stores without
    /// touching `~/.config/gid/projects.yml`.
    pub fn open_at(project: String, project_root: PathBuf) -> Result<Self, StoreError> {
        if !project_root.exists() {
            return Err(StoreError::MissingProjectRoot {
                path: project_root,
            });
        }
        let layout = Layout::default();
        Ok(Self {
            project,
            project_root,
            layout,
            cache: Mutex::new(CacheState::empty()),
        })
    }

    /// Open with a caller-supplied [`Layout`]. Used by the §6 binding
    /// scalability test (custom `postmortem` kind via `layout.yml` override).
    pub fn open_with_layout(
        project: String,
        project_root: PathBuf,
        layout: Layout,
    ) -> Result<Self, StoreError> {
        if !project_root.exists() {
            return Err(StoreError::MissingProjectRoot {
                path: project_root,
            });
        }
        Ok(Self {
            project,
            project_root,
            layout,
            cache: Mutex::new(CacheState::empty()),
        })
    }

    // ------------------------------------------------------------------
    // Internal: path translation
    // ------------------------------------------------------------------

    /// Absolute path on disk for a given [`ArtifactId`] within this project.
    fn abs_path(&self, id: &ArtifactId) -> PathBuf {
        self.project_root.join(id.as_path())
    }

    /// Layout-relative path (i.e. relative to `.gid/`) for a given
    /// [`ArtifactId`]. Returns `None` if the id is not under `.gid/`
    /// (which currently means: "the id is malformed for our corpus";
    /// callers will fall back to the layout's `fallback` rule).
    fn layout_relative<'a>(&self, id: &'a ArtifactId) -> Option<&'a str> {
        id.as_str().strip_prefix(".gid/")
    }

    /// Resolve a kind for an id by consulting the layout. Out-of-`.gid/`
    /// ids fall through to the layout's fallback kind (default: `note`).
    fn kind_for(&self, id: &ArtifactId) -> String {
        match self.layout_relative(id) {
            Some(rel) => self.layout.match_path(rel).kind,
            None => self.layout.match_path(id.as_str()).kind,
        }
    }

    // ------------------------------------------------------------------
    // Read: list / get
    // ------------------------------------------------------------------

    /// List every artifact under the project's `.gid/` directory.
    ///
    /// `kind_filter` (when `Some`) restricts results to that kind. A
    /// malformed-frontmatter file is reported as an error; callers like
    /// `gid artifact list` may downgrade to a warning + skip per §4.2.
    pub fn list(&self, kind_filter: Option<&str>) -> Result<Vec<Artifact>, StoreError> {
        let gid_dir = self.project_root.join(".gid");
        if !gid_dir.exists() {
            return Ok(Vec::new());
        }
        let mut out: Vec<Artifact> = Vec::new();
        walk_md_files(&gid_dir, &mut |abs_path: &Path| {
            let rel = match abs_path.strip_prefix(&self.project_root) {
                Ok(r) => r,
                Err(_) => return Ok(()),
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let id = ArtifactId::new(&rel_str)?;
            let kind = self.kind_for(&id);
            if let Some(want) = kind_filter {
                if kind != want {
                    return Ok(());
                }
            }
            let artifact = Artifact::load_raw(&self.project_root, &id, kind)?;
            out.push(artifact);
            Ok(())
        })?;
        // Stable ordering — by id path, ascending.
        out.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Ok(out)
    }

    /// Load a single artifact by id. Returns `Ok(None)` if the file does
    /// not exist (distinct from "exists but malformed", which is `Err`).
    pub fn get(&self, id: &ArtifactId) -> Result<Option<Artifact>, StoreError> {
        let abs = self.abs_path(id);
        if !abs.exists() {
            return Ok(None);
        }
        let kind = self.kind_for(id);
        Ok(Some(Artifact::load_raw(
            &self.project_root,
            id,
            kind,
        )?))
    }

    // ------------------------------------------------------------------
    // Allocate: next_id / next_path
    // ------------------------------------------------------------------

    /// Allocate the next sequence-rendered local id for `kind`.
    ///
    /// - For project-scoped sequences (e.g. `issue` → `ISS-{seq:04}`),
    ///   `parent` is ignored. The seq scans every existing pattern-match
    ///   under the project's `.gid/` and returns `max + 1`, zero-padded.
    /// - For parent-scoped sequences (e.g. `review` →
    ///   `r-{seq:NN}` under `issues/{parent_id}/reviews/`), `parent` is
    ///   required and the seq scans only that parent's directory.
    /// - Slug-style kinds (no `{seq:NN}` placeholder in the pattern, e.g.
    ///   `feature-design` requires a caller-supplied `slug`) cannot
    ///   allocate via `next_id` and return [`StoreError::MissingSlotForSlugKind`].
    pub fn next_id(
        &self,
        kind: &str,
        parent: Option<&ArtifactId>,
    ) -> Result<String, StoreError> {
        let pattern = self
            .layout
            .pattern_for_kind(kind)
            .ok_or_else(|| StoreError::Layout(LayoutError::UnknownKind(kind.to_string())))?;

        // We only allocate for patterns that contain a {seq:NN} (either
        // top-level or nested inside {id:...}).
        let template = pattern
            .id_template_str()
            .ok_or_else(|| StoreError::MissingSlotForSlugKind {
                kind: kind.to_string(),
                required_slot: "slug".to_string(),
            })?;
        let width = seq_width_in_template(&template).ok_or_else(|| {
            StoreError::MissingSlotForSlugKind {
                kind: kind.to_string(),
                required_slot: "seq".to_string(),
            }
        })?;

        let max_seq = self.scan_max_seq(kind, parent)?;
        let next = max_seq + 1;
        let max_value = 10u64.pow(width as u32);
        if next >= max_value {
            return Err(StoreError::Layout(LayoutError::SeqExhausted {
                pattern: pattern.pattern.clone(),
                max: max_value - 1,
            }));
        }
        let rendered = render_id_template(&template, next, width);
        Ok(rendered)
    }

    /// Allocate the next path for `kind`, applying `slot_overrides` for
    /// caller-supplied slots (e.g. `slug` for feature kinds, or `name` for
    /// reviews). When the kind has a `{seq:NN}` placeholder, this calls
    /// [`Self::next_id`] internally and threads the result into the
    /// rendered slots before delegating to [`Layout::resolve`].
    pub fn next_path(
        &self,
        kind: &str,
        parent: Option<&ArtifactId>,
        slot_overrides: &SlotMap,
    ) -> Result<PathBuf, StoreError> {
        let pattern = self
            .layout
            .pattern_for_kind(kind)
            .ok_or_else(|| StoreError::Layout(LayoutError::UnknownKind(kind.to_string())))?;

        let mut slots: SlotMap = slot_overrides.clone();

        // If the pattern uses {parent_id}, fill it in from `parent`.
        if pattern.pattern.contains("{parent_id}") {
            let parent = parent.ok_or_else(|| StoreError::MissingParentForKind {
                kind: kind.to_string(),
            })?;
            // parent_id is the *last* directory component of the parent's
            // canonical id (e.g., parent ".gid/issues/ISS-0042/issue.md"
            // → parent_id "ISS-0042").
            let pid = parent_id_from(parent).ok_or_else(|| StoreError::ParentScopeMismatch {
                kind: kind.to_string(),
                parent: parent.as_str().to_string(),
            })?;
            slots.entry("parent_id".to_string()).or_insert(pid);
        }

        // Auto-fill {seq} / {id:...} when the pattern needs them.
        if let Some(template) = pattern.id_template_str() {
            if !slots.contains_key("id") && !slots.contains_key("seq") {
                let id_str = self.next_id(kind, parent)?;
                // Stash the rendered id wholesale; Layout::resolve handles
                // the "id slot present, use it verbatim" branch.
                slots.insert("id".to_string(), id_str);
                // Also derive the bare seq for any future renderer need.
                if let Some(width) = seq_width_in_template(&template) {
                    let max = self.scan_max_seq(kind, parent)?;
                    let bare = format!("{:0width$}", max + 1, width = width);
                    slots.entry("seq".to_string()).or_insert(bare);
                }
            }
        } else if pattern.pattern.contains("{seq:") {
            // Top-level {seq:NN} without a {id:...} wrapper. Auto-fill seq.
            if !slots.contains_key("seq") {
                if let Some(width) = top_level_seq_width(&pattern.pattern) {
                    let max = self.scan_max_seq(kind, parent)?;
                    let next = max + 1;
                    let max_value = 10u64.pow(width as u32);
                    if next >= max_value {
                        return Err(StoreError::Layout(LayoutError::SeqExhausted {
                            pattern: pattern.pattern.clone(),
                            max: max_value - 1,
                        }));
                    }
                    slots.insert(
                        "seq".to_string(),
                        format!("{:0width$}", next, width = width),
                    );
                }
            }
        }

        let rel = self.layout.resolve(kind, &slots)?;
        // Stored under .gid/, so prefix.
        Ok(PathBuf::from(".gid").join(rel))
    }

    /// Scan existing artifacts of `kind` and return the maximum seq value.
    /// Returns 0 when the corpus is empty (so `next = max+1 = 1`).
    fn scan_max_seq(
        &self,
        kind: &str,
        parent: Option<&ArtifactId>,
    ) -> Result<u64, StoreError> {
        let pattern = self
            .layout
            .pattern_for_kind(kind)
            .ok_or_else(|| StoreError::Layout(LayoutError::UnknownKind(kind.to_string())))?;

        // Scope of scan depends on seq_scope.
        let scan_root = match &pattern.seq_scope {
            SeqScope::Project => self.project_root.join(".gid"),
            SeqScope::Parent { rel } => {
                let parent = parent.ok_or_else(|| StoreError::MissingParentForKind {
                    kind: kind.to_string(),
                })?;
                let pid = parent_id_from(parent).ok_or_else(|| {
                    StoreError::ParentScopeMismatch {
                        kind: kind.to_string(),
                        parent: parent.as_str().to_string(),
                    }
                })?;
                let mut slots = SlotMap::new();
                slots.insert("parent_id".to_string(), pid.clone());
                slots.insert("slug".to_string(), pid);
                let resolved = render_literal_template(rel, &slots);
                self.project_root.join(".gid").join(resolved)
            }
        };

        if !scan_root.exists() {
            return Ok(0);
        }

        let mut max_seq: u64 = 0;
        let kind_target = kind.to_string();
        walk_md_files(&scan_root, &mut |abs: &Path| {
            let rel = match abs.strip_prefix(&self.project_root.join(".gid")) {
                Ok(r) => r,
                Err(_) => return Ok(()),
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let m = self.layout.match_path(&rel_str);
            if m.kind != kind_target {
                return Ok(());
            }
            if let Some(seq) = m.slots.get("seq") {
                if let Ok(n) = seq.parse::<u64>() {
                    if n > max_seq {
                        max_seq = n;
                    }
                }
            }
            Ok(())
        })?;
        Ok(max_seq)
    }

    // ------------------------------------------------------------------
    // Write: create / update
    // ------------------------------------------------------------------

    /// Create a new artifact at `path` (relative path, will be joined onto
    /// the project root). Refuses to overwrite an existing file. Writes
    /// atomically via tempfile + rename.
    pub fn create(
        &self,
        path: &Path,
        metadata: Metadata,
        body: &str,
    ) -> Result<Artifact, StoreError> {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project_root.join(path)
        };
        if abs.exists() {
            return Err(StoreError::AlreadyExists { path: abs });
        }
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut content = metadata.render();
        content.push_str(body);
        atomic_write(&abs, &content)?;

        let rel = abs
            .strip_prefix(&self.project_root)
            .map_err(|_| StoreError::Io {
                path: abs.clone(),
                source: io::Error::new(
                    io::ErrorKind::Other,
                    "created path is outside the project root",
                ),
            })?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let id = ArtifactId::new(&rel_str)?;
        let kind = self.kind_for(&id);
        // Invalidate cache.
        self.invalidate_cache();
        Ok(Artifact {
            id,
            kind,
            metadata,
            body: body.to_string(),
        })
    }

    /// Persist an existing artifact's current in-memory state to disk
    /// (atomic). The artifact's id determines the target file; we do not
    /// move/rename. Round-trip safety (D4): unmodified metadata produces
    /// byte-identical output.
    pub fn update(&self, artifact: &Artifact) -> Result<(), StoreError> {
        let abs = self.abs_path(&artifact.id);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let content = artifact.render();
        atomic_write(&abs, &content)?;
        self.invalidate_cache();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Relations: relations_from / relations_to
    // ------------------------------------------------------------------

    /// All outgoing relations from `id` (relations whose `from` equals `id`).
    /// Triggers an mtime-driven cache rebuild if the project has changed
    /// since the last build.
    pub fn relations_from(&self, id: &ArtifactId) -> Result<Vec<Relation>, StoreError> {
        self.ensure_cache()?;
        let cache = self.cache.lock().expect("cache mutex poisoned");
        Ok(cache.index.get(id).cloned().unwrap_or_default())
    }

    /// All incoming relations to `id` (relations whose `to` equals `id`).
    /// Scans the index values, which is O(total_relations) — acceptable
    /// for <1000 artifacts per project (§4.6 design assumption).
    pub fn relations_to(&self, id: &ArtifactId) -> Result<Vec<Relation>, StoreError> {
        self.ensure_cache()?;
        let cache = self.cache.lock().expect("cache mutex poisoned");
        let mut out: Vec<Relation> = Vec::new();
        for relations in cache.index.values() {
            for rel in relations {
                if &rel.to == id {
                    out.push(rel.clone());
                }
            }
        }
        Ok(out)
    }

    /// Force-invalidate the relation cache. Called automatically by
    /// `create`/`update`. Public so test harnesses and CLIs that mutate
    /// files outside this crate can poke the store.
    pub fn invalidate_cache(&self) {
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        cache.last_built_at = None;
        cache.index.clear();
    }

    /// Rebuild the relation index if `.gid/` mtime is newer than the
    /// last-built timestamp (or the cache has never been built).
    fn ensure_cache(&self) -> Result<(), StoreError> {
        let gid_dir = self.project_root.join(".gid");
        let current_mtime = if gid_dir.exists() {
            Some(deepest_mtime(&gid_dir)?)
        } else {
            None
        };

        {
            let cache = self.cache.lock().expect("cache mutex poisoned");
            if let (Some(built), Some(cur)) = (cache.last_built_at, current_mtime) {
                if built >= cur {
                    return Ok(());
                }
            }
        }

        // Rebuild outside the lock to avoid holding it during disk IO.
        let artifacts = self.list(None)?;
        let registry = Registry::load_default().unwrap_or_else(|_| Registry::empty());
        let mut new_index: RelationIndex = RelationIndex::new();
        for art in &artifacts {
            let rels = discover(art, &self.layout, Some(&registry))?;
            if !rels.is_empty() {
                new_index.insert(art.id.clone(), rels);
            }
        }

        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        cache.index = new_index;
        cache.last_built_at = current_mtime;
        Ok(())
    }
}

// ----------------------------------------------------------------------
// Free fns: cross-project resolve / find_references_to (D5)
// ----------------------------------------------------------------------

/// Resolve a (possibly project-prefixed) [`ArtifactId`] across the local
/// project registry. The id may be canonical (no project prefix; resolved
/// against the registry's default project) or a short form previously
/// parsed via `ArtifactId::parse_short` and rebuilt as a plain `ArtifactId`.
///
/// **Phase D scope:** the supplied id is treated as canonical (path-only).
/// Cross-project lookup-by-short-form is provided by callers that go
/// through `ArtifactId::parse_short` themselves and pass the resolved
/// `(project, id)` tuple to [`resolve_in`].
pub fn resolve(_id: &ArtifactId) -> Result<Artifact, StoreError> {
    // Until we wire short-form resolution at the gid-core surface (a
    // follow-up to Phase D — needs CLI/MCP context to choose the
    // "default project"), this is intentionally not implemented.
    Err(StoreError::Registry {
        name: "<unspecified>".into(),
        source: RegistryError::NotFound(
            "resolve() requires explicit project; use resolve_in()".into(),
        ),
    })
}

/// Resolve `id` (project-relative) within the project named `project`.
/// Convenience wrapper around `ArtifactStore::open` + `get`.
pub fn resolve_in(project: &str, id: &ArtifactId) -> Result<Artifact, StoreError> {
    let store = ArtifactStore::open(project)?;
    store
        .get(id)?
        .ok_or_else(|| StoreError::Io {
            path: store.abs_path(id),
            source: io::Error::new(io::ErrorKind::NotFound, "artifact not found"),
        })
}

/// Find every relation pointing at `target`, scanning across every
/// project in the local registry.
///
/// **Phase D scope:** signature accepts a single project context for now;
/// cross-project scans are wired in by [`find_references_to_in_projects`].
/// This default function scans only the registry's first non-archived
/// project (the most common case in CI fixtures).
pub fn find_references_to(target: &ArtifactId) -> Result<Vec<Relation>, StoreError> {
    let registry = Registry::load_default().map_err(|e| StoreError::Registry {
        name: "<default>".into(),
        source: e,
    })?;
    let mut out: Vec<Relation> = Vec::new();
    for entry in registry.list(false) {
        if let Ok(store) = ArtifactStore::open_at(entry.name.clone(), entry.path.clone()) {
            if let Ok(rels) = store.relations_to(target) {
                out.extend(rels);
            }
        }
    }
    Ok(out)
}

/// Like [`find_references_to`] but takes an explicit set of stores —
/// avoids registry IO for tests / explicit pipelines.
pub fn find_references_to_in_projects(
    target: &ArtifactId,
    stores: &[&ArtifactStore],
) -> Result<Vec<Relation>, StoreError> {
    let mut out: Vec<Relation> = Vec::new();
    for store in stores {
        out.extend(store.relations_to(target)?);
    }
    Ok(out)
}

// ----------------------------------------------------------------------
// Helpers: id template / sequence / path utilities
// ----------------------------------------------------------------------

/// Width of the *first* `{seq:NN}` token inside an id template body
/// (the inner text of `{id:TEMPLATE}`).
fn seq_width_in_template(template: &str) -> Option<usize> {
    let bytes = template.as_bytes();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        if &bytes[i..i + 5] == b"{seq:" {
            let mut j = i + 5;
            let start = j;
            while j < bytes.len() && bytes[j] != b'}' {
                j += 1;
            }
            if j < bytes.len() {
                let n: usize = template[start..j].parse().ok()?;
                return Some(n);
            }
        }
        i += 1;
    }
    None
}

/// Width of the first top-level `{seq:NN}` in a full pattern (used when
/// the pattern has no `{id:...}` wrapper).
fn top_level_seq_width(pattern: &str) -> Option<usize> {
    seq_width_in_template(pattern)
}

/// Render an id template like `"ISS-{seq:04}"` with a concrete value.
fn render_id_template(template: &str, value: u64, width: usize) -> String {
    // Replace the {seq:NN} token with the zero-padded value. There is
    // exactly one {seq:...} per id template (Phase D assumption — matches
    // every layout pattern shipped today).
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len() + width);
    let mut i = 0;
    while i < bytes.len() {
        if i + 5 <= bytes.len() && &bytes[i..i + 5] == b"{seq:" {
            // Skip until '}'.
            let mut j = i + 5;
            while j < bytes.len() && bytes[j] != b'}' {
                j += 1;
            }
            out.push_str(&format!("{:0width$}", value, width = width));
            i = j + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Substitute `{slug}` and `{parent_id}` tokens in a literal template
/// (used by parent-scoped `seq_scope` directory resolution). Falls back
/// to dropping unmatched tokens.
fn render_literal_template(template: &str, slots: &SlotMap) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // Find matching `}`.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'}' {
                j += 1;
            }
            if j < bytes.len() {
                let key = &template[i + 1..j];
                if let Some(v) = slots.get(key) {
                    out.push_str(v);
                }
                i = j + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Extract the `parent_id` (last directory component) of a parent
/// artifact id. For `.gid/issues/ISS-0042/issue.md` this is `ISS-0042`.
fn parent_id_from(parent: &ArtifactId) -> Option<String> {
    let p = Path::new(parent.as_str());
    p.parent()
        .and_then(|d| d.file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// Atomically write `content` to `path` via a sibling tempfile + rename.
fn atomic_write(path: &Path, content: &str) -> Result<(), StoreError> {
    let parent = path.parent().ok_or_else(|| StoreError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"),
    })?;
    let file_name = path.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
        StoreError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"),
        }
    })?;
    let tmp = parent.join(format!(".{}.tmp", file_name));
    {
        let mut f = fs::File::create(&tmp).map_err(|source| StoreError::Io {
            path: tmp.clone(),
            source,
        })?;
        f.write_all(content.as_bytes())
            .map_err(|source| StoreError::Io {
                path: tmp.clone(),
                source,
            })?;
        f.sync_all().map_err(|source| StoreError::Io {
            path: tmp.clone(),
            source,
        })?;
    }
    fs::rename(&tmp, path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Walk `root` and return the maximum mtime of any file or directory under
/// it. A cheap "did anything change" signal for the relation index cache.
fn deepest_mtime(root: &Path) -> Result<SystemTime, StoreError> {
    let mut latest = fs::metadata(root)
        .map_err(|source| StoreError::Io {
            path: root.to_path_buf(),
            source,
        })?
        .modified()
        .map_err(|source| StoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
    let entries = match fs::read_dir(root) {
        Ok(it) => it,
        Err(_) => return Ok(latest),
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let meta = match fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if let Ok(t) = meta.modified() {
            if t > latest {
                latest = t;
            }
        }
        if meta.is_dir() {
            if let Ok(t) = deepest_mtime(&p) {
                if t > latest {
                    latest = t;
                }
            }
        }
    }
    Ok(latest)
}

// ----------------------------------------------------------------------
// Filesystem walk helper
// ----------------------------------------------------------------------

/// Recursively walk `root`, invoking `visit` once per `*.md` file found
/// (depth-first, deterministic ordering). Errors surface verbatim.
fn walk_md_files<F>(root: &Path, visit: &mut F) -> Result<(), StoreError>
where
    F: FnMut(&Path) -> Result<(), StoreError>,
{
    let mut entries: Vec<_> = fs::read_dir(root)
        .map_err(|source| StoreError::Io {
            path: root.to_path_buf(),
            source,
        })?
        .filter_map(|r| r.ok())
        .collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let ft = entry.file_type().map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        if ft.is_dir() {
            walk_md_files(&path, visit)?;
        } else if ft.is_file()
            && path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
        {
            visit(&path)?;
        }
    }
    Ok(())
}


// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::layout::Layout;
    use crate::artifact::metadata::{FieldValue, MetaSourceHint};
    use crate::artifact::relation::RelationSource;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Per-test sandbox under the OS tempdir. Uses an atomic counter so
    /// parallel tests never collide. Auto-cleaned on Drop.
    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(label: &'static str) -> Self {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let root = std::env::temp_dir().join(format!(
                "iss053-store-{}-{}-{}",
                label, pid, n
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join(".gid")).unwrap();
            Self { root }
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let path = self.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }

        fn store(&self) -> ArtifactStore {
            ArtifactStore::open_at("test".into(), self.root.clone()).unwrap()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn issue_body(title: &str) -> String {
        format!("---\ntitle: {}\n---\n\nbody\n", title)
    }

    // ----- open / open_at ------------------------------------------------

    #[test]
    fn open_at_succeeds_for_existing_root() {
        let sb = Sandbox::new("open_at_ok");
        let store = ArtifactStore::open_at("foo".into(), sb.root.clone()).unwrap();
        assert_eq!(store.project(), "foo");
        assert_eq!(store.project_root(), sb.root);
    }

    #[test]
    fn open_at_fails_for_missing_root() {
        let result = ArtifactStore::open_at(
            "missing".into(),
            PathBuf::from("/tmp/iss053-definitely-does-not-exist-xyz"),
        );
        assert!(matches!(
            result,
            Err(StoreError::MissingProjectRoot { .. })
        ));
    }

    // ----- list / get ----------------------------------------------------

    #[test]
    fn list_empty_corpus_returns_empty_vec() {
        let sb = Sandbox::new("list_empty");
        let store = sb.store();
        let items = store.list(None).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn list_finds_issue_and_resolves_kind() {
        let sb = Sandbox::new("list_one");
        sb.write(
            ".gid/issues/ISS-0001/issue.md",
            &issue_body("first"),
        );
        let store = sb.store();
        let items = store.list(None).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "issue");
        assert_eq!(items[0].id.as_str(), ".gid/issues/ISS-0001/issue.md");
    }

    #[test]
    fn list_kind_filter_excludes_others() {
        let sb = Sandbox::new("list_filter");
        sb.write(".gid/issues/ISS-0001/issue.md", &issue_body("a"));
        sb.write(".gid/issues/ISS-0001/design.md", &issue_body("d"));
        let store = sb.store();
        let issues = store.list(Some("issue")).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, "issue");
        let designs = store.list(Some("design")).unwrap();
        assert_eq!(designs.len(), 1);
        assert_eq!(designs[0].kind, "design");
    }

    #[test]
    fn list_returns_stable_order() {
        let sb = Sandbox::new("list_order");
        sb.write(".gid/issues/ISS-0003/issue.md", &issue_body("c"));
        sb.write(".gid/issues/ISS-0001/issue.md", &issue_body("a"));
        sb.write(".gid/issues/ISS-0002/issue.md", &issue_body("b"));
        let store = sb.store();
        let items = store.list(None).unwrap();
        let paths: Vec<_> = items.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                ".gid/issues/ISS-0001/issue.md",
                ".gid/issues/ISS-0002/issue.md",
                ".gid/issues/ISS-0003/issue.md",
            ]
        );
    }

    #[test]
    fn get_returns_none_for_missing_file() {
        let sb = Sandbox::new("get_missing");
        let store = sb.store();
        let id = ArtifactId::new(".gid/issues/ISS-9999/issue.md").unwrap();
        assert!(store.get(&id).unwrap().is_none());
    }

    #[test]
    fn get_returns_artifact_with_kind() {
        let sb = Sandbox::new("get_ok");
        sb.write(".gid/issues/ISS-0007/issue.md", &issue_body("seven"));
        let store = sb.store();
        let id = ArtifactId::new(".gid/issues/ISS-0007/issue.md").unwrap();
        let art = store.get(&id).unwrap().expect("must exist");
        assert_eq!(art.kind, "issue");
        assert!(art.body.contains("body"));
    }

    // ----- next_id -------------------------------------------------------

    #[test]
    fn next_id_starts_at_0001_for_empty_project() {
        let sb = Sandbox::new("nextid_empty");
        let store = sb.store();
        let id = store.next_id("issue", None).unwrap();
        assert_eq!(id, "ISS-0001");
    }

    #[test]
    fn next_id_increments_past_existing_max() {
        let sb = Sandbox::new("nextid_inc");
        sb.write(".gid/issues/ISS-0042/issue.md", &issue_body("x"));
        sb.write(".gid/issues/ISS-0007/issue.md", &issue_body("y"));
        let store = sb.store();
        let id = store.next_id("issue", None).unwrap();
        assert_eq!(id, "ISS-0043");
    }

    #[test]
    fn next_id_unknown_kind_errors() {
        let sb = Sandbox::new("nextid_unknown");
        let store = sb.store();
        let r = store.next_id("not-a-real-kind", None);
        assert!(matches!(r, Err(StoreError::Layout(LayoutError::UnknownKind(_)))));
    }

    #[test]
    fn next_id_slug_kind_errors_clearly() {
        let sb = Sandbox::new("nextid_slug");
        let store = sb.store();
        // feature-design's pattern is "features/{slug}/design.md" — no
        // {seq:NN}, so next_id can't allocate.
        let r = store.next_id("feature-design", None);
        assert!(matches!(r, Err(StoreError::MissingSlotForSlugKind { .. })));
    }

    // ----- next_path -----------------------------------------------------

    #[test]
    fn next_path_for_issue_renders_full_relative_path() {
        let sb = Sandbox::new("nextpath_issue");
        let store = sb.store();
        let path = store.next_path("issue", None, &SlotMap::new()).unwrap();
        assert_eq!(path, PathBuf::from(".gid/issues/ISS-0001/issue.md"));
    }

    #[test]
    fn next_path_with_existing_artifacts_continues_sequence() {
        let sb = Sandbox::new("nextpath_seq");
        sb.write(".gid/issues/ISS-0050/issue.md", &issue_body("x"));
        let store = sb.store();
        let path = store.next_path("issue", None, &SlotMap::new()).unwrap();
        assert_eq!(path, PathBuf::from(".gid/issues/ISS-0051/issue.md"));
    }

    #[test]
    fn next_path_for_feature_design_with_slug_works() {
        let sb = Sandbox::new("nextpath_feat");
        let store = sb.store();
        let mut slots = SlotMap::new();
        slots.insert("slug".into(), "auth".into());
        let path = store.next_path("feature-design", None, &slots).unwrap();
        assert_eq!(path, PathBuf::from(".gid/features/auth/design.md"));
    }

    // ----- create / update ----------------------------------------------

    #[test]
    fn create_writes_atomically_and_returns_artifact() {
        let sb = Sandbox::new("create_ok");
        let store = sb.store();
        let mut meta = Metadata::new(MetaSourceHint::Frontmatter);
        meta.set_field("title", FieldValue::Scalar("hello".into()));
        let path = sb.root.join(".gid/issues/ISS-0001/issue.md");
        let rel = PathBuf::from(".gid/issues/ISS-0001/issue.md");
        let art = store.create(&rel, meta, "body text\n").unwrap();
        assert!(path.exists());
        assert_eq!(art.kind, "issue");
        assert_eq!(art.id.as_str(), ".gid/issues/ISS-0001/issue.md");
        let on_disk = fs::read_to_string(&path).unwrap();
        assert!(on_disk.starts_with("---\n"));
        assert!(on_disk.contains("title: hello"));
        assert!(on_disk.ends_with("body text\n"));
    }

    #[test]
    fn create_refuses_to_overwrite_existing_file() {
        let sb = Sandbox::new("create_no_overwrite");
        sb.write(".gid/issues/ISS-0001/issue.md", &issue_body("old"));
        let store = sb.store();
        let meta = Metadata::new(MetaSourceHint::Frontmatter);
        let rel = PathBuf::from(".gid/issues/ISS-0001/issue.md");
        let r = store.create(&rel, meta, "new");
        assert!(matches!(r, Err(StoreError::AlreadyExists { .. })));
        // Original file is untouched.
        let on_disk = fs::read_to_string(sb.root.join(&rel)).unwrap();
        assert!(on_disk.contains("title: old"));
    }

    #[test]
    fn update_writes_byte_identical_for_unchanged_metadata() {
        let sb = Sandbox::new("update_roundtrip");
        let original = "---\ntitle: x\nrelated: [ISS-0002]\n---\n\nbody\n";
        sb.write(".gid/issues/ISS-0001/issue.md", original);
        let store = sb.store();
        let id = ArtifactId::new(".gid/issues/ISS-0001/issue.md").unwrap();
        let art = store.get(&id).unwrap().unwrap();
        store.update(&art).unwrap();
        let after = fs::read_to_string(sb.root.join(id.as_path())).unwrap();
        assert_eq!(after, original);
    }

    #[test]
    fn update_persists_metadata_changes() {
        let sb = Sandbox::new("update_change");
        sb.write(".gid/issues/ISS-0001/issue.md", &issue_body("old"));
        let store = sb.store();
        let id = ArtifactId::new(".gid/issues/ISS-0001/issue.md").unwrap();
        let mut art = store.get(&id).unwrap().unwrap();
        art.metadata
            .set_field("title", FieldValue::Scalar("new".into()));
        store.update(&art).unwrap();
        let reloaded = store.get(&id).unwrap().unwrap();
        assert_eq!(
            reloaded
                .metadata
                .get("title")
                .and_then(|v| v.as_scalar()),
            Some("new")
        );
    }

    // ----- relations_from / relations_to --------------------------------

    #[test]
    fn relations_from_finds_frontmatter_refs() {
        let sb = Sandbox::new("rel_from_fm");
        sb.write(
            ".gid/issues/ISS-0001/issue.md",
            "---\ntitle: a\nrelated: [ISS-0002]\n---\nbody\n",
        );
        sb.write(".gid/issues/ISS-0002/issue.md", &issue_body("b"));
        let store = sb.store();
        let id = ArtifactId::new(".gid/issues/ISS-0001/issue.md").unwrap();
        let rels = store.relations_from(&id).unwrap();
        // At least one Frontmatter relation pointing at ISS-0002.
        assert!(rels.iter().any(|r| {
            matches!(&r.source, RelationSource::Frontmatter { field } if field == "related")
                && r.to.as_str().contains("ISS-0002")
        }));
    }

    #[test]
    fn relations_to_finds_reverse_via_directory_nesting() {
        let sb = Sandbox::new("rel_to_nest");
        sb.write(".gid/issues/ISS-0001/issue.md", &issue_body("parent"));
        sb.write(
            ".gid/issues/ISS-0001/reviews/design-r1.md",
            &issue_body("review"),
        );
        let store = sb.store();
        let parent = ArtifactId::new(".gid/issues/ISS-0001/issue.md").unwrap();
        let rels = store.relations_to(&parent).unwrap();
        assert!(rels.iter().any(|r| matches!(
            &r.source,
            RelationSource::DirectoryNesting
        )));
    }

    #[test]
    fn relations_cache_invalidates_on_create() {
        let sb = Sandbox::new("rel_invalidate");
        sb.write(".gid/issues/ISS-0001/issue.md", &issue_body("a"));
        let store = sb.store();
        let id = ArtifactId::new(".gid/issues/ISS-0001/issue.md").unwrap();
        let before = store.relations_from(&id).unwrap();
        assert!(before.is_empty());

        // Replace the artifact via update with a related field — this
        // goes through invalidate_cache.
        let mut art = store.get(&id).unwrap().unwrap();
        art.metadata.set_field(
            "related",
            FieldValue::List(vec!["ISS-0002".into()]),
        );
        store.update(&art).unwrap();

        let after = store.relations_from(&id).unwrap();
        assert!(!after.is_empty());
    }

    // ----- ISS-050 collision regression ---------------------------------

    #[test]
    fn two_projects_can_both_have_iss_0050() {
        let sb_a = Sandbox::new("collide_a");
        let sb_b = Sandbox::new("collide_b");
        sb_a.write(".gid/issues/ISS-0050/issue.md", &issue_body("a"));
        sb_b.write(".gid/issues/ISS-0050/issue.md", &issue_body("b"));
        let store_a = ArtifactStore::open_at("a".into(), sb_a.root.clone()).unwrap();
        let store_b = ArtifactStore::open_at("b".into(), sb_b.root.clone()).unwrap();

        let id_a = ArtifactId::new(".gid/issues/ISS-0050/issue.md").unwrap();
        let art_a = store_a.get(&id_a).unwrap().unwrap();
        let art_b = store_b.get(&id_a).unwrap().unwrap();
        assert_eq!(
            art_a.metadata.get("title").and_then(|v| v.as_scalar()),
            Some("a")
        );
        assert_eq!(
            art_b.metadata.get("title").and_then(|v| v.as_scalar()),
            Some("b")
        );

        // next_id is project-scoped — both projects independently land on 0051.
        let next_a = store_a.next_id("issue", None).unwrap();
        let next_b = store_b.next_id("issue", None).unwrap();
        assert_eq!(next_a, "ISS-0051");
        assert_eq!(next_b, "ISS-0051");
    }

    // ----- find_references_to_in_projects -------------------------------

    #[test]
    fn cross_project_find_references_to() {
        let sb_a = Sandbox::new("xref_a");
        let sb_b = Sandbox::new("xref_b");
        sb_a.write(".gid/issues/ISS-0001/issue.md", &issue_body("target"));
        sb_b.write(
            ".gid/issues/ISS-0002/issue.md",
            "---\ntitle: source\nrelated: [.gid/issues/ISS-0001/issue.md]\n---\nbody\n",
        );
        let store_a = ArtifactStore::open_at("a".into(), sb_a.root.clone()).unwrap();
        let store_b = ArtifactStore::open_at("b".into(), sb_b.root.clone()).unwrap();

        let target = ArtifactId::new(".gid/issues/ISS-0001/issue.md").unwrap();
        let stores: Vec<&ArtifactStore> = vec![&store_a, &store_b];
        let rels = find_references_to_in_projects(&target, &stores).unwrap();
        assert!(!rels.is_empty(), "expected at least one cross-project reference");
    }

    // ----- helpers self-test --------------------------------------------

    #[test]
    fn render_id_template_pads_seq_correctly() {
        assert_eq!(render_id_template("ISS-{seq:04}", 7, 4), "ISS-0007");
        assert_eq!(render_id_template("PM-{seq:03}", 1, 3), "PM-001");
    }

    #[test]
    fn seq_width_in_template_extracts_width() {
        assert_eq!(seq_width_in_template("ISS-{seq:04}"), Some(4));
        assert_eq!(seq_width_in_template("PM-{seq:03}"), Some(3));
        assert_eq!(seq_width_in_template("no-seq-here"), None);
    }

    #[test]
    fn parent_id_from_extracts_last_directory_component() {
        let p = ArtifactId::new(".gid/issues/ISS-0042/issue.md").unwrap();
        assert_eq!(parent_id_from(&p).as_deref(), Some("ISS-0042"));
    }

    #[test]
    fn open_with_layout_uses_supplied_layout() {
        let sb = Sandbox::new("open_with");
        let custom = Layout::default();
        let store = ArtifactStore::open_with_layout(
            "x".into(),
            sb.root.clone(),
            custom,
        )
        .unwrap();
        // With default layout, an unknown-shape file falls back to "note".
        sb.write(".gid/random.md", &issue_body("z"));
        let items = store.list(None).unwrap();
        assert!(items.iter().any(|a| a.kind == "note" || a.kind == "doc"));
    }

    #[test]
    fn invalidate_cache_forces_rebuild() {
        let sb = Sandbox::new("cache_invalid");
        sb.write(".gid/issues/ISS-0001/issue.md", &issue_body("a"));
        let store = sb.store();
        let id = ArtifactId::new(".gid/issues/ISS-0001/issue.md").unwrap();
        let _ = store.relations_from(&id).unwrap();
        store.invalidate_cache();
        // Should not panic; cache rebuilds transparently.
        let _ = store.relations_from(&id).unwrap();
    }
}
