//! Design module for LLM-assisted graph generation.
//!
//! Generates prompts and parses LLM responses. Does NOT call LLM directly.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use crate::graph::{Graph, Node, Edge, NodeStatus, ProjectMeta};

/// A proposed feature from requirements decomposition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureProposal {
    pub name: String,
    pub description: String,
    /// Priority: core, supporting, or optional
    pub priority: String,
    /// Whether this feature is selected for implementation
    #[serde(default = "default_true")]
    pub selected: bool,
}

fn default_true() -> bool { true }

/// A proposed component for a feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentProposal {
    pub name: String,
    pub description: String,
    /// Layer: interface, application, domain, infrastructure
    pub layer: String,
    /// IDs of components this one depends on
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Result from parsing an LLM graph design response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignResult {
    pub features: Vec<FeatureProposal>,
    pub components: Vec<ComponentProposal>,
    pub graph: Option<Graph>,
}

/// Generate a prompt for decomposing requirements into features.
pub fn generate_features_prompt(requirements: &str) -> String {
    format!(r#"You are a software architect. Analyze the following requirements and decompose them into features.

REQUIREMENTS:
{}

Respond with a JSON object containing a "features" array. Each feature should have:
- name: Short identifier (snake_case)
- description: One sentence explaining the feature
- priority: "core" (essential), "supporting" (needed but not critical), or "optional"

Example response:
```json
{{
  "features": [
    {{
      "name": "user_authentication",
      "description": "Allow users to sign up, log in, and manage their accounts",
      "priority": "core"
    }},
    {{
      "name": "data_export",
      "description": "Export user data to various formats like CSV and JSON",
      "priority": "supporting"
    }}
  ]
}}
```

Only output valid JSON. No explanation before or after."#, requirements)
}

/// Generate a prompt for designing components for a feature.
pub fn generate_components_prompt(feature: &FeatureProposal, context: Option<&str>) -> String {
    let context_section = context.map(|c| format!("\nEXISTING CONTEXT:\n{}\n", c)).unwrap_or_default();
    
    format!(r#"You are a software architect. Design components for implementing the following feature.
{context_section}
FEATURE:
Name: {}
Description: {}
Priority: {}

Design components following Clean Architecture layers:
- interface: User-facing (CLI commands, API routes, UI components)
- application: Use cases and orchestration
- domain: Core business logic and entities
- infrastructure: External integrations (DB, filesystem, APIs)

Respond with a JSON object containing a "components" array. Each component should have:
- name: Short identifier (PascalCase)
- description: What this component does
- layer: One of interface, application, domain, infrastructure
- depends_on: Array of other component names this depends on

Example response:
```json
{{
  "components": [
    {{
      "name": "AuthController",
      "description": "Handles HTTP authentication endpoints",
      "layer": "interface",
      "depends_on": ["AuthService"]
    }},
    {{
      "name": "AuthService",
      "description": "Orchestrates authentication logic",
      "layer": "application",
      "depends_on": ["UserRepository", "TokenValidator"]
    }}
  ]
}}
```

Only output valid JSON. No explanation before or after."#,
        feature.name,
        feature.description,
        feature.priority
    )
}

/// Generate a prompt that produces a complete graph YAML.
pub fn generate_graph_prompt(requirements: &str) -> String {
    format!(r#"You are a software architect. Generate a GID (Graph Indexed Development) graph for the following requirements.

REQUIREMENTS:
{}

Output a valid YAML graph with this structure:
- project: Project metadata (name, description)
- nodes: Array of nodes (tasks, features, components)
- edges: Array of edges (dependencies between nodes)

Node structure:
- id: Unique identifier (snake_case)
- title: Human-readable title
- status: todo, in_progress, done, blocked
- description: Optional detailed description
- tags: Optional array of tags
- type: Optional type (task, feature, component, file)

Edge structure:
- from: Source node ID
- to: Target node ID  
- relation: depends_on, implements, contains

Example output:
```yaml
project:
  name: my-project
  description: A sample project

nodes:
  - id: setup_repo
    title: Initialize repository
    status: todo
    type: task
    
  - id: user_auth
    title: User Authentication
    status: todo
    type: feature
    description: Allow users to sign in
    
  - id: auth_service
    title: Authentication Service
    status: todo
    type: component
    
edges:
  - from: auth_service
    to: user_auth
    relation: implements
    
  - from: user_auth
    to: setup_repo
    relation: depends_on
```

Only output valid YAML. No explanation before or after.
Start your response with "```yaml" and end with "```"."#, requirements)
}

/// Generate a graph prompt scoped to a single feature, including existing graph context.
///
/// Unlike `generate_graph_prompt()` which generates a full graph from scratch, this function
/// generates ONLY new nodes for the specified feature while referencing existing nodes by ID.
/// This avoids ID collisions and enables cross-feature dependency edges.
pub fn generate_scoped_graph_prompt(
    design_doc: &str,
    existing_nodes: &[&Node],
    feature_scope: &str,
) -> String {
    let existing_context = if existing_nodes.is_empty() {
        "  (none — this is the first feature)\n".to_string()
    } else {
        existing_nodes
            .iter()
            .map(|n| {
                let node_type = n.node_type.as_deref().unwrap_or("unknown");
                format!("  - {} ({}): {}", n.id, node_type, n.title)
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };

    format!(
        r#"You are a software architect. Generate ONLY the new graph nodes for feature "{feature_scope}".

EXISTING GRAPH NODES (do NOT recreate these, reference them by ID in edges):
{existing_context}
NEW FEATURE DESIGN:
{design_doc}

Instructions:
- Generate YAML with ONLY new nodes and edges for this feature
- Use existing node IDs in edges for cross-feature dependencies
- New task IDs should follow the pattern: task-{{feature-slug}}-{{task-slug}}
- Create a feature node: feat-{{feature-slug}}
- Each task should have: implements edge to the feature node
- Add depends_on edges where tasks have dependencies

Output format:
```yaml
nodes:
  - id: ...
    title: ...
    node_type: task|feature
    status: todo
    ...
edges:
  - from: ...
    to: ...
    relation: implements|depends_on|...
```

Only output valid YAML. No explanation before or after.
Start your response with "```yaml" and end with "```"."#,
        feature_scope = feature_scope,
        existing_context = existing_context,
        design_doc = design_doc,
    )
}

/// Parse an LLM response containing features JSON.
pub fn parse_features_response(response: &str) -> Result<Vec<FeatureProposal>> {
    let json_str = extract_json(response)?;
    
    #[derive(Deserialize)]
    struct FeaturesResponse {
        features: Vec<FeatureProposal>,
    }
    
    let parsed: FeaturesResponse = serde_json::from_str(&json_str)
        .context("Failed to parse features JSON")?;
    
    Ok(parsed.features)
}

/// Parse an LLM response containing components JSON.
pub fn parse_components_response(response: &str) -> Result<Vec<ComponentProposal>> {
    let json_str = extract_json(response)?;
    
    #[derive(Deserialize)]
    struct ComponentsResponse {
        components: Vec<ComponentProposal>,
    }
    
    let parsed: ComponentsResponse = serde_json::from_str(&json_str)
        .context("Failed to parse components JSON")?;
    
    Ok(parsed.components)
}

/// Parse an LLM response containing a graph YAML.
pub fn parse_llm_response(response: &str) -> Result<Graph> {
    let yaml_str = extract_yaml(response)?;
    
    let graph: Graph = serde_yaml::from_str(&yaml_str)
        .context("Failed to parse graph YAML")?;
    
    Ok(graph)
}

/// Build a Graph from features and components.
pub fn build_graph_from_proposals(
    project_name: &str,
    features: &[FeatureProposal],
    components: &[ComponentProposal],
) -> Graph {
    let mut graph = Graph {
        project: Some(ProjectMeta {
            name: project_name.to_string(),
            description: None,
        }),
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    
    // Add feature nodes
    for feature in features {
        if !feature.selected {
            continue;
        }
        
        let mut node = Node::new(&feature.name, &feature.name);
        node.description = Some(feature.description.clone());
        node.node_type = Some("feature".to_string());
        node.status = NodeStatus::Todo;
        
        // Add priority as tag
        node.tags.push(feature.priority.clone());
        
        graph.add_node(node);
    }
    
    // Add component nodes
    for component in components {
        let id = to_snake_case(&component.name);
        let mut node = Node::new(&id, &component.name);
        node.description = Some(component.description.clone());
        node.node_type = Some("component".to_string());
        node.status = NodeStatus::Todo;
        
        // Add layer as tag
        node.tags.push(component.layer.clone());
        
        graph.add_node(node);
        
        // Add dependency edges
        for dep in &component.depends_on {
            let dep_id = to_snake_case(dep);
            graph.add_edge(Edge::new(&id, &dep_id, "depends_on"));
        }
    }
    
    graph
}

/// Extract JSON from a response that may contain markdown code blocks.
fn extract_json(response: &str) -> Result<String> {
    // Try to find JSON in code block
    if let Some(start) = response.find("```json") {
        let content = &response[start + 7..];
        if let Some(end) = content.find("```") {
            return Ok(content[..end].trim().to_string());
        }
    }
    
    // Try plain code block
    if let Some(start) = response.find("```") {
        let content = &response[start + 3..];
        if let Some(end) = content.find("```") {
            let inner = content[..end].trim();
            // Skip language identifier if present
            if let Some(newline) = inner.find('\n') {
                let first_line = &inner[..newline];
                if !first_line.starts_with('{') && !first_line.starts_with('[') {
                    return Ok(inner[newline..].trim().to_string());
                }
            }
            return Ok(inner.to_string());
        }
    }
    
    // Try to find raw JSON (starts with { or [)
    let trimmed = response.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Ok(trimmed.to_string());
    }
    
    bail!("No JSON found in response")
}

/// Extract YAML from a response that may contain markdown code blocks.
fn extract_yaml(response: &str) -> Result<String> {
    // Try to find YAML in code block
    if let Some(start) = response.find("```yaml") {
        let content = &response[start + 7..];
        if let Some(end) = content.find("```") {
            return Ok(content[..end].trim().to_string());
        }
    }
    
    // Try yml variant
    if let Some(start) = response.find("```yml") {
        let content = &response[start + 6..];
        if let Some(end) = content.find("```") {
            return Ok(content[..end].trim().to_string());
        }
    }
    
    // Try plain code block
    if let Some(start) = response.find("```") {
        let content = &response[start + 3..];
        if let Some(end) = content.find("```") {
            let inner = content[..end].trim();
            // Skip language identifier if present
            if let Some(newline) = inner.find('\n') {
                let first_line = &inner[..newline];
                if !first_line.contains(':') {
                    return Ok(inner[newline..].trim().to_string());
                }
            }
            return Ok(inner.to_string());
        }
    }
    
    // Assume raw YAML
    let trimmed = response.trim();
    if trimmed.contains(':') {
        return Ok(trimmed.to_string());
    }
    
    bail!("No YAML found in response")
}

/// Convert PascalCase or any case to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    let mut prev_was_upper = false;
    
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 && !prev_was_upper {
                result.push('_');
            }
            // to_lowercase() can yield multiple chars for some Unicode (e.g. 'İ' → 'i̇')
            // Use the first char, fallback to original if iterator is empty (shouldn't happen)
            for lc in c.to_lowercase() {
                result.push(lc);
            }
            prev_was_upper = true;
        } else if c == '-' || c == ' ' {
            result.push('_');
            prev_was_upper = false;
        } else {
            result.push(c);
            prev_was_upper = false;
        }
    }
    
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_extract_json_from_code_block() {
        let response = r#"Here's the JSON:
```json
{
  "features": [{"name": "test", "description": "Test feature", "priority": "core"}]
}
```
"#;
        let json = extract_json(response).unwrap();
        assert!(json.contains("features"));
    }
    
    #[test]
    fn test_extract_yaml_from_code_block() {
        let response = r#"```yaml
project:
  name: test
nodes: []
edges: []
```"#;
        let yaml = extract_yaml(response).unwrap();
        assert!(yaml.contains("project:"));
    }
    
    #[test]
    fn test_parse_features_response() {
        let response = r#"```json
{
  "features": [
    {"name": "auth", "description": "Authentication", "priority": "core"}
  ]
}
```"#;
        let features = parse_features_response(response).unwrap();
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].name, "auth");
    }
    
    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("AuthService"), "auth_service");
        assert_eq!(to_snake_case("HTTPClient"), "httpclient"); // All caps treated as sequence
        assert_eq!(to_snake_case("user-auth"), "user_auth");
    }
    
    #[test]
    fn test_build_graph() {
        let features = vec![
            FeatureProposal {
                name: "auth".to_string(),
                description: "Authentication".to_string(),
                priority: "core".to_string(),
                selected: true,
            },
        ];
        
        let components = vec![
            ComponentProposal {
                name: "AuthService".to_string(),
                description: "Auth service".to_string(),
                layer: "application".to_string(),
                depends_on: vec![],
            },
        ];
        
        let graph = build_graph_from_proposals("test", &features, &components);
        assert_eq!(graph.nodes.len(), 2);
    }

    #[test]
    fn test_scoped_prompt_includes_existing_node_context() {
        let mut node1 = Node::new("feat-auth", "Authentication system");
        node1.node_type = Some("feature".to_string());
        let mut node2 = Node::new("task-auth-jwt", "Implement JWT validation");
        node2.node_type = Some("task".to_string());

        let existing: Vec<&Node> = vec![&node1, &node2];
        let prompt = generate_scoped_graph_prompt(
            "Add payment processing",
            &existing,
            "payments",
        );

        assert!(prompt.contains("feat-auth (feature): Authentication system"));
        assert!(prompt.contains("task-auth-jwt (task): Implement JWT validation"));
    }

    #[test]
    fn test_scoped_prompt_includes_design_doc() {
        let design_doc = "Add Stripe-based payment processing with webhooks";
        let prompt = generate_scoped_graph_prompt(design_doc, &[], "payments");

        assert!(prompt.contains(design_doc));
    }

    #[test]
    fn test_scoped_prompt_specifies_feature_scope() {
        let prompt = generate_scoped_graph_prompt("Some design", &[], "payments");

        assert!(prompt.contains(r#"feature "payments""#));
    }

    #[test]
    fn test_scoped_prompt_with_empty_existing_nodes() {
        let prompt = generate_scoped_graph_prompt(
            "Build the first feature",
            &[],
            "initial-setup",
        );

        assert!(prompt.contains("(none — this is the first feature)"));
        assert!(prompt.contains(r#"feature "initial-setup""#));
        assert!(prompt.contains("Build the first feature"));
        // Should still contain output format instructions
        assert!(prompt.contains("implements|depends_on"));
        assert!(prompt.contains("task-{feature-slug}-{task-slug}"));
    }
}
