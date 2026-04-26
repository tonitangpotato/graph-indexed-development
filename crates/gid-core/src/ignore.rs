//! Ignore list support for GID.
//!
//! Similar to .gitignore, allows specifying patterns to skip during extraction.

use std::path::Path;
use std::fs;
use anyhow::Result;
use regex::Regex;

/// Default patterns that are always ignored.
pub const DEFAULT_IGNORES: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "__pycache__",
    "venv",
    ".venv",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".cache",
    ".pytest_cache",
    ".mypy_cache",
    "coverage",
    ".coverage",
    "*.egg-info",
    ".tox",
    ".nox",
    ".idea",
    ".vscode",
    ".DS_Store",
    "Thumbs.db",
    "*.pyc",
    "*.pyo",
    "*.o",
    "*.a",
    "*.so",
    "*.dylib",
    "*.dll",
    "*.class",
    "*.jar",
    "*.log",
    "*.tmp",
    "*.temp",
    "*.swp",
    "*.swo",
    "*~",
    "vendor",
    "deps",
    "_deps",
    "CMakeFiles",
    "cmake-build-*",
];

/// A compiled ignore pattern.
#[derive(Debug, Clone)]
pub struct IgnorePattern {
    /// Original pattern string
    pub pattern: String,
    /// Whether this is a negation pattern (starts with !)
    pub negated: bool,
    /// Whether this matches only directories (ends with /)
    pub dir_only: bool,
    /// Compiled regex for matching
    regex: Regex,
}

impl IgnorePattern {
    /// Create a new ignore pattern from a gitignore-style pattern string.
    pub fn new(pattern: &str) -> Result<Self> {
        let pattern = pattern.trim();
        
        // Handle negation
        let (negated, pattern) = if pattern.starts_with('!') {
            (true, &pattern[1..])
        } else {
            (false, pattern)
        };
        
        // Handle directory-only pattern
        let (dir_only, pattern) = if pattern.ends_with('/') {
            (true, &pattern[..pattern.len() - 1])
        } else {
            (false, pattern)
        };
        
        // Convert gitignore pattern to regex
        let regex_pattern = gitignore_to_regex(pattern);
        let regex = Regex::new(&regex_pattern)?;
        
        Ok(Self {
            pattern: pattern.to_string(),
            negated,
            dir_only,
            regex,
        })
    }
    
    /// Check if this pattern matches a path.
    pub fn matches(&self, path: &str, is_dir: bool) -> bool {
        // Directory-only patterns don't match files
        if self.dir_only && !is_dir {
            return false;
        }
        
        // Try matching against the full path and just the filename
        let filename = Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        
        self.regex.is_match(path) || self.regex.is_match(&filename)
    }
}

/// A set of ignore patterns.
#[derive(Debug, Clone, Default)]
pub struct IgnoreList {
    patterns: Vec<IgnorePattern>,
}

impl IgnoreList {
    /// Create a new empty ignore list.
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Create an ignore list with default patterns.
    pub fn with_defaults() -> Self {
        let mut list = Self::new();
        for pattern in DEFAULT_IGNORES {
            if let Ok(p) = IgnorePattern::new(pattern) {
                list.patterns.push(p);
            }
        }
        list
    }
    
    /// Add a pattern to the ignore list.
    pub fn add(&mut self, pattern: &str) -> Result<()> {
        let pattern = IgnorePattern::new(pattern)?;
        self.patterns.push(pattern);
        Ok(())
    }
    
    /// Add multiple patterns.
    pub fn add_patterns(&mut self, patterns: &[&str]) -> Result<()> {
        for pattern in patterns {
            self.add(pattern)?;
        }
        Ok(())
    }
    
    /// Check if a path should be ignored.
    pub fn should_ignore(&self, path: &str, is_dir: bool) -> bool {
        let mut ignored = false;
        
        for pattern in &self.patterns {
            if pattern.matches(path, is_dir) {
                ignored = !pattern.negated;
            }
        }
        
        ignored
    }
    
    /// Check if a path should be ignored (convenience method for paths).
    pub fn is_ignored(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        let is_dir = path.is_dir();
        self.should_ignore(&path_str, is_dir)
    }
    
    /// Get all patterns.
    pub fn patterns(&self) -> &[IgnorePattern] {
        &self.patterns
    }
}

/// Load ignore patterns from a .gidignore file.
pub fn load_ignore_list(project_dir: &Path) -> IgnoreList {
    let mut list = IgnoreList::with_defaults();
    
    // Try to load .gidignore
    let gidignore_path = project_dir.join(".gidignore");
    if let Ok(content) = fs::read_to_string(&gidignore_path) {
        parse_ignore_file(&content, &mut list);
    }
    
    // Also respect .gitignore if it exists
    let gitignore_path = project_dir.join(".gitignore");
    if let Ok(content) = fs::read_to_string(&gitignore_path) {
        parse_ignore_file(&content, &mut list);
    }
    
    list
}

