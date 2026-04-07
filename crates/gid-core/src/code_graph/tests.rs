//! Tests for code graph extraction, call analysis, and path resolution.

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use tree_sitter::Parser;
    use crate::code_graph::types::*;
    use crate::code_graph::lang::python::*;
    use crate::code_graph::lang::rust_lang::*;
    use crate::code_graph::lang::typescript::*;
    #[allow(unused_imports)]
    use crate::code_graph::lang::find_project_root;

    #[test]
    fn test_extract_python() {
        let content = r#"
import os
from pathlib import Path

class MyClass(BaseClass):
    def method(self):
        pass

def top_level():
    pass
"#;
        let mut parser = Parser::new();
        let language = tree_sitter_python::LANGUAGE;
        parser.set_language(&language.into()).unwrap();
        let mut class_map = HashMap::new();

        let (nodes, edges, _) = extract_python_tree_sitter("test.py", content, &mut parser, &mut class_map);

        assert!(nodes.iter().any(|n| n.name == "MyClass"));
        assert!(nodes.iter().any(|n| n.name == "method"));
        assert!(nodes.iter().any(|n| n.name == "top_level"));
        assert!(edges.iter().any(|e| e.to.contains("BaseClass")));
    }

    #[test]
    fn test_extract_rust() {
        let content = r#"
use std::path::Path;
use crate::module;

pub struct MyStruct {
    field: i32,
}

impl MyTrait for MyStruct {
    fn method(&self) {}
}

pub fn top_level() {}
"#;
        let mut parser = Parser::new();
        let mut class_map = HashMap::new();
        let (nodes, edges, _, _) = extract_rust_tree_sitter("test.rs", content, &mut parser, &mut class_map);

        assert!(nodes.iter().any(|n| n.name == "MyStruct"), "Should find MyStruct");
        assert!(nodes.iter().any(|n| n.name == "method"), "Should find method");
        assert!(nodes.iter().any(|n| n.name == "top_level"), "Should find top_level");
        assert!(edges.iter().any(|e| e.to.contains("module")), "Should have module import edge");
        
        assert!(edges.iter().any(|e| e.relation == EdgeRelation::Inherits && e.to.contains("MyTrait")),
            "Should capture trait impl inheritance");
    }

    #[test]
    fn test_extract_rust_comprehensive() {
        let content = r#"
use crate::foo::bar;

/// A documented struct
pub struct Person {
    name: String,
    age: u32,
}

/// A documented enum
pub enum Status {
    Active,
    Inactive,
}

/// A trait
pub trait Greeter {
    fn greet(&self) -> String;
}

impl Greeter for Person {
    fn greet(&self) -> String {
        format!("Hello, {}", self.name)
    }
}

impl Person {
    pub fn new(name: String) -> Self {
        Self { name, age: 0 }
    }
    
    pub fn birthday(&mut self) {
        self.age += 1;
    }
}

mod inner {
    pub fn nested_fn() {}
}

type MyAlias = Vec<String>;

pub fn standalone() {}

#[test]
fn test_something() {}
"#;
        let mut parser = Parser::new();
        let mut class_map = HashMap::new();
        let (nodes, edges, _, _) = extract_rust_tree_sitter("test.rs", content, &mut parser, &mut class_map);

        // Structs and enums
        assert!(nodes.iter().any(|n| n.name == "Person"), "Should find Person struct");
        assert!(nodes.iter().any(|n| n.name == "Status"), "Should find Status enum");
        
        // Traits
        assert!(nodes.iter().any(|n| n.name == "Greeter"), "Should find Greeter trait");
        
        // Methods from impl blocks
        assert!(nodes.iter().any(|n| n.name == "greet"), "Should find greet method");
        assert!(nodes.iter().any(|n| n.name == "new"), "Should find new method");
        assert!(nodes.iter().any(|n| n.name == "birthday"), "Should find birthday method");
        
        // Nested module functions
        assert!(nodes.iter().any(|n| n.name.contains("nested_fn")), "Should find nested_fn");
        
        // Type aliases
        assert!(nodes.iter().any(|n| n.name == "MyAlias"), "Should find type alias");
        
        // Standalone function
        assert!(nodes.iter().any(|n| n.name == "standalone"), "Should find standalone fn");
        
        // Test function should be marked as test
        let test_node = nodes.iter().find(|n| n.name == "test_something");
        assert!(test_node.is_some(), "Should find test function");
        assert!(test_node.unwrap().is_test, "Test function should be marked as test");
        
        // Methods should be linked to their impl target
        let greet_edges: Vec<_> = edges.iter()
            .filter(|e| e.from.contains("greet") && e.relation == EdgeRelation::DefinedIn)
            .collect();
        assert!(!greet_edges.is_empty(), "greet should have DefinedIn edge");
    }

    #[test]
    fn test_extract_typescript() {
        let content = r#"
import { Component } from './component';

export class MyClass extends BaseClass {
    method(): void {}
}

export function topLevel(): void {}

export const arrowFn = () => {};
"#;
        let mut parser = Parser::new();
        let mut class_map = HashMap::new();
        let (nodes, edges, _) = extract_typescript_tree_sitter("test.ts", content, &mut parser, &mut class_map, "ts");

        assert!(nodes.iter().any(|n| n.name == "MyClass"), "Should find MyClass");
        assert!(nodes.iter().any(|n| n.name == "topLevel"), "Should find topLevel");
        assert!(nodes.iter().any(|n| n.name == "arrowFn"), "Should find arrowFn");
        assert!(edges.iter().any(|e| e.to.contains("component")), "Should have component import");
        
        assert!(nodes.iter().any(|n| n.name == "method"), "Should find method inside class");
        
        assert!(edges.iter().any(|e| e.relation == EdgeRelation::Inherits && e.to.contains("BaseClass")),
            "Should capture class inheritance");
    }

    #[test]
    fn test_extract_typescript_comprehensive() {
        let content = r#"
import { Injectable } from '@angular/core';
import type { User } from './types';

/**
 * A service class
 */
@Injectable()
export class UserService {
    private users: User[] = [];
    
    /**
     * Get all users
     */
    getUsers(): User[] {
        return this.users;
    }
    
    addUser(user: User): void {
        this.users.push(user);
    }
}

export interface IRepository<T> {
    find(id: string): T | undefined;
    save(item: T): void;
}

export type UserId = string;

export enum UserRole {
    Admin = 'admin',
    User = 'user',
}

export function createUser(name: string): User {
    return { name };
}

export const fetchUser = async (id: string) => {
    return null;
};

export default class DefaultExport {}

namespace MyNamespace {
    export function innerFn() {}
}
"#;
        let mut parser = Parser::new();
        let mut class_map = HashMap::new();
        let (nodes, edges, _) = extract_typescript_tree_sitter("test.ts", content, &mut parser, &mut class_map, "ts");

        assert!(nodes.iter().any(|n| n.name == "UserService"), "Should find UserService class");
        assert!(nodes.iter().any(|n| n.name == "DefaultExport"), "Should find default export class");
        assert!(nodes.iter().any(|n| n.name == "getUsers"), "Should find getUsers method");
        assert!(nodes.iter().any(|n| n.name == "addUser"), "Should find addUser method");
        assert!(nodes.iter().any(|n| n.name == "IRepository"), "Should find interface");
        assert!(nodes.iter().any(|n| n.name == "UserId"), "Should find type alias");
        assert!(nodes.iter().any(|n| n.name == "UserRole"), "Should find enum");
        assert!(nodes.iter().any(|n| n.name == "createUser"), "Should find function");
        assert!(nodes.iter().any(|n| n.name == "fetchUser"), "Should find arrow function");
        assert!(nodes.iter().any(|n| n.name == "MyNamespace"), "Should find namespace");
        assert!(edges.iter().any(|e| e.relation == EdgeRelation::Imports), "Should have import edges");
    }

    #[test]
    fn test_rust_call_extraction() {
        let content = r#"
pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new() -> Self {
        Self { value: 0 }
    }
    
    pub fn add(&mut self, x: i32) {
        self.value += x;
        self.log_operation("add");
    }
    
    fn log_operation(&self, op: &str) {
        helper_fn(op);
    }
}

