//! TypeScript/JavaScript code extraction using tree-sitter AST parsing

use std::collections::{HashMap, HashSet};

use regex::Regex;
use tree_sitter::Parser;

use crate::code_graph::types::*;

// ─── TypeScript Tree-Sitter Extraction ───

/// Extract from TypeScript/JavaScript source using tree-sitter AST parsing.
/// Handles classes, interfaces, functions, enums, type aliases, and export statements.
pub(crate) fn extract_typescript_tree_sitter(
    path: &str,
    content: &str,
    parser: &mut Parser,
    class_id_map: &mut HashMap<String, String>,
    extension: &str,
) -> (Vec<CodeNode>, Vec<CodeEdge>, HashSet<String>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut imports = HashSet::new();

    // Choose language based on file extension
    let lang_result = match extension {
        "tsx" => parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into()),
        "ts" => parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "jsx" => parser.set_language(&tree_sitter_javascript::LANGUAGE.into()),
        _ => parser.set_language(&tree_sitter_javascript::LANGUAGE.into()),  // .js default
    };
    
    if lang_result.is_err() {
        return (nodes, edges, imports);
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return (nodes, edges, imports),
    };

    let file_id = format!("file:{}", path);
    let source = content.as_bytes();
    let root = tree.root_node();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        extract_typescript_node(
            child,
            source,
            content,
            path,
            &file_id,
            &mut nodes,
            &mut edges,
            class_id_map,
            &mut imports,
        );
    }

    (nodes, edges, imports)
}

