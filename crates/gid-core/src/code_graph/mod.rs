//! Code Graph Extraction — extract code dependencies from source files
//!
//! Multi-language support with tree-sitter AST parsing for Python, Rust, and TypeScript.
//! Builds a code structure graph:
//! - Nodes: files, classes/structs/traits, functions/methods
//! - Edges: imports, calls, inherits, defined_in

pub mod types;
pub(crate) mod lang;
mod extract;
mod query;
mod analysis;
mod format;
mod test_analysis;
mod build;
#[cfg(test)]
mod tests;

pub use types::*;