fn helper_fn(msg: &str) {
    println!("{}", msg);
}

pub fn create_and_use() {
    let mut calc = Calculator::new();
    calc.add(5);
    helper_fn("done");
}
"#;
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        
        let mut class_map = HashMap::new();
        let (nodes, mut edges, _, _) = extract_rust_tree_sitter("calc.rs", content, &mut parser, &mut class_map);

        let func_map: HashMap<String, Vec<String>> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .fold(HashMap::new(), |mut acc, n| {
                acc.entry(n.name.clone()).or_default().push(n.id.clone());
                acc
            });

        let method_to_class: HashMap<String, String> = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::DefinedIn && e.to.starts_with("class:"))
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();

        let file_func_ids: HashSet<String> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.id.clone())
            .collect();

        let node_pkg_map: HashMap<String, String> = nodes
            .iter()
            .map(|n| (n.id.clone(), "".to_string()))
            .collect();

        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();
        
        extract_calls_rust(
            root,
            content.as_bytes(),
            "calc.rs",
            &func_map,
            &method_to_class,
            &file_func_ids,
            &node_pkg_map,
            &HashMap::new(),
            &HashMap::new(),
            &mut edges,
        );

        let call_edges: Vec<_> = edges.iter()
            .filter(|e| e.relation == EdgeRelation::Calls)
            .collect();
        
        assert!(!call_edges.is_empty(), "Should have call edges");
        
        assert!(
            call_edges.iter().any(|e| e.from.contains("create_and_use") && e.to.contains("helper_fn")),
            "create_and_use should call helper_fn"
        );
        
        assert!(
            call_edges.iter().any(|e| e.from.contains("log_operation") && e.to.contains("helper_fn")),
            "log_operation should call helper_fn"
        );
    }

    #[test]
    fn test_typescript_call_extraction() {
        let content = r#"
export class UserService {
    private helper: Helper;
    
    constructor() {
        this.helper = new Helper();
    }
    
    getUser(id: string) {
        return this.fetchFromDb(id);
    }
    
    private fetchFromDb(id: string) {
        return formatUser(this.helper.query(id));
    }
}

function formatUser(data: any) {
    return processData(data);
}

function processData(data: any) {
    return data;
}

class Helper {
    query(id: string) {
        return null;
    }
}
"#;
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).unwrap();
        
        let mut class_map = HashMap::new();
        let (nodes, mut edges, imports) = extract_typescript_tree_sitter("user.ts", content, &mut parser, &mut class_map, "ts");

        let func_map: HashMap<String, Vec<String>> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .fold(HashMap::new(), |mut acc, n| {
                acc.entry(n.name.clone()).or_default().push(n.id.clone());
                acc
            });

        let method_to_class: HashMap<String, String> = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::DefinedIn && e.to.starts_with("class:"))
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();

        let file_func_ids: HashSet<String> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.id.clone())
            .collect();

        let mut file_imported_names: HashMap<String, HashSet<String>> = HashMap::new();
        file_imported_names.insert("user.ts".to_string(), imports);

        let node_pkg_map: HashMap<String, String> = nodes
            .iter()
            .map(|n| (n.id.clone(), "".to_string()))
            .collect();

        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();
        
        extract_calls_typescript(
            root,
            content.as_bytes(),
            "user.ts",
            &func_map,
            &method_to_class,
            &file_func_ids,
            &file_imported_names,
            &node_pkg_map,
            &mut edges,
        );

        let call_edges: Vec<_> = edges.iter()
            .filter(|e| e.relation == EdgeRelation::Calls)
            .collect();
        
        assert!(!call_edges.is_empty(), "Should have call edges");
        
        assert!(
            call_edges.iter().any(|e| e.from.contains("fetchFromDb") && e.to.contains("formatUser")),
            "fetchFromDb should call formatUser"
        );
        
        assert!(
            call_edges.iter().any(|e| e.from.contains("formatUser") && e.to.contains("processData")),
            "formatUser should call processData"
        );
    }

    #[test]
    fn test_resolve_relative_path() {
        assert_eq!(resolve_relative_path("src/pages", "./Dashboard"), "src/pages/Dashboard");
        assert_eq!(resolve_relative_path("src/pages", "../utils/helper"), "src/utils/helper");
        assert_eq!(resolve_relative_path("src/pages/admin", "../../components/Stats"), "src/components/Stats");
        assert_eq!(resolve_relative_path("src/pages", "../../components/Stats"), "components/Stats");
        assert_eq!(resolve_relative_path("", "./foo"), "foo");
        assert_eq!(resolve_relative_path("src", "../lib/util"), "lib/util");
    }

    #[test]
    fn test_normalize_ts_module_path() {
        assert_eq!(normalize_ts_module_path("src/components/Stats.js"), "src.components.Stats");
        assert_eq!(normalize_ts_module_path("src/components/Stats.tsx"), "src.components.Stats");
        assert_eq!(normalize_ts_module_path("src/components/Stats.ts"), "src.components.Stats");
        assert_eq!(normalize_ts_module_path("src/components/Stats.jsx"), "src.components.Stats");
        assert_eq!(normalize_ts_module_path("src/components/Stats"), "src.components.Stats");
    }

    #[test]
    fn test_resolve_ts_import() {
        let mut module_map = HashMap::new();
        module_map.insert("src.components.Stats".to_string(), "file:src/components/Stats.tsx".to_string());
        module_map.insert("src.utils.helper".to_string(), "file:src/utils/helper.ts".to_string());
        module_map.insert("components.Stats".to_string(), "file:src/components/Stats.tsx".to_string());

        let result = resolve_ts_import("src/pages/Dashboard.tsx", "../../components/Stats.js", &module_map);
        assert_eq!(result, Some("file:src/components/Stats.tsx".to_string()), 
            "Should resolve ../../components/Stats.js from src/pages/Dashboard.tsx");

        let result = resolve_ts_import("src/pages/Dashboard.tsx", "../utils/helper", &module_map);
        assert_eq!(result, Some("file:src/utils/helper.ts".to_string()),
            "Should resolve ../utils/helper from src/pages/Dashboard.tsx");

        let mut module_map2 = HashMap::new();
        module_map2.insert("src.pages.local".to_string(), "file:src/pages/local.ts".to_string());
        let result = resolve_ts_import("src/pages/Dashboard.tsx", "./local", &module_map2);
        assert_eq!(result, Some("file:src/pages/local.ts".to_string()),
            "Should resolve ./local from src/pages/Dashboard.tsx");

        let result = resolve_ts_import("src/pages/Dashboard.tsx", "lodash", &module_map);
        assert_eq!(result, None, "Non-relative imports should return None");
    }

    #[test]
    fn test_resolve_ts_import_path_alias() {
        let mut module_map = HashMap::new();
        module_map.insert("src.components.Stats".to_string(), "file:src/components/Stats.tsx".to_string());

        let result = resolve_ts_import("src/pages/Dashboard.tsx", "@/components/Stats", &module_map);
        assert_eq!(result, Some("file:src/components/Stats.tsx".to_string()),
            "Should resolve @/components/Stats path alias");
    }

    /// ISS-007: Verify no ghost nodes when same-named files exist in different directories.
    #[test]
    fn test_no_ghost_nodes_same_basename() {
        use std::io::Write;
        use crate::code_graph::CodeGraph;

        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");

        // Create two files with the same basename in different directories
        std::fs::create_dir_all(src.join("components")).unwrap();

        let mut f1 = std::fs::File::create(src.join("Tool.ts")).unwrap();
        writeln!(f1, "export class Tool {{ run() {{}} }}").unwrap();

        let mut f2 = std::fs::File::create(src.join("components/Tool.ts")).unwrap();
        writeln!(f2, "export class ToolComponent {{ render() {{}} }}").unwrap();

        // A file that imports "Tool" — which one should it resolve to?
        let mut f3 = std::fs::File::create(src.join("main.ts")).unwrap();
        writeln!(f3, "import {{ Tool }} from './Tool';").unwrap();
        writeln!(f3, "export function main() {{ const t = new Tool(); return t; }}").unwrap();

        let graph = CodeGraph::extract_from_dir(&src);

        // Count file nodes
        let file_nodes: Vec<&CodeNode> = graph.nodes.iter()
            .filter(|n| n.kind == NodeKind::File)
            .collect();

        let actual_files = vec!["Tool.ts", "components/Tool.ts", "main.ts"];

        // There should be exactly 3 file nodes — no ghosts
        assert_eq!(
            file_nodes.len(), actual_files.len(),
            "Expected {} file nodes but got {}. File nodes: {:?}",
            actual_files.len(),
            file_nodes.len(),
            file_nodes.iter().map(|n| &n.file_path).collect::<Vec<_>>()
        );

        // Every file node's file_path should be one of the actual files
        for node in &file_nodes {
            assert!(
                actual_files.contains(&node.file_path.as_str()),
                "Ghost file node detected: {} (file_path: {})",
                node.id, node.file_path
            );
        }

        // No edges should reference non-existent nodes
        let node_ids: HashSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        for edge in &graph.edges {
            assert!(
                node_ids.contains(edge.from.as_str()),
                "Edge from non-existent node: {} -> {}",
                edge.from, edge.to
            );
            assert!(
                node_ids.contains(edge.to.as_str()),
                "Edge to non-existent node: {} -> {}",
                edge.from, edge.to
            );
        }
    }

    // ═══ ISS-009 Cross-Layer Tests ═══

    #[test]
    fn test_module_generation_flat() {
        // Flat directory → one module node per dir
        let files = vec![
            ("src/main.rs".to_string(), "fn main() {}".to_string(), Language::Rust),
            ("src/lib.rs".to_string(), "pub mod auth;".to_string(), Language::Rust),
        ];
        let (nodes, edges) = super::super::extract::generate_module_nodes_pub(&files);

        assert_eq!(nodes.len(), 1, "Should have one module: src");
        assert_eq!(nodes[0].id, "module:src");
        assert_eq!(nodes[0].kind, NodeKind::Module);
        assert_eq!(nodes[0].name, "src");
        assert!(edges.is_empty(), "No parent module → no belongs_to edge");
    }

    #[test]
    fn test_module_generation_nested() {
        // Nested dirs → hierarchical belongs_to edges
        let files = vec![
            ("src/auth/mod.rs".to_string(), "".to_string(), Language::Rust),
            ("src/auth/middleware.rs".to_string(), "".to_string(), Language::Rust),
            ("src/main.rs".to_string(), "".to_string(), Language::Rust),
        ];
        let (nodes, edges) = super::super::extract::generate_module_nodes_pub(&files);

        assert!(nodes.len() >= 2, "Should have module:src and module:src/auth");
        assert!(nodes.iter().any(|n| n.id == "module:src"));
        assert!(nodes.iter().any(|n| n.id == "module:src/auth"));

        // src/auth → belongs_to → src
        assert!(edges.iter().any(|e|
            e.from == "module:src/auth" && e.to == "module:src" && e.relation == EdgeRelation::BelongsTo
        ), "src/auth should belong_to src");
    }

    #[test]
    fn test_module_generation_empty_dir() {
        // No source files → no module nodes
        let files: Vec<(String, String, Language)> = vec![];
        let (nodes, edges) = super::super::extract::generate_module_nodes_pub(&files);
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn test_file_belongs_to_module() {
        // Each non-root file has belongs_to edge to its directory's module
        let files = vec![
            ("src/auth/middleware.rs".to_string(), "".to_string(), Language::Rust),
            ("src/main.rs".to_string(), "".to_string(), Language::Rust),
        ];
        let edges = super::super::extract::generate_file_to_module_edges_pub(&files);

        assert!(edges.iter().any(|e|
            e.from == "file:src/auth/middleware.rs"
            && e.to == "module:src/auth"
            && e.relation == EdgeRelation::BelongsTo
        ));
        assert!(edges.iter().any(|e|
            e.from == "file:src/main.rs"
            && e.to == "module:src"
            && e.relation == EdgeRelation::BelongsTo
        ));
    }

    #[test]
    fn test_root_file_no_belongs_to() {
        // File at project root → no belongs_to edge
        let files = vec![
            ("main.rs".to_string(), "".to_string(), Language::Rust),
        ];
        let edges = super::super::extract::generate_file_to_module_edges_pub(&files);
        assert!(edges.is_empty(), "Root file should not have belongs_to edge");
    }

    #[test]
    fn test_nodekind_module_serde_roundtrip() {
        let node = CodeNode::new_module("src/auth");
        let yaml = serde_yaml::to_string(&node).unwrap();
        let deserialized: CodeNode = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.kind, NodeKind::Module);
        assert_eq!(deserialized.id, "module:src/auth");
        assert_eq!(deserialized.name, "auth");
    }

    #[test]
    fn test_edge_relation_belongs_to_roundtrip() {
        let edge = CodeEdge::new("file:src/main.rs", "module:src", EdgeRelation::BelongsTo);
        let yaml = serde_yaml::to_string(&edge).unwrap();
        let deserialized: CodeEdge = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.relation, EdgeRelation::BelongsTo);
        assert_eq!(deserialized.from, "file:src/main.rs");
        assert_eq!(deserialized.to, "module:src");
    }

    #[test]
    fn test_rust_tests_for_matching() {
        // tests/auth.rs → file:src/auth.rs
        let files = vec![
            ("src/auth.rs".to_string(), "".to_string(), Language::Rust),
            ("tests/auth.rs".to_string(), "".to_string(), Language::Rust),
        ];
        let edges = super::super::extract::generate_rust_tests_for_edges_pub(&files);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "file:tests/auth.rs");
        assert_eq!(edges[0].to, "file:src/auth.rs");
        assert_eq!(edges[0].relation, EdgeRelation::TestsFor);
        assert!((edges[0].confidence - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_rust_tests_for_test_prefix() {
        // tests/test_auth.rs → file:src/auth.rs
        let files = vec![
            ("src/auth.rs".to_string(), "".to_string(), Language::Rust),
            ("tests/test_auth.rs".to_string(), "".to_string(), Language::Rust),
        ];
        let edges = super::super::extract::generate_rust_tests_for_edges_pub(&files);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "file:src/auth.rs");
    }

    #[test]
    fn test_rust_tests_for_mod() {
        // tests/auth.rs → file:src/auth/mod.rs
        let files = vec![
            ("src/auth/mod.rs".to_string(), "".to_string(), Language::Rust),
            ("tests/auth.rs".to_string(), "".to_string(), Language::Rust),
        ];
        let edges = super::super::extract::generate_rust_tests_for_edges_pub(&files);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "file:src/auth/mod.rs");
    }

    #[test]
    fn test_ts_test_file_matching() {
        // auth.test.ts → file:auth.ts
        let files = vec![
            ("auth.ts".to_string(), "".to_string(), Language::TypeScript),
            ("auth.test.ts".to_string(), "".to_string(), Language::TypeScript),
        ];
        let edges = super::super::extract::generate_ts_tests_for_edges_pub(&files);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "file:auth.test.ts");
        assert_eq!(edges[0].to, "file:auth.ts");
        assert_eq!(edges[0].relation, EdgeRelation::TestsFor);
    }

    #[test]
    fn test_ts_spec_file_matching() {
        // auth.spec.ts → file:auth.ts
        let files = vec![
            ("auth.ts".to_string(), "".to_string(), Language::TypeScript),
            ("auth.spec.ts".to_string(), "".to_string(), Language::TypeScript),
        ];
        let edges = super::super::extract::generate_ts_tests_for_edges_pub(&files);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to, "file:auth.ts");
    }

    #[test]
    fn test_code_graph_impact_relation_filter() {
        // Build a small graph with multiple relation types
        let mut graph = CodeGraph::default();
        graph.nodes.push(CodeNode::new_file("src/a.rs"));
        graph.nodes.push(CodeNode::new_file("src/b.rs"));
        graph.nodes.push(CodeNode::new_file("tests/a.rs"));
        graph.edges.push(CodeEdge::new("file:src/b.rs", "file:src/a.rs", EdgeRelation::Imports));
        graph.edges.push(CodeEdge::new_heuristic("file:tests/a.rs", "file:src/a.rs", EdgeRelation::TestsFor, 0.8));
        graph.build_indexes();

        // Unfiltered: both b.rs and tests/a.rs are impacted
        let all = graph.get_impact("file:src/a.rs");
        assert_eq!(all.len(), 2);

        // Filter to Imports only: only b.rs
        let imports_only = graph.get_impact_filtered(
            "file:src/a.rs",
            Some(&[EdgeRelation::Imports]),
        );
        assert_eq!(imports_only.len(), 1);
        assert_eq!(imports_only[0].id, "file:src/b.rs");

        // Filter to TestsFor only: only tests/a.rs
        let tests_only = graph.get_impact_filtered(
            "file:src/a.rs",
            Some(&[EdgeRelation::TestsFor]),
        );
        assert_eq!(tests_only.len(), 1);
        assert_eq!(tests_only[0].id, "file:tests/a.rs");
    }

    #[test]
    fn test_new_heuristic_constructor() {
        let edge = CodeEdge::new_heuristic("a", "b", EdgeRelation::TestsFor, 0.8);
        assert_eq!(edge.confidence, 0.8);
        assert_eq!(edge.relation, EdgeRelation::TestsFor);
        assert!(edge.call_site_line.is_none());
        assert!(edge.call_site_column.is_none());
    }

    #[test]
    fn test_ts_nested_tests_dir() {
        // FINDING-6: __tests__/Button.test.tsx → src/components/Button.tsx
        let entries = vec![
            ("src/components/Button.tsx".to_string(), "export {}".to_string(), Language::TypeScript),
            ("src/components/__tests__/Button.test.tsx".to_string(), "test('btn', () => {})".to_string(), Language::TypeScript),
        ];
        let edges = super::super::extract::generate_ts_tests_for_edges_pub(&entries);
        assert_eq!(edges.len(), 1, "Should match nested __tests__ to source: {:?}", edges);
        assert_eq!(edges[0].from, "file:src/components/__tests__/Button.test.tsx");
        assert_eq!(edges[0].to, "file:src/components/Button.tsx");
        assert_eq!(edges[0].relation, EdgeRelation::TestsFor);
        assert_eq!(edges[0].confidence, 0.8);
    }

    #[test]
    fn test_python_tests_for_naming() {
        let entries = vec![
            ("auth.py".to_string(), "class Auth: pass".to_string(), Language::Python),
            ("tests/test_auth.py".to_string(), "def test_auth(): pass".to_string(), Language::Python),
        ];
        let edges = super::super::extract::generate_python_tests_for_edges_pub(&entries);
        assert_eq!(edges.len(), 1, "Should match test_auth.py → auth.py: {:?}", edges);
        assert_eq!(edges[0].from, "file:tests/test_auth.py");
        assert_eq!(edges[0].to, "file:auth.py");
        assert_eq!(edges[0].relation, EdgeRelation::TestsFor);
    }

    #[test]
    fn test_python_tests_for_no_self_match() {
        // Test files should not match themselves
        let entries = vec![
            ("test_auth.py".to_string(), "def test_auth(): pass".to_string(), Language::Python),
        ];
        let edges = super::super::extract::generate_python_tests_for_edges_pub(&entries);
        assert_eq!(edges.len(), 0, "Should not self-match test file");
    }

    #[test]
    fn test_edge_relation_from_str() {
        use std::str::FromStr;
        assert_eq!(EdgeRelation::from_str("imports").unwrap(), EdgeRelation::Imports);
        assert_eq!(EdgeRelation::from_str("belongs_to").unwrap(), EdgeRelation::BelongsTo);
        assert_eq!(EdgeRelation::from_str("tests_for").unwrap(), EdgeRelation::TestsFor);
        assert_eq!(EdgeRelation::from_str("CALLS").unwrap(), EdgeRelation::Calls);
        assert!(EdgeRelation::from_str("nonexistent").is_err());
    }

    #[test]
    fn test_scope_map_preserves_module_prefix_for_test_functions() {
        // Bug 1: build_scope_map_rust must include module prefix (e.g. "tests::") 
        // so that scope IDs match the node IDs created by extract_rust_node.
        let content = r#"
fn bar() {}

mod tests {
    use super::*;

    fn test_foo() {
        bar();
    }
}
"#;
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();

        let mut class_map = HashMap::new();
        let (nodes, mut edges, _, _) = extract_rust_tree_sitter("test.rs", content, &mut parser, &mut class_map);

        // Node extraction should create "func:test.rs:tests::test_foo"
        assert!(
            nodes.iter().any(|n| n.id == "func:test.rs:tests::test_foo"),
            "Should have node with tests:: prefix, got: {:?}",
            nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
        );

        // Now extract calls — the scope map should also use "tests::" prefix
        let func_map: HashMap<String, Vec<String>> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .fold(HashMap::new(), |mut acc, n| {
                acc.entry(n.name.clone()).or_default().push(n.id.clone());
                acc
            });

        let method_to_class: HashMap<String, String> = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::DefinedIn && e.to.starts_with("class:"))
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();

        let file_func_ids: HashSet<String> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.id.clone())
            .collect();

        let node_pkg_map: HashMap<String, String> = nodes
            .iter()
            .map(|n| (n.id.clone(), "".to_string()))
            .collect();

        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();

        let file_imported_names: HashMap<String, HashSet<String>> = HashMap::new();
        let struct_field_types: HashMap<String, HashMap<String, String>> = HashMap::new();

        extract_calls_rust(
            root,
            content.as_bytes(),
            "test.rs",
            &func_map,
            &method_to_class,
            &file_func_ids,
            &node_pkg_map,
            &file_imported_names,
            &struct_field_types,
            &mut edges,
        );

        let call_edges: Vec<_> = edges.iter()
            .filter(|e| e.relation == EdgeRelation::Calls)
            .collect();

        // The call edge from test_foo → bar should have from = "func:test.rs:tests::test_foo"
        assert!(
            call_edges.iter().any(|e| e.from == "func:test.rs:tests::test_foo" && e.to.contains("bar")),
            "Call edge from tests::test_foo to bar should use tests:: prefix. Edges: {:?}",
            call_edges.iter().map(|e| (&e.from, &e.to)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_nested_functions_do_not_create_dangling_scope_entries() {
        // Bug 2: inner/nested functions should not get their own scope entries,
        // since extract_rust_node doesn't create nodes for them.
        // Calls inside nested functions should be attributed to the enclosing function.
        let content = r#"
fn bar() {}

fn main() {
    fn inner() {
        bar();
    }
    inner();
}
"#;
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();

        let mut class_map = HashMap::new();
        let (nodes, mut edges, _, _) = extract_rust_tree_sitter("test.rs", content, &mut parser, &mut class_map);

        // extract_rust_node should NOT create a node for "inner" (it's nested)
        assert!(
            !nodes.iter().any(|n| n.name == "inner"),
            "Should NOT create node for nested function 'inner'"
        );

        // Build call edges
        let func_map: HashMap<String, Vec<String>> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .fold(HashMap::new(), |mut acc, n| {
                acc.entry(n.name.clone()).or_default().push(n.id.clone());
                acc
            });

        let method_to_class: HashMap<String, String> = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::DefinedIn && e.to.starts_with("class:"))
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();

        let file_func_ids: HashSet<String> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.id.clone())
            .collect();

        let node_pkg_map: HashMap<String, String> = nodes
            .iter()
            .map(|n| (n.id.clone(), "".to_string()))
            .collect();

        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();

        let file_imported_names: HashMap<String, HashSet<String>> = HashMap::new();
        let struct_field_types: HashMap<String, HashMap<String, String>> = HashMap::new();

        extract_calls_rust(
            root,
            content.as_bytes(),
            "test.rs",
            &func_map,
            &method_to_class,
            &file_func_ids,
            &node_pkg_map,
            &file_imported_names,
            &struct_field_types,
            &mut edges,
        );

        let call_edges: Vec<_> = edges.iter()
            .filter(|e| e.relation == EdgeRelation::Calls)
            .collect();

        // All call edges should have 'from' IDs that correspond to existing nodes
        let node_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        for edge in &call_edges {
            assert!(
                node_ids.contains(edge.from.as_str()),
                "Call edge 'from' should match an existing node. Dangling from: {}, to: {}",
                edge.from, edge.to
            );
        }

        // The call to bar() from inside inner() should be attributed to main
        assert!(
            call_edges.iter().any(|e| e.from == "func:test.rs:main" && e.to.contains("bar")),
            "Call to bar() from nested inner() should be attributed to main. Edges: {:?}",
            call_edges.iter().map(|e| (&e.from, &e.to)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_scope_map_with_cfg_test_attribute() {
        // Real-world pattern: #[cfg(test)] mod tests { ... }
        // Verifies that #[cfg(test)] attribute doesn't break module prefix tracking
        let content = r#"
fn bar() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitizer_detect_system_injection() {
        bar();
    }
}
"#;
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        
        use crate::code_graph::lang::rust_lang::build_scope_map_rust;
        let tree = parser.parse(content, None).unwrap();
        let root = tree.root_node();
        
        let mut scope_map: Vec<(usize, usize, String, Option<String>)> = Vec::new();
        build_scope_map_rust(root, content.as_bytes(), "safety.rs", &mut scope_map);
        
        // The test function should have tests:: prefix
        assert!(
            scope_map.iter().any(|(_, _, id, _)| id == "func:safety.rs:tests::test_sanitizer_detect_system_injection"),
            "Should have scope entry with tests:: prefix, got: {:?}",
            scope_map.iter().map(|(_, _, id, _)| id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_remap_cross_file_impl_defined_in_edges() {
        use crate::code_graph::extract::remap_cross_file_impl_edges;
        use crate::code_graph::types::*;

        // Simulate: struct CodeGraph defined in extract.rs, impl CodeGraph in format.rs
        let nodes = vec![
            CodeNode {
                id: "class:code_graph/extract.rs:CodeGraph".to_string(),
                name: "CodeGraph".to_string(),
                kind: NodeKind::Class,
                file_path: "code_graph/extract.rs".to_string(),
                line: None,
                decorators: vec![],
                signature: None,
                docstring: None,
                line_count: 0,
                is_test: false,
            },
            CodeNode {
                id: "method:code_graph/format.rs:CodeGraph.format_for_llm".to_string(),
                name: "format_for_llm".to_string(),
                kind: NodeKind::Function,
                file_path: "code_graph/format.rs".to_string(),
                line: None,
                decorators: vec![],
                signature: None,
                docstring: None,
                line_count: 0,
                is_test: false,
            },
        ];

        let mut edges = vec![
            // This edge is dangling: class:code_graph/format.rs:CodeGraph doesn't exist
            CodeEdge {
                from: "method:code_graph/format.rs:CodeGraph.format_for_llm".to_string(),
                to: "class:code_graph/format.rs:CodeGraph".to_string(),
                relation: EdgeRelation::DefinedIn,
                weight: 1.0,
                call_count: 0,
                in_error_path: false,
                confidence: 1.0,
                call_site_line: None,
                call_site_column: None,
            },
            // This edge is fine: target node exists
            CodeEdge {
                from: "func:code_graph/extract.rs:some_func".to_string(),
                to: "class:code_graph/extract.rs:CodeGraph".to_string(),
                relation: EdgeRelation::Calls,
                weight: 1.0,
                call_count: 1,
                in_error_path: false,
                confidence: 1.0,
                call_site_line: None,
                call_site_column: None,
            },
        ];

        remap_cross_file_impl_edges(&mut edges, &nodes);

        // The dangling DefinedIn edge should now point to the actual class node
        assert_eq!(
            edges[0].to,
            "class:code_graph/extract.rs:CodeGraph",
            "DefinedIn edge should be remapped to actual class node"
        );

        // The Calls edge should be unchanged
        assert_eq!(
            edges[1].to,
            "class:code_graph/extract.rs:CodeGraph",
            "Non-dangling edge should be unchanged"
        );
    }
}