/// Extract TypeScript/JavaScript nodes from AST
pub(crate) fn extract_typescript_node(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    file_id: &str,
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
    class_id_map: &mut HashMap<String, String>,
    imports: &mut HashSet<String>,
) {
    let text = |n: tree_sitter::Node| -> String {
        n.utf8_text(source).unwrap_or("").to_string()
    };

    match node.kind() {
        "import_statement" => {
            // Extract: import { ... } from 'module'; or import x from 'module';
            let import_text = text(node);
            if let Some(from_idx) = import_text.rfind(" from ") {
                let module_part = import_text[from_idx + 6..].trim();
                let module = module_part.trim_matches(|c| c == '\'' || c == '"' || c == ';');
                if module.starts_with('.') || module.starts_with("@/") {
                    edges.push(CodeEdge {
                        from: file_id.to_string(),
                        to: format!("module_ref:{}", module),
                        relation: EdgeRelation::Imports,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 1.0,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
                imports.insert(module.to_string());
                
                // Extract imported names
                if let Some(start) = import_text.find('{') {
                    if let Some(end) = import_text.find('}') {
                        let names_part = &import_text[start+1..end];
                        for name in names_part.split(',') {
                            let clean = name.trim().split(" as ").next().unwrap_or("").trim();
                            if !clean.is_empty() {
                                imports.insert(clean.to_string());
                            }
                        }
                    }
                }
            }
        }

        "class_declaration" | "class" => {
            extract_typescript_class(node, source, source_str, path, file_id, nodes, edges, class_id_map);
        }

        "abstract_class_declaration" => {
            extract_typescript_class(node, source, source_str, path, file_id, nodes, edges, class_id_map);
        }

        "interface_declaration" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let line = node.start_position().row + 1;
            let interface_id = format!("class:{}:{}", path, name);

            let signature = extract_typescript_signature(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: interface_id.clone(),
                kind: NodeKind::Class,
                name: name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: vec!["interface".to_string()],
                signature,
                docstring: extract_typescript_docstring(node, source_str),
                line_count,
                is_test: path.contains("/test") || name.contains("Test"),
            });

            edges.push(CodeEdge::defined_in(&interface_id, file_id));
            class_id_map.insert(name, interface_id);
        }

        "function_declaration" | "function" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let line = node.start_position().row + 1;
            let func_id = format!("func:{}:{}", path, name);

            let signature = extract_typescript_signature(node, source_str);
            let docstring = extract_typescript_docstring(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;
            let decorators = extract_typescript_decorators(node, source);

            nodes.push(CodeNode {
                id: func_id.clone(),
                kind: NodeKind::Function,
                name,
                file_path: path.to_string(),
                line: Some(line),
                decorators,
                signature,
                docstring,
                line_count,
                is_test: path.contains("/test") || path.contains(".test.") || path.contains(".spec."),
            });

            edges.push(CodeEdge::defined_in(&func_id, file_id));
        }

        "lexical_declaration" | "variable_declaration" => {
            // Check for arrow functions: const foo = () => { ... }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    let name = child.child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .unwrap_or("")
                        .to_string();
                    
                    if let Some(value) = child.child_by_field_name("value") {
                        if value.kind() == "arrow_function" || value.kind() == "function" {
                            if name.is_empty() { continue; }
                            
                            let line = node.start_position().row + 1;
                            let func_id = format!("func:{}:{}", path, name);

                            let signature = extract_typescript_signature(node, source_str);
                            let line_count = node.end_position().row - node.start_position().row + 1;

                            nodes.push(CodeNode {
                                id: func_id.clone(),
                                kind: NodeKind::Function,
                                name,
                                file_path: path.to_string(),
                                line: Some(line),
                                decorators: Vec::new(),
                                signature,
                                docstring: extract_typescript_docstring(node, source_str),
                                line_count,
                                is_test: path.contains("/test") || path.contains(".test.") || path.contains(".spec."),
                            });

                            edges.push(CodeEdge::defined_in(&func_id, file_id));
                        }
                    }
                }
            }
        }

        "enum_declaration" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let line = node.start_position().row + 1;
            let enum_id = format!("class:{}:{}", path, name);

            let signature = extract_typescript_signature(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: enum_id.clone(),
                kind: NodeKind::Class,
                name: name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: vec!["enum".to_string()],
                signature,
                docstring: extract_typescript_docstring(node, source_str),
                line_count,
                is_test: false,
            });

            edges.push(CodeEdge::defined_in(&enum_id, file_id));
            class_id_map.insert(name, enum_id);
        }

        "type_alias_declaration" => {
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            if name.is_empty() { return; }

            let line = node.start_position().row + 1;
            let type_id = format!("class:{}:{}", path, name);

            let signature = extract_typescript_signature(node, source_str);
            let line_count = node.end_position().row - node.start_position().row + 1;

            nodes.push(CodeNode {
                id: type_id.clone(),
                kind: NodeKind::Class,
                name: name.clone(),
                file_path: path.to_string(),
                line: Some(line),
                decorators: vec!["type".to_string()],
                signature,
                docstring: None,
                line_count,
                is_test: false,
            });

            edges.push(CodeEdge::defined_in(&type_id, file_id));
            class_id_map.insert(name, type_id);
        }

        "export_statement" => {
            // Unwrap export and process inner declaration
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "class_declaration" | "class" | "abstract_class_declaration" |
                    "interface_declaration" | "function_declaration" | "function" |
                    "lexical_declaration" | "variable_declaration" | "enum_declaration" |
                    "type_alias_declaration" => {
                        extract_typescript_node(child, source, source_str, path, file_id, nodes, edges, class_id_map, imports);
                    }
                    _ => {}
                }
            }
        }

        "expression_statement" => {
            // Handle wrapped statements like namespace (which appears as expression_statement → internal_module)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_typescript_node(child, source, source_str, path, file_id, nodes, edges, class_id_map, imports);
            }
        }

        "module" | "internal_module" | "namespace" => {
            // namespace/module declarations
            let name = node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            
            if !name.is_empty() {
                let line = node.start_position().row + 1;
                let module_id = format!("class:{}:{}", path, name);

                nodes.push(CodeNode {
                    id: module_id.clone(),
                    kind: NodeKind::Class,
                    name: name.clone(),
                    file_path: path.to_string(),
                    line: Some(line),
                    decorators: vec!["namespace".to_string()],
                    signature: Some(format!("namespace {}", name)),
                    docstring: None,
                    line_count: node.end_position().row - node.start_position().row + 1,
                    is_test: false,
                });

                edges.push(CodeEdge::defined_in(&module_id, file_id));
            }

            // Recurse into module body
            if let Some(body) = node.child_by_field_name("body") {
                let mut body_cursor = body.walk();
                for body_child in body.children(&mut body_cursor) {
                    extract_typescript_node(body_child, source, source_str, path, file_id, nodes, edges, class_id_map, imports);
                }
            }
        }

        _ => {}
    }
}

