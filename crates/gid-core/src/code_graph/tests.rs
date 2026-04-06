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
}
