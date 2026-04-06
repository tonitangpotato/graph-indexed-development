//! Artifact Manager — Track and resolve artifacts between phases.
//!
//! Manages the flow of files (artifacts) between ritual phases,
//! supporting globs and reference resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result, bail};

use super::definition::ArtifactRef;

/// Manages artifacts produced by ritual phases.
#[derive(Debug, Clone)]
pub struct ArtifactManager {
    /// Project root directory.
    project_root: PathBuf,
    /// Artifacts produced by each phase, keyed by phase ID.
    produced: HashMap<String, Vec<PathBuf>>,
}

impl ArtifactManager {
    /// Create a new artifact manager.
    pub fn new(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            produced: HashMap::new(),
        }
    }
    
    /// Record artifacts produced by a phase.
    pub fn record(&mut self, phase_id: &str, paths: Vec<PathBuf>) {
        self.produced
            .entry(phase_id.to_string())
            .or_default()
            .extend(paths);
    }
    
    /// Resolve an artifact reference to actual file paths.
    ///
    /// Supports glob patterns and phase references.
    pub fn resolve(&self, artifact_ref: &ArtifactRef) -> Result<Vec<PathBuf>> {
        let pattern = &artifact_ref.path;
        
        // If from a specific phase, check our records first
        if let Some(ref from_phase) = artifact_ref.from_phase {
            if let Some(produced) = self.produced.get(from_phase) {
                // Filter produced artifacts by pattern
                let matching: Vec<PathBuf> = produced.iter()
                    .filter(|p| {
                        let p_str = p.to_string_lossy();
                        if pattern.contains('*') || pattern.contains('{') {
                            // Replace template variables with glob wildcards
                            let glob_pattern = pattern
                                .replace("{feature}", "*")
                                .replace("{component}", "*")
                                .replace("{module}", "*")
                                .replace("{service}", "*");
                            if let Ok(compiled) = glob::Pattern::new(&glob_pattern) {
                                compiled.matches(&p_str)
                            } else {
                                false
                            }
                        } else {
                            p_str.ends_with(pattern) || **p == self.project_root.join(pattern)
                        }
                    })
                    .cloned()
                    .collect();
                
                if !matching.is_empty() {
                    return Ok(matching);
                }
            }
        }
        
        // Fall back to filesystem glob
        self.resolve_glob(pattern)
    }
    
    /// Resolve a glob pattern to actual file paths.
    fn resolve_glob(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        // Handle template variables like {feature}
        // For now, just replace with wildcard for glob matching
        let glob_pattern = pattern
            .replace("{feature}", "*")
            .replace("{component}", "*")
            .replace("{module}", "*")
            .replace("{service}", "*");
        
        let full_pattern = self.project_root.join(&glob_pattern);
        let pattern_str = full_pattern.to_string_lossy().to_string();
        
        let paths: Vec<PathBuf> = glob::glob(&pattern_str)
            .with_context(|| format!("Invalid glob pattern: {}", pattern))?
            .filter_map(Result::ok)
            .collect();
        
        Ok(paths)
    }
    
    /// Verify that all required output artifacts exist on disk.
    pub fn verify_outputs(&self, phase_id: &str, outputs: &[super::definition::ArtifactSpec]) -> Result<()> {
        for output in outputs {
            if output.required {
                let resolved = self.resolve_glob(&output.path)?;
                if resolved.is_empty() {
                    bail!(
                        "Phase '{}' missing required output artifact: {}",
                        phase_id, output.path
                    );
                }
            }
        }
        Ok(())
    }
    
    /// Get all artifacts produced by a specific phase.
    pub fn get(&self, phase_id: &str) -> Option<&Vec<PathBuf>> {
        self.produced.get(phase_id)
    }
    
    /// Get all recorded artifacts.
    pub fn get_all(&self) -> &HashMap<String, Vec<PathBuf>> {
        &self.produced
    }
    
    /// Check if any artifacts have been recorded for a phase.
    pub fn has_artifacts(&self, phase_id: &str) -> bool {
        self.produced.get(phase_id).map(|v| !v.is_empty()).unwrap_or(false)
    }
    
    /// Clear all recorded artifacts.
    pub fn clear(&mut self) {
        self.produced.clear();
    }

    /// Clear recorded artifacts for a specific phase.
    pub fn clear_phase(&mut self, phase_id: &str) {
        self.produced.remove(phase_id);
    }

    /// Rebuild artifact records by scanning the filesystem based on phase outputs.
    pub fn rebuild_from_disk(&mut self, phases: &[super::definition::PhaseDefinition]) {
        self.produced.clear();
        
        for phase in phases {
            let mut phase_artifacts = Vec::new();
            
            for output in &phase.output {
                if let Ok(paths) = self.resolve_glob(&output.path) {
                    phase_artifacts.extend(paths);
                }
            }
            
            if !phase_artifacts.is_empty() {
                self.produced.insert(phase.id.clone(), phase_artifacts);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;
    
    #[test]
    fn test_record_and_get() {
        let temp_dir = TempDir::new().unwrap();
        let mut mgr = ArtifactManager::new(temp_dir.path());
        
        mgr.record("phase1", vec![PathBuf::from("file1.txt")]);
        mgr.record("phase1", vec![PathBuf::from("file2.txt")]);
        mgr.record("phase2", vec![PathBuf::from("file3.txt")]);
        
        let phase1_artifacts = mgr.get("phase1").unwrap();
        assert_eq!(phase1_artifacts.len(), 2);
        
        let phase2_artifacts = mgr.get("phase2").unwrap();
        assert_eq!(phase2_artifacts.len(), 1);
        
        assert!(mgr.get("phase3").is_none());
    }
    
    #[test]
    fn test_resolve_simple_path() {
        let temp_dir = TempDir::new().unwrap();
        let mut mgr = ArtifactManager::new(temp_dir.path());
        
        // Create a file
        let file_path = temp_dir.path().join("output.txt");
        fs::write(&file_path, "test").unwrap();
        
        // Record it
        mgr.record("phase1", vec![file_path.clone()]);
        
        // Resolve reference
        let artifact_ref = ArtifactRef {
            from_phase: Some("phase1".to_string()),
            path: "output.txt".to_string(),
        };
        
        let resolved = mgr.resolve(&artifact_ref).unwrap();
        assert_eq!(resolved.len(), 1);
    }
    
    #[test]
    fn test_resolve_glob() {
        let temp_dir = TempDir::new().unwrap();
        let mgr = ArtifactManager::new(temp_dir.path());
        
        // Create directory structure
        let features_dir = temp_dir.path().join(".gid/features");
        fs::create_dir_all(&features_dir.join("auth")).unwrap();
        fs::create_dir_all(&features_dir.join("api")).unwrap();
        
        // Create files
        fs::write(features_dir.join("auth/requirements.md"), "auth").unwrap();
        fs::write(features_dir.join("api/requirements.md"), "api").unwrap();
        
        // Resolve glob
        let artifact_ref = ArtifactRef {
            from_phase: None,
            path: ".gid/features/*/requirements.md".to_string(),
        };
        
        let resolved = mgr.resolve(&artifact_ref).unwrap();
        assert_eq!(resolved.len(), 2);
    }
    
    #[test]
    fn test_verify_outputs_success() {
        let temp_dir = TempDir::new().unwrap();
        let mgr = ArtifactManager::new(temp_dir.path());
        
        // Create required file
        fs::write(temp_dir.path().join("required.txt"), "content").unwrap();
        
        let outputs = vec![
            super::super::definition::ArtifactSpec {
                path: "required.txt".to_string(),
                required: true,
            },
        ];
        
        let result = mgr.verify_outputs("test", &outputs);
        assert!(result.is_ok());
    }
    
    #[test]
    fn test_verify_outputs_missing() {
        let temp_dir = TempDir::new().unwrap();
        let mgr = ArtifactManager::new(temp_dir.path());
        
        let outputs = vec![
            super::super::definition::ArtifactSpec {
                path: "missing.txt".to_string(),
                required: true,
            },
        ];
        
        let result = mgr.verify_outputs("test", &outputs);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_verify_outputs_optional() {
        let temp_dir = TempDir::new().unwrap();
        let mgr = ArtifactManager::new(temp_dir.path());
        
        let outputs = vec![
            super::super::definition::ArtifactSpec {
                path: "optional.txt".to_string(),
                required: false,
            },
        ];
        
        let result = mgr.verify_outputs("test", &outputs);
        assert!(result.is_ok());
    }
    
    #[test]
    fn test_has_artifacts() {
        let temp_dir = TempDir::new().unwrap();
        let mut mgr = ArtifactManager::new(temp_dir.path());
        
        assert!(!mgr.has_artifacts("phase1"));
        
        mgr.record("phase1", vec![PathBuf::from("file.txt")]);
        assert!(mgr.has_artifacts("phase1"));
        assert!(!mgr.has_artifacts("phase2"));
    }
    
    #[test]
    fn test_clear() {
        let temp_dir = TempDir::new().unwrap();
        let mut mgr = ArtifactManager::new(temp_dir.path());
        
        mgr.record("phase1", vec![PathBuf::from("file.txt")]);
        assert!(mgr.has_artifacts("phase1"));
        
        mgr.clear();
        assert!(!mgr.has_artifacts("phase1"));
    }
}