/// Extract TypeScript class with methods
pub(crate) fn extract_typescript_class(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    file_id: &str,
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
    class_id_map: &mut HashMap<String, String>,
) {
    let name = node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();
    if name.is_empty() { return; }

    let line = node.start_position().row + 1;
    let class_id = format!("class:{}:{}", path, name);

    let signature = extract_typescript_signature(node, source_str);
    let docstring = extract_typescript_docstring(node, source_str);
    let line_count = node.end_position().row - node.start_position().row + 1;
    let decorators = extract_typescript_decorators(node, source);

    nodes.push(CodeNode {
        id: class_id.clone(),
        kind: NodeKind::Class,
        name: name.clone(),
        file_path: path.to_string(),
        line: Some(line),
        decorators,
        signature,
        docstring,
        line_count,
        is_test: path.contains("/test") || name.contains("Test"),
    });

    edges.push(CodeEdge::defined_in(&class_id, file_id));
    class_id_map.insert(name.clone(), class_id.clone());

    // Find parent class from extends clause
    fn find_extends_identifier(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "identifier" | "type_identifier" => {
                    return child.utf8_text(source).ok().map(|s| s.to_string());
                }
                "extends_clause" | "class_heritage" | "extends_type_clause" => {
                    if let Some(name) = find_extends_identifier(child, source) {
                        return Some(name);
                    }
                }
                _ => {}
            }
        }
        None
    }
    
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "class_heritage" || child.kind() == "extends_clause" {
            if let Some(parent_name) = find_extends_identifier(child, source) {
                if !parent_name.is_empty() {
                    edges.push(CodeEdge {
                        from: class_id.clone(),
                        to: format!("class_ref:{}", parent_name),
                        relation: EdgeRelation::Inherits,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 1.0,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        }
    }

    // Extract methods from class body
    if let Some(body) = node.child_by_field_name("body") {
        let mut body_cursor = body.walk();
        for body_child in body.children(&mut body_cursor) {
            match body_child.kind() {
                "method_definition" | "public_field_definition" | "method_signature" => {
                    extract_typescript_method(body_child, source, source_str, path, &class_id, nodes, edges);
                }
                _ => {}
            }
        }
    }
}

/// Extract method from class
pub(crate) fn extract_typescript_method(
    node: tree_sitter::Node,
    source: &[u8],
    source_str: &str,
    path: &str,
    class_id: &str,
    nodes: &mut Vec<CodeNode>,
    edges: &mut Vec<CodeEdge>,
) {
    let mut name = node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();
    
    // Handle computed property names [key]
    if name.is_empty() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "property_identifier" || child.kind() == "identifier" {
                if let Ok(text) = child.utf8_text(source) {
                    name = text.to_string();
                    break;
                }
            }
        }
    }
    
    if name.is_empty() { return; }

    let line = node.start_position().row + 1;
    // Include parent class name in method ID to avoid collisions
    let parent_name = class_id.rsplit(':').next().unwrap_or("");
    let method_id = if parent_name.is_empty() {
        format!("method:{}:{}", path, name)
    } else {
        format!("method:{}:{}.{}", path, parent_name, name)
    };

    let signature = extract_typescript_signature(node, source_str);
    let docstring = extract_typescript_docstring(node, source_str);
    let line_count = node.end_position().row - node.start_position().row + 1;
    let decorators = extract_typescript_decorators(node, source);

    nodes.push(CodeNode {
        id: method_id.clone(),
        kind: NodeKind::Function,
        name,
        file_path: path.to_string(),
        line: Some(line),
        decorators,
        signature,
        docstring,
        line_count,
        is_test: path.contains("/test") || path.contains(".test.") || path.contains(".spec."),
    });

    edges.push(CodeEdge {
        from: method_id,
        to: class_id.to_string(),
        relation: EdgeRelation::DefinedIn,
        weight: 0.5,
        call_count: 1,
        in_error_path: false,
        confidence: 1.0,
        call_site_line: None,
        call_site_column: None,
    });
}

/// Extract TypeScript decorators (@decorator)
pub(crate) fn extract_typescript_decorators(node: tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut decorators = Vec::new();
    
    // Look for decorator siblings before this node
    if let Some(parent) = node.parent() {
        let mut cursor = parent.walk();
        for child in parent.children(&mut cursor) {
            if child.kind() == "decorator" {
                if let Ok(dec_text) = child.utf8_text(source) {
                    let name = dec_text.trim_start_matches('@');
                    let name = name.split('(').next().unwrap_or(name).trim();
                    if !name.is_empty() {
                        decorators.push(name.to_string());
                    }
                }
            }
            if child.id() == node.id() {
                break;
            }
        }
    }
    
    decorators
}

