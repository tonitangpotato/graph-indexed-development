pub(crate) mod python;
pub(crate) mod rust_lang;
pub(crate) mod typescript;

use std::path::Path;

/// Walk up from `dir` to find the project root by looking for config files.
/// Looks for: tsconfig.json, package.json, Cargo.toml, pyproject.toml, .git
pub(crate) fn find_project_root(dir: &Path) -> std::path::PathBuf {
    let markers = ["tsconfig.json", "package.json", "Cargo.toml", "pyproject.toml", ".git"];
    let abs_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

    let mut current = abs_dir.as_path();
    loop {
        for marker in &markers {
            if current.join(marker).exists() {
                return current.to_path_buf();
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }
    // Fallback: use the original directory
    abs_dir
}

/// Find the column position of a function call in a line of source code.
/// Looks for patterns like `callee_name(` or `.callee_name(`.
pub(crate) fn find_call_position(line: &str, callee_name: &str) -> Option<usize> {
    // Look for `callee_name(` pattern
    let pattern = format!("{}(", callee_name);
    if let Some(pos) = line.find(&pattern) {
        return Some(pos);
    }

    // Look for `.callee_name(` pattern (method call)
    let dot_pattern = format!(".{}(", callee_name);
    if let Some(pos) = line.find(&dot_pattern) {
        // Return position of the method name, not the dot
        return Some(pos + 1);
    }

    None
}