/// Parse an ignore file content and add patterns to the list.
fn parse_ignore_file(content: &str, list: &mut IgnoreList) {
    for line in content.lines() {
        let line = line.trim();
        
        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        
        // Add pattern (ignore errors for invalid patterns)
        let _ = list.add(line);
    }
}

/// Convert a gitignore-style pattern to a regex pattern.
fn gitignore_to_regex(pattern: &str) -> String {
    let mut regex = String::new();
    let mut chars = pattern.chars().peekable();
    
    // Patterns starting with / are anchored to the root
    let anchored = pattern.starts_with('/');
    if anchored {
        regex.push('^');
        chars.next(); // Skip the leading /
    }
    
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    // ** matches everything including /
                    chars.next();
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        regex.push_str("(.*/)?");
                    } else {
                        regex.push_str(".*");
                    }
                } else {
                    // * matches everything except /
                    regex.push_str("[^/]*");
                }
            }
            '?' => {
                // ? matches any single character except /
                regex.push_str("[^/]");
            }
            '[' => {
                // Character class - pass through
                regex.push('[');
                while let Some(c) = chars.next() {
                    if c == ']' {
                        regex.push(']');
                        break;
                    }
                    if c == '\\' {
                        regex.push('\\');
                        if let Some(escaped) = chars.next() {
                            regex.push(escaped);
                        }
                    } else {
                        regex.push(c);
                    }
                }
            }
            '\\' => {
                // Escape next character
                regex.push('\\');
                if let Some(escaped) = chars.next() {
                    regex.push(escaped);
                }
            }
            '.' | '+' | '^' | '$' | '(' | ')' | '{' | '}' | '|' => {
                // Escape regex special characters
                regex.push('\\');
                regex.push(c);
            }
            _ => {
                regex.push(c);
            }
        }
    }
    
    // Add end anchor if pattern doesn't contain /
    if !pattern.contains('/') {
        // Pattern should match at any level
        regex = format!("(^|/){}", regex);
    }
    
    regex.push('$');
    regex
}

/// Check if a path component should be ignored (quick check for common patterns).
pub fn is_common_ignore(name: &str) -> bool {
    matches!(
        name,
        "node_modules" | "target" | ".git" | "__pycache__" | 
        "venv" | ".venv" | "dist" | "build" | ".next" | ".nuxt" |
        ".cache" | ".pytest_cache" | ".mypy_cache" | "coverage" |
        ".idea" | ".vscode" | ".DS_Store" | "vendor" | "deps"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_pattern_simple() {
        let pattern = IgnorePattern::new("node_modules").unwrap();
        assert!(pattern.matches("node_modules", true));
        assert!(pattern.matches("foo/node_modules", true));
        assert!(!pattern.matches("my_node_modules", true));
    }
    
    #[test]
    fn test_pattern_wildcard() {
        let pattern = IgnorePattern::new("*.pyc").unwrap();
        assert!(pattern.matches("foo.pyc", false));
        assert!(pattern.matches("bar/foo.pyc", false));
        assert!(!pattern.matches("foo.py", false));
    }
    
    #[test]
    fn test_pattern_doublestar() {
        let pattern = IgnorePattern::new("**/*.log").unwrap();
        assert!(pattern.matches("foo.log", false));
        assert!(pattern.matches("bar/foo.log", false));
        assert!(pattern.matches("a/b/c/foo.log", false));
    }
    
    #[test]
    fn test_pattern_dir_only() {
        let pattern = IgnorePattern::new("build/").unwrap();
        assert!(pattern.matches("build", true));
        assert!(!pattern.matches("build", false)); // Doesn't match files
    }
    
    #[test]
    fn test_pattern_negation() {
        let mut list = IgnoreList::new();
        list.add("*.log").unwrap();
        list.add("!important.log").unwrap();
        
        assert!(list.should_ignore("debug.log", false));
        assert!(!list.should_ignore("important.log", false));
    }
    
    #[test]
    fn test_default_ignores() {
        let list = IgnoreList::with_defaults();
        
        assert!(list.should_ignore("node_modules", true));
        assert!(list.should_ignore("target", true));
        assert!(list.should_ignore(".git", true));
        assert!(list.should_ignore("__pycache__", true));
        assert!(list.should_ignore("foo.pyc", false));
        
        assert!(!list.should_ignore("src", true));
        assert!(!list.should_ignore("main.rs", false));
    }
    
    #[test]
    fn test_is_common_ignore() {
        assert!(is_common_ignore("node_modules"));
        assert!(is_common_ignore("target"));
        assert!(is_common_ignore(".git"));
        assert!(!is_common_ignore("src"));
        assert!(!is_common_ignore("main.rs"));
    }
}