/// Extract signature from TypeScript node
pub(crate) fn extract_typescript_signature(node: tree_sitter::Node, source_str: &str) -> Option<String> {
    let start = node.start_byte();
    if start >= source_str.len() { return None; }
    
    let sig_text = &source_str[start..];
    // Find the end of signature (before body block)
    let sig_end = sig_text.find(" {")
        .or_else(|| sig_text.find("\n{"))
        .or_else(|| sig_text.find("{\n"))
        .unwrap_or(sig_text.len().min(200));
    
    let sig = sig_text[..sig_end].trim();
    if sig.is_empty() { None } else { Some(sig.to_string()) }
}

/// Extract JSDoc comment from TypeScript node
pub(crate) fn extract_typescript_docstring(node: tree_sitter::Node, source_str: &str) -> Option<String> {
    let start_line = node.start_position().row;
    if start_line == 0 { return None; }
    
    let lines: Vec<&str> = source_str.lines().collect();
    
    // Look for /** ... */ comment before the node
    for i in (0..start_line).rev() {
        if i >= lines.len() { continue; }
        let line = lines[i].trim();
        
        if line.ends_with("*/") {
            // Found end of JSDoc, find the start
            let mut doc_lines: Vec<&str> = Vec::new();
            for j in (0..=i).rev() {
                if j >= lines.len() { continue; }
                let doc_line = lines[j].trim();
                if doc_line.starts_with("/**") {
                    let first = doc_line.trim_start_matches("/**").trim_start_matches('*').trim();
                    if !first.is_empty() && !first.starts_with('@') {
                        doc_lines.push(first);
                    }
                    break;
                } else if doc_line.starts_with('*') {
                    let content = doc_line.trim_start_matches('*').trim();
                    if !content.is_empty() && !content.starts_with('@') {
                        doc_lines.push(content);
                    }
                }
            }
            
            if doc_lines.is_empty() {
                return None;
            }
            
            doc_lines.reverse();
            let first_line = doc_lines.first().copied().unwrap_or("");
            let truncated = if first_line.len() > 100 {
                &first_line[..100]
            } else {
                first_line
            };
            
            return if truncated.is_empty() { None } else { Some(truncated.to_string()) };
        } else if line.is_empty() || line.starts_with('@') || line.starts_with("//") {
            continue;
        } else {
            break;
        }
    }
    
    None
}

// ─── Regex-Based Fallbacks (kept for reference) ───


/// Extract from TypeScript/JavaScript source (regex-based fallback, kept for reference).
#[allow(dead_code)]
pub(crate) fn extract_typescript_regex(path: &str, content: &str) -> (Vec<CodeNode>, Vec<CodeEdge>, HashSet<String>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let file_id = format!("file:{}", path);

    let re_import = Regex::new(r#"(?m)^import\s+.*?\s+from\s+['"]([^'"]+)['"]"#).unwrap();
    let re_class = Regex::new(r"(?m)^(?:export\s+)?(?:abstract\s+)?class\s+(\w+)(?:\s+extends\s+(\w+))?").unwrap();
    let re_interface = Regex::new(r"(?m)^(?:export\s+)?interface\s+(\w+)(?:\s+extends\s+(\w+))?").unwrap();
    let re_function = Regex::new(r"(?m)^(?:export\s+)?(?:async\s+)?function\s+(\w+)").unwrap();
    let re_arrow = Regex::new(r"(?m)^(?:export\s+)?(?:const|let)\s+(\w+)\s*=\s*(?:async\s+)?\([^)]*\)\s*=>").unwrap();

    for cap in re_import.captures_iter(content) {
        let module = cap[1].to_string();
        if module.starts_with('.') || module.starts_with("@/") {
            edges.push(CodeEdge::new(
                &file_id,
                &format!("module_ref:{}", module),
                EdgeRelation::Imports,
            ));
        }
    }

    for cap in re_class.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_class(path, &name, line);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));

        if let Some(parent) = cap.get(2) {
            edges.push(CodeEdge::new(
                &node.id,
                &format!("class_ref:{}", parent.as_str()),
                EdgeRelation::Inherits,
            ));
        }

        nodes.push(node);
    }

    for cap in re_interface.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_class(path, &name, line);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    for cap in re_function.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_function(path, &name, line, false);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    for cap in re_arrow.captures_iter(content) {
        let name = cap[1].to_string();
        let line = content[..cap.get(0).unwrap().start()].lines().count() + 1;
        let node = CodeNode::new_function(path, &name, line, false);
        edges.push(CodeEdge::defined_in(&node.id, &file_id));
        nodes.push(node);
    }

    (nodes, edges, HashSet::new())
}

// ═══ Helpers ═══


pub(crate) fn is_typescript_builtin(name: &str) -> bool {
    matches!(
        name,
        // Console
        "log" | "error" | "warn" | "debug" | "info" | "trace" | "dir" | "table"
            // Timers
            | "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval"
            | "setImmediate" | "clearImmediate"
            // Parsing
            | "parseInt" | "parseFloat" | "isNaN" | "isFinite"
            // Require/import
            | "require" | "import"
            // Promise statics
            | "resolve" | "reject" | "all" | "race" | "allSettled" | "any"
            // Object statics
            | "keys" | "values" | "entries" | "assign" | "freeze" | "seal"
            | "defineProperty" | "getOwnPropertyNames" | "getPrototypeOf"
            // Array statics
            | "isArray" | "from" | "of"
            // JSON
            | "parse" | "stringify"
            // Math
            | "floor" | "ceil" | "round" | "abs" | "min" | "max" | "random"
            | "sqrt" | "pow" | "sin" | "cos" | "tan"
            // String methods (common)
            | "toString" | "valueOf" | "charAt" | "charCodeAt" | "codePointAt"
            | "concat" | "includes" | "indexOf" | "lastIndexOf"
            | "match" | "replace" | "search" | "slice" | "split"
            | "substring" | "substr" | "toLowerCase" | "toUpperCase" | "trim"
            // Array methods (common)
            | "push" | "pop" | "shift" | "unshift" | "splice"
            | "map" | "filter" | "reduce" | "reduceRight" | "find" | "findIndex"
            | "every" | "some" | "forEach" | "join" | "sort" | "reverse"
            | "fill" | "copyWithin" | "flat" | "flatMap"
            // Reflect/Proxy
            | "Reflect" | "Proxy"
            // Node.js globals
            | "process" | "Buffer" | "__dirname" | "__filename"
    )
}

/// Check if a TypeScript object.method call should be skipped
pub(crate) fn is_typescript_builtin_method(obj: &str, method: &str) -> bool {
    match obj {
        "console" => matches!(method, "log" | "error" | "warn" | "debug" | "info" | "trace" | "dir" | "table" | "time" | "timeEnd" | "assert"),
        "Promise" => matches!(method, "resolve" | "reject" | "all" | "race" | "allSettled" | "any"),
        "Object" => matches!(method, "keys" | "values" | "entries" | "assign" | "freeze" | "seal" | "defineProperty" | "getOwnPropertyNames" | "getPrototypeOf" | "create" | "hasOwn"),
        "Array" => matches!(method, "isArray" | "from" | "of"),
        "JSON" => matches!(method, "parse" | "stringify"),
        "Math" => true, // All Math methods are builtins
        "Number" => matches!(method, "isNaN" | "isFinite" | "isInteger" | "isSafeInteger" | "parseInt" | "parseFloat"),
        "String" => matches!(method, "fromCharCode" | "fromCodePoint" | "raw"),
        "Date" => matches!(method, "now" | "parse" | "UTC"),
        "Reflect" => true, // All Reflect methods are builtins
        "process" => true, // Node.js process is builtin
        "Buffer" => true, // Node.js Buffer is builtin
        _ => false,
    }
}

/// Resolve a TypeScript/JavaScript import path to a module_map key.
/// Handles relative paths like `./foo`, `../bar`, `../../components/Stats.js`
/// and converts them to dot-separated format matching module_map keys.
pub(crate) fn resolve_ts_import(
    importing_file: &str,
    import_module: &str,
    module_map: &HashMap<String, String>,
) -> Option<String> {
    // Handle path aliases like @/foo - just try the literal path
    if import_module.starts_with('@') {
        // Try @/foo -> src.foo
        let without_at = import_module.trim_start_matches("@/");
        let normalized = normalize_ts_module_path(without_at);
        if let Some(file_id) = module_map.get(&normalized) {
            return Some(file_id.clone());
        }
        // Try with src prefix
        let with_src = format!("src.{}", normalized);
        if let Some(file_id) = module_map.get(&with_src) {
            return Some(file_id.clone());
        }
        return None;
    }

    // Only handle relative imports
    if !import_module.starts_with('.') {
        return None;
    }

    // Get the directory of the importing file
    let importing_dir = if let Some(pos) = importing_file.rfind('/') {
        &importing_file[..pos]
    } else {
        ""
    };

    // Resolve the relative path
    let resolved = resolve_relative_path(importing_dir, import_module);
    
    // Normalize: strip extensions and convert / to .
    let normalized = normalize_ts_module_path(&resolved);
    
    // Try direct lookup
    if let Some(file_id) = module_map.get(&normalized) {
        return Some(file_id.clone());
    }
    
    // Try with common TS extensions (import says .js but file might be .tsx)
    // The module_map was built without extensions, so we just try the base name
    // But sometimes partial paths exist, try those too
    let parts: Vec<&str> = normalized.split('.').collect();
    for start in 1..parts.len() {
        let partial = parts[start..].join(".");
        if let Some(file_id) = module_map.get(&partial) {
            return Some(file_id.clone());
        }
    }
    
    None
}

/// Resolve a relative path against a base directory
pub(crate) fn resolve_relative_path(base_dir: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    
    for segment in relative.split('/') {
        match segment {
            "." | "" => continue,
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    
    parts.join("/")
}

/// Normalize a TypeScript module path to dot-separated format
pub(crate) fn normalize_ts_module_path(path: &str) -> String {
    path.replace('/', ".")
        .trim_end_matches(".js")
        .trim_end_matches(".jsx")
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".mjs")
        .trim_end_matches(".mts")
        .to_string()
}

// ═══ Call Extraction - Rust ═══

/// Build scope map for Rust — maps line ranges to function IDs

pub(crate) fn build_scope_map_typescript(
    node: tree_sitter::Node,
    source: &[u8],
    rel_path: &str,
    scope_map: &mut Vec<(usize, usize, String, Option<String>)>,
) {
    let mut stack: Vec<(tree_sitter::Node, Option<String>)> = vec![(node, None)];

    while let Some((current, class_ctx)) = stack.pop() {
        match current.kind() {
            "class_declaration" | "class" | "abstract_class_declaration" => {
                let class_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let class_id = if !class_name.is_empty() {
                    Some(format!("class:{}:{}", rel_path, class_name))
                } else {
                    class_ctx.clone()
                };

                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_id.clone()));
                    }
                }
            }
            "function_declaration" | "function" => {
                let func_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");

                if !func_name.is_empty() {
                    let start_line = current.start_position().row + 1;
                    let end_line = current.end_position().row + 1;
                    let func_id = format!("func:{}:{}", rel_path, func_name);
                    scope_map.push((start_line, end_line, func_id, class_ctx.clone()));
                }

                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "method_definition" | "method_signature" => {
                let method_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");

                if !method_name.is_empty() {
                    let start_line = current.start_position().row + 1;
                    let end_line = current.end_position().row + 1;

                    let method_id = if let Some(ref cls) = class_ctx {
                        let cls_name = cls.rsplit(':').next().unwrap_or("");
                        if cls_name.is_empty() {
                            format!("method:{}:{}", rel_path, method_name)
                        } else {
                            format!("method:{}:{}.{}", rel_path, cls_name, method_name)
                        }
                    } else {
                        format!("method:{}:{}", rel_path, method_name)
                    };

                    scope_map.push((start_line, end_line, method_id, class_ctx.clone()));
                }

                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "arrow_function" => {
                // Arrow functions inside variable declarators
                // The scope is tracked but ID comes from the variable name
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                // Check for const foo = () => {}
                let mut cursor = current.walk();
                for child in current.children(&mut cursor) {
                    if child.kind() == "variable_declarator" {
                        let var_name = child.child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("");
                        
                        if let Some(value) = child.child_by_field_name("value") {
                            if value.kind() == "arrow_function" || value.kind() == "function" {
                                if !var_name.is_empty() {
                                    let start_line = current.start_position().row + 1;
                                    let end_line = current.end_position().row + 1;
                                    let func_id = format!("func:{}:{}", rel_path, var_name);
                                    scope_map.push((start_line, end_line, func_id, class_ctx.clone()));
                                }
                            }
                        }
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            "export_statement" => {
                // Unwrap export and process inner
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
            _ => {
                let child_count = current.child_count();
                for i in (0..child_count).rev() {
                    if let Some(child) = current.child(i) {
                        stack.push((child, class_ctx.clone()));
                    }
                }
            }
        }
    }
}

/// Extract calls from TypeScript AST
pub(crate) fn extract_calls_typescript(
    root: tree_sitter::Node,
    source: &[u8],
    rel_path: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    file_func_ids: &HashSet<String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    node_pkg_map: &HashMap<String, String>,
    edges: &mut Vec<CodeEdge>,
) {
    // Build scope map
    let mut scope_map: Vec<(usize, usize, String, Option<String>)> = Vec::new();
    build_scope_map_typescript(root, source, rel_path, &mut scope_map);

    let package_dir = rel_path.rsplitn(2, '/').nth(1).unwrap_or("");

    // Walk tree looking for calls
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        // Skip string literals and comments
        if node.kind() == "string" 
            || node.kind() == "template_string"
            || node.kind() == "comment"
        {
            continue;
        }

        match node.kind() {
            "call_expression" => {
                let call_line = node.start_position().row + 1;

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, caller_class)) = scope {
                    if let Some(func_node) = node.child_by_field_name("function") {
                        match func_node.kind() {
                            "identifier" => {
                                let callee_name = func_node.utf8_text(source).unwrap_or("");
                                if !callee_name.is_empty() && !is_typescript_builtin(callee_name) {
                                    resolve_typescript_call_edge(
                                        caller_id,
                                        callee_name,
                                        func_name_map,
                                        file_func_ids,
                                        file_imported_names,
                                        rel_path,
                                        package_dir,
                                        node_pkg_map,
                                        false,
                                        edges,
                                    );
                                }
                            }
                            "member_expression" => {
                                // obj.method()
                                let obj_node = func_node.child_by_field_name("object");
                                let prop_node = func_node.child_by_field_name("property");

                                if let (Some(obj), Some(prop)) = (obj_node, prop_node) {
                                    let obj_text = obj.utf8_text(source).unwrap_or("");
                                    let method_name = prop.utf8_text(source).unwrap_or("");

                                    if !method_name.is_empty() {
                                        // Check for builtin object.method patterns
                                        if is_typescript_builtin_method(obj_text, method_name) {
                                            // Skip builtin
                                        } else if obj_text == "this" {
                                            // this.method() — resolve within class
                                            resolve_typescript_self_method_call(
                                                caller_id,
                                                method_name,
                                                caller_class.as_deref(),
                                                func_name_map,
                                                method_to_class,
                                                file_func_ids,
                                                edges,
                                            );
                                        } else if !is_typescript_builtin(method_name) {
                                            // Regular method call
                                            resolve_typescript_call_edge(
                                                caller_id,
                                                method_name,
                                                func_name_map,
                                                file_func_ids,
                                                file_imported_names,
                                                rel_path,
                                                package_dir,
                                                node_pkg_map,
                                                true,
                                                edges,
                                            );
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "new_expression" => {
                // new ClassName()
                let call_line = node.start_position().row + 1;

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, _)) = scope {
                    if let Some(constructor) = node.child_by_field_name("constructor") {
                        let class_name = constructor.utf8_text(source).unwrap_or("");
                        
                        // Skip builtins like new Promise, new Error, etc.
                        if !class_name.is_empty() 
                            && !matches!(class_name, "Promise" | "Error" | "Array" | "Object" | "Map" | "Set" | "WeakMap" | "WeakSet" | "Date" | "RegExp" | "URL" | "URLSearchParams" | "Headers" | "Request" | "Response" | "FormData" | "Blob" | "File" | "FileReader" | "Image" | "Event" | "CustomEvent" | "AbortController")
                        {
                            // Look for class constructor by name
                            if let Some(callee_ids) = func_name_map.get(class_name) {
                                let targets: Vec<&String> = if callee_ids.len() <= 5 {
                                    callee_ids.iter().collect()
                                } else {
                                    callee_ids.iter().filter(|id| file_func_ids.contains(*id)).collect()
                                };

                                for callee_id in targets {
                                    if callee_id != caller_id {
                                        edges.push(CodeEdge {
                                            from: caller_id.to_string(),
                                            to: callee_id.clone(),
                                            relation: EdgeRelation::Calls,
                                            weight: 0.5,
                                            call_count: 1,
                                            in_error_path: false,
                                            confidence: 0.7,
                                            call_site_line: None,
                                            call_site_column: None,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // JSX component references like <Stats /> or <Dashboard>...</Dashboard>
            "jsx_element" | "jsx_self_closing_element" => {
                let call_line = node.start_position().row + 1;

                let scope = scope_map
                    .iter()
                    .filter(|(start, end, _, _)| call_line >= *start && call_line <= *end)
                    .max_by_key(|(start, _, _, _)| *start);

                if let Some((_start, _end, caller_id, _)) = scope {
                    // For jsx_element, the opening tag is the first child (jsx_opening_element)
                    // For jsx_self_closing_element, the name is directly accessible
                    let tag_name = if node.kind() == "jsx_self_closing_element" {
                        node.child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("")
                    } else {
                        // jsx_element has opening_element as first child
                        node.child(0)
                            .and_then(|open| open.child_by_field_name("name"))
                            .and_then(|n| n.utf8_text(source).ok())
                            .unwrap_or("")
                    };

                    // Only process PascalCase component names (user-defined components)
                    // Lowercase tags like <div>, <span> are HTML elements
                    if !tag_name.is_empty() 
                        && tag_name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                        && !is_typescript_builtin(tag_name)
                    {
                        resolve_typescript_call_edge(
                            caller_id,
                            tag_name,
                            func_name_map,
                            file_func_ids,
                            file_imported_names,
                            rel_path,
                            package_dir,
                            node_pkg_map,
                            false,
                            edges,
                        );
                    }
                }
            }
            _ => {}
        }

        let child_count = node.child_count();
        for i in (0..child_count).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
}

/// Resolve and add TypeScript call edge
pub(crate) fn resolve_typescript_call_edge(
    caller_id: &str,
    callee_name: &str,
    func_name_map: &HashMap<String, Vec<String>>,
    file_func_ids: &HashSet<String>,
    file_imported_names: &HashMap<String, HashSet<String>>,
    rel_path: &str,
    package_dir: &str,
    node_pkg_map: &HashMap<String, String>,
    is_method_call: bool,
    edges: &mut Vec<CodeEdge>,
) {
    if let Some(callee_ids) = func_name_map.get(callee_name) {
        let same_file: Vec<&String> = callee_ids
            .iter()
            .filter(|id| file_func_ids.contains(*id))
            .collect();
        let imported: Vec<&String> = callee_ids
            .iter()
            .filter(|_id| {
                file_imported_names
                    .get(rel_path)
                    .map(|names| names.contains(callee_name))
                    .unwrap_or(false)
            })
            .collect();
        let same_pkg: Vec<&String> = callee_ids
            .iter()
            .filter(|id| {
                node_pkg_map
                    .get(id.as_str())
                    .map(|pkg| pkg == package_dir)
                    .unwrap_or(false)
            })
            .collect();

        let global_limit = if is_method_call { 15 } else { 3 };

        let (targets, confidence): (Vec<&String>, f32) = if !same_file.is_empty() {
            (same_file, 0.9)
        } else if !imported.is_empty() {
            (imported, 0.8)
        } else if !same_pkg.is_empty() {
            (same_pkg, 0.7)
        } else if callee_ids.len() <= global_limit {
            (callee_ids.iter().collect(), 0.5)
        } else {
            (vec![], 0.0)
        };

        for callee_id in targets {
            if callee_id != caller_id {
                edges.push(CodeEdge {
                    from: caller_id.to_string(),
                    to: callee_id.clone(),
                    relation: EdgeRelation::Calls,
                    weight: 0.5,
                    call_count: 1,
                    in_error_path: false,
                    confidence,
                    call_site_line: None,
                    call_site_column: None,
                });
            }
        }
    }
}

/// Resolve this.method() calls in TypeScript
pub(crate) fn resolve_typescript_self_method_call(
    caller_id: &str,
    method_name: &str,
    caller_class: Option<&str>,
    func_name_map: &HashMap<String, Vec<String>>,
    method_to_class: &HashMap<String, String>,
    file_func_ids: &HashSet<String>,
    edges: &mut Vec<CodeEdge>,
) {
    if let Some(callee_ids) = func_name_map.get(method_name) {
        if let Some(class_id) = caller_class {
            let scoped: Vec<&String> = callee_ids
                .iter()
                .filter(|id| {
                    method_to_class
                        .get(*id)
                        .map(|cls| cls == class_id)
                        .unwrap_or(false)
                })
                .collect();

            let targets = if !scoped.is_empty() {
                scoped
            } else if callee_ids.len() <= 5 {
                callee_ids.iter().collect()
            } else {
                callee_ids
                    .iter()
                    .filter(|id| file_func_ids.contains(*id))
                    .collect()
            };

            for callee_id in targets {
                if callee_id != caller_id {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: callee_id.clone(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.9,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        } else {
            // No class context
            for callee_id in callee_ids {
                if callee_id != caller_id && file_func_ids.contains(callee_id) {
                    edges.push(CodeEdge {
                        from: caller_id.to_string(),
                        to: callee_id.clone(),
                        relation: EdgeRelation::Calls,
                        weight: 0.5,
                        call_count: 1,
                        in_error_path: false,
                        confidence: 0.6,
                        call_site_line: None,
                        call_site_column: None,
                    });
                }
            }
        }
    }
}

