//! MCP (Model Context Protocol) server for GID
//!
//! Exposes all gid operations as tools and resources via JSON-RPC 2.0 over stdio.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use gid_core::{
    Graph, Node, Edge, NodeStatus,
    load_graph, save_graph,
    parser::find_graph_file,
    query::QueryEngine,
    validator::Validator,
    render, VisualFormat,
    analyze as advise_analyze,
    generate_graph_prompt, parse_llm_response,
};

#[cfg(feature = "ritual")]
use gid_core::ritual::{
    RitualDefinition, RitualEngine,
    TemplateRegistry,
};

// ═══════════════════════════════════════════════════════════════════════════════
// JSON-RPC Types
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// MCP Error codes
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
#[allow(dead_code)]
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

// ═══════════════════════════════════════════════════════════════════════════════
// MCP Tool Definition Types
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct ResourceDefinition {
    uri: String,
    name: String,
    description: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
}

#[derive(Debug, Serialize)]
struct ToolContent {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
}

#[derive(Debug, Serialize)]
struct ToolResult {
    content: Vec<ToolContent>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

impl ToolResult {
    fn text(s: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent {
                content_type: "text",
                text: s.into(),
            }],
            is_error: None,
        }
    }

    fn error(s: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent {
                content_type: "text",
                text: s.into(),
            }],
            is_error: Some(true),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// MCP Server
// ═══════════════════════════════════════════════════════════════════════════════

pub struct McpServer {
    project_root: PathBuf,
}

impl McpServer {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    fn find_graph_path(&self) -> Result<PathBuf> {
        find_graph_file(&self.project_root).context(
            "No graph file found. Use gid_init to create one."
        )
    }

    pub fn run_stdio(&self) -> Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut stdout = stdout.lock();

        eprintln!("[gid-mcp] Server started, project: {}", self.project_root.display());

        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[gid-mcp] Read error: {}", e);
                    break;
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            let response = self.handle_line(&line);
            let response_str = serde_json::to_string(&response).unwrap_or_else(|e| {
                serde_json::to_string(&JsonRpcResponse::error(
                    None,
                    INTERNAL_ERROR,
                    format!("Serialization error: {}", e),
                ))
                .unwrap()
            });

            if let Err(e) = writeln!(stdout, "{}", response_str) {
                eprintln!("[gid-mcp] Write error: {}", e);
                break;
            }
            if let Err(e) = stdout.flush() {
                eprintln!("[gid-mcp] Flush error: {}", e);
                break;
            }
        }

        eprintln!("[gid-mcp] Server stopped");
        Ok(())
    }

    fn handle_line(&self, line: &str) -> JsonRpcResponse {
        let request: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                return JsonRpcResponse::error(
                    None,
                    PARSE_ERROR,
                    format!("Parse error: {}", e),
                );
            }
        };

        if request.jsonrpc != "2.0" {
            return JsonRpcResponse::error(
                request.id,
                INVALID_REQUEST,
                "Invalid JSON-RPC version",
            );
        }

        self.handle_request(request)
    }

    fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone();
        
        match request.method.as_str() {
            "initialize" => self.handle_initialize(id, request.params),
            "initialized" => JsonRpcResponse::success(id, json!({})),
            "tools/list" => self.handle_tools_list(id),
            "tools/call" => self.handle_tools_call(id, request.params),
            "resources/list" => self.handle_resources_list(id),
            "resources/read" => self.handle_resources_read(id, request.params),
            "ping" => JsonRpcResponse::success(id, json!({})),
            _ => JsonRpcResponse::error(id, METHOD_NOT_FOUND, format!("Unknown method: {}", request.method)),
        }
    }

    fn handle_initialize(&self, id: Option<Value>, _params: Value) -> JsonRpcResponse {
        JsonRpcResponse::success(id, json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": {
                "name": "gid",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": {},
                "resources": {},
            }
        }))
    }

    fn handle_tools_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let tools = self.get_tool_definitions();
        JsonRpcResponse::success(id, json!({ "tools": tools }))
    }

    fn handle_tools_call(&self, id: Option<Value>, params: Value) -> JsonRpcResponse {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        let result = self.call_tool(name, arguments);
        
        match result {
            Ok(tool_result) => JsonRpcResponse::success(id, serde_json::to_value(tool_result).unwrap()),
            Err(e) => JsonRpcResponse::success(id, serde_json::to_value(ToolResult::error(e.to_string())).unwrap()),
        }
    }

    fn handle_resources_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let resources = self.get_resource_definitions();
        JsonRpcResponse::success(id, json!({ "resources": resources }))
    }

    fn handle_resources_read(&self, id: Option<Value>, params: Value) -> JsonRpcResponse {
        let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
        
        match self.read_resource(uri) {
            Ok(content) => JsonRpcResponse::success(id, json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "text/plain",
                    "text": content,
                }]
            })),
            Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, e.to_string()),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Tool Definitions
    // ═══════════════════════════════════════════════════════════════════════════

    fn get_tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = vec![
            ToolDefinition {
                name: "gid_read".into(),
                description: "Read the current graph as YAML".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                }),
            },
            ToolDefinition {
                name: "gid_init".into(),
                description: "Initialize a new .gid/graph.yml in the project".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Project name" },
                        "desc": { "type": "string", "description": "Project description" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_add_node".into(),
                description: "Add a new node to the graph".into(),
                input_schema: json!({
                    "type": "object",
                    "required": ["id", "title"],
                    "properties": {
                        "id": { "type": "string", "description": "Unique node ID" },
                        "title": { "type": "string", "description": "Node title" },
                        "status": { "type": "string", "enum": ["todo", "in_progress", "done", "blocked", "cancelled"], "description": "Node status" },
                        "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags" },
                        "desc": { "type": "string", "description": "Description" },
                        "node_type": { "type": "string", "description": "Node type (task, file, component, etc.)" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_add_edge".into(),
                description: "Add an edge between two nodes".into(),
                input_schema: json!({
                    "type": "object",
                    "required": ["from", "to"],
                    "properties": {
                        "from": { "type": "string", "description": "Source node ID" },
                        "to": { "type": "string", "description": "Target node ID" },
                        "relation": { "type": "string", "description": "Relation type (default: depends_on)" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_tasks".into(),
                description: "List tasks with optional status filter".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "status": { "type": "string", "enum": ["todo", "in_progress", "done", "blocked", "cancelled"], "description": "Filter by status" },
                        "ready": { "type": "boolean", "description": "Show only ready tasks (todo with all deps done)" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_task_update".into(),
                description: "Update a task's status".into(),
                input_schema: json!({
                    "type": "object",
                    "required": ["id", "status"],
                    "properties": {
                        "id": { "type": "string", "description": "Node ID" },
                        "status": { "type": "string", "enum": ["todo", "in_progress", "done", "blocked", "cancelled"], "description": "New status" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_query_deps".into(),
                description: "Query dependencies of a node".into(),
                input_schema: json!({
                    "type": "object",
                    "required": ["node_id"],
                    "properties": {
                        "node_id": { "type": "string", "description": "Node ID to query" },
                        "depth": { "type": "integer", "description": "Max depth (0 for direct only, -1 for all)" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_query_impact".into(),
                description: "Query what nodes would be affected by changes to a node".into(),
                input_schema: json!({
                    "type": "object",
                    "required": ["node_id"],
                    "properties": {
                        "node_id": { "type": "string", "description": "Node ID to analyze" },
                        "depth": { "type": "integer", "description": "Max depth (-1 for all)" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_visual".into(),
                description: "Generate visualization of the graph".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "format": { "type": "string", "enum": ["ascii", "mermaid", "dot"], "description": "Output format (default: mermaid)" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_design".into(),
                description: "Generate LLM prompt for graph design from requirements, or parse LLM response".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "requirements": { "type": "string", "description": "Requirements text (for prompt generation)" },
                        "parse": { "type": "string", "description": "LLM response to parse (alternative to requirements)" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_extract".into(),
                description: "Extract code graph from directory".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "dir": { "type": "string", "description": "Directory to extract from (default: project root)" },
                        "format": { "type": "string", "enum": ["summary", "yaml", "json"], "description": "Output format" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_advise".into(),
                description: "Analyze graph and suggest improvements".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "errors_only": { "type": "boolean", "description": "Show only errors" },
                    },
                }),
            },
            ToolDefinition {
                name: "gid_validate".into(),
                description: "Validate graph consistency (cycles, orphans, missing refs)".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                }),
            },
        ];

        // Add ritual tools if feature is enabled
        #[cfg(feature = "ritual")]
        {
            tools.extend(vec![
                ToolDefinition {
                    name: "gid_ritual_init".into(),
                    description: "Initialize a new ritual from a template".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "template": { "type": "string", "description": "Template name (default: full-dev-cycle)" },
                        },
                    }),
                },
                ToolDefinition {
                    name: "gid_ritual_status".into(),
                    description: "Get current ritual status".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                    }),
                },
                ToolDefinition {
                    name: "gid_ritual_templates".into(),
                    description: "List available ritual templates".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                    }),
                },
            ]);
        }

        tools
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Resource Definitions
    // ═══════════════════════════════════════════════════════════════════════════

    fn get_resource_definitions(&self) -> Vec<ResourceDefinition> {
        let mut resources = vec![
            ResourceDefinition {
                uri: "graph://current".into(),
                name: "Current Graph".into(),
                description: "Current project graph as YAML".into(),
                mime_type: "text/yaml".into(),
            },
            ResourceDefinition {
                uri: "graph://tasks".into(),
                name: "Task List".into(),
                description: "List of all tasks with status".into(),
                mime_type: "application/json".into(),
            },
        ];

        #[cfg(feature = "ritual")]
        {
            resources.push(ResourceDefinition {
                uri: "graph://ritual-state".into(),
                name: "Ritual State".into(),
                description: "Current ritual state (if any)".into(),
                mime_type: "application/json".into(),
            });
        }

        resources
    }

    fn read_resource(&self, uri: &str) -> Result<String> {
        match uri {
            "graph://current" => {
                let path = self.find_graph_path()?;
                let graph = load_graph(&path)?;
                Ok(serde_yaml::to_string(&graph)?)
            }
            "graph://tasks" => {
                let path = self.find_graph_path()?;
                let graph = load_graph(&path)?;
                let tasks: Vec<_> = graph.nodes.iter().map(|n| json!({
                    "id": n.id,
                    "title": n.title,
                    "status": n.status.to_string(),
                    "tags": n.tags,
                })).collect();
                Ok(serde_json::to_string_pretty(&tasks)?)
            }
            #[cfg(feature = "ritual")]
            "graph://ritual-state" => {
                let state_path = self.project_root.join(".gid/ritual-state.json");
                if state_path.exists() {
                    Ok(std::fs::read_to_string(&state_path)?)
                } else {
                    Ok(json!({"status": "no_ritual"}).to_string())
                }
            }
            _ => bail!("Unknown resource: {}", uri),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Tool Implementations
    // ═══════════════════════════════════════════════════════════════════════════

    fn call_tool(&self, name: &str, args: Value) -> Result<ToolResult> {
        match name {
            "gid_read" => self.tool_read(),
            "gid_init" => self.tool_init(args),
            "gid_add_node" => self.tool_add_node(args),
            "gid_add_edge" => self.tool_add_edge(args),
            "gid_tasks" => self.tool_tasks(args),
            "gid_task_update" => self.tool_task_update(args),
            "gid_query_deps" => self.tool_query_deps(args),
            "gid_query_impact" => self.tool_query_impact(args),
            "gid_visual" => self.tool_visual(args),
            "gid_design" => self.tool_design(args),
            "gid_extract" => self.tool_extract(args),
            "gid_advise" => self.tool_advise(args),
            "gid_validate" => self.tool_validate(),
            #[cfg(feature = "ritual")]
            "gid_ritual_init" => self.tool_ritual_init(args),
            #[cfg(feature = "ritual")]
            "gid_ritual_status" => self.tool_ritual_status(),
            #[cfg(feature = "ritual")]
            "gid_ritual_templates" => self.tool_ritual_templates(),
            _ => bail!("Unknown tool: {}", name),
        }
    }

    fn tool_read(&self) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let graph = load_graph(&path)?;
        let yaml = serde_yaml::to_string(&graph)?;
        Ok(ToolResult::text(yaml))
    }

    fn tool_init(&self, args: Value) -> Result<ToolResult> {
        let path = self.project_root.join(".gid/graph.yml");
        if path.exists() {
            bail!("Graph file already exists: {}", path.display());
        }

        let name = args.get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                self.project_root
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "project".to_string())
            });
        let desc = args.get("desc").and_then(|v| v.as_str()).map(String::from);

        let graph = Graph {
            project: Some(gid_core::ProjectMeta {
                name: name.clone(),
                description: desc,
            }),
            nodes: Vec::new(),
            edges: Vec::new(),
        };

        std::fs::create_dir_all(self.project_root.join(".gid"))?;
        save_graph(&graph, &path)?;

        Ok(ToolResult::text(json!({
            "success": true,
            "path": path.display().to_string(),
            "project": name,
        }).to_string()))
    }

    fn tool_add_node(&self, args: Value) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let mut graph = load_graph(&path)?;

        let id = args.get("id").and_then(|v| v.as_str())
            .context("Missing required parameter: id")?;
        let title = args.get("title").and_then(|v| v.as_str())
            .context("Missing required parameter: title")?;

        if graph.get_node(id).is_some() {
            bail!("Node already exists: {}", id);
        }

        let mut node = Node::new(id, title);
        
        if let Some(desc) = args.get("desc").and_then(|v| v.as_str()) {
            node.description = Some(desc.to_string());
        }
        if let Some(status) = args.get("status").and_then(|v| v.as_str()) {
            node.status = status.parse()?;
        }
        if let Some(tags) = args.get("tags").and_then(|v| v.as_array()) {
            node.tags = tags.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        if let Some(nt) = args.get("node_type").and_then(|v| v.as_str()) {
            node.node_type = Some(nt.to_string());
        }

        graph.add_node(node);
        save_graph(&graph, &path)?;

        Ok(ToolResult::text(json!({
            "success": true,
            "id": id,
        }).to_string()))
    }

    fn tool_add_edge(&self, args: Value) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let mut graph = load_graph(&path)?;

        let from = args.get("from").and_then(|v| v.as_str())
            .context("Missing required parameter: from")?;
        let to = args.get("to").and_then(|v| v.as_str())
            .context("Missing required parameter: to")?;
        let relation = args.get("relation")
            .and_then(|v| v.as_str())
            .unwrap_or("depends_on");

        if graph.get_node(from).is_none() {
            bail!("Source node not found: {}", from);
        }
        if graph.get_node(to).is_none() {
            bail!("Target node not found: {}", to);
        }

        if relation == "depends_on" {
            let validator = Validator::new(&graph);
            if validator.would_create_cycle(from, to) {
                bail!("Adding this edge would create a cycle");
            }
        }

        graph.add_edge(Edge::new(from, to, relation));
        save_graph(&graph, &path)?;

        Ok(ToolResult::text(json!({
            "success": true,
            "from": from,
            "to": to,
            "relation": relation,
        }).to_string()))
    }

    fn tool_tasks(&self, args: Value) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let graph = load_graph(&path)?;

        let ready_only = args.get("ready").and_then(|v| v.as_bool()).unwrap_or(false);
        let status_filter = args.get("status").and_then(|v| v.as_str());

        let tasks: Vec<&Node> = if ready_only {
            graph.ready_tasks()
        } else if let Some(status_str) = status_filter {
            let status: NodeStatus = status_str.parse()?;
            graph.tasks_by_status(&status)
        } else {
            graph.nodes.iter().collect()
        };

        let tasks_json: Vec<_> = tasks.iter().map(|t| json!({
            "id": t.id,
            "title": t.title,
            "status": t.status.to_string(),
            "tags": t.tags,
            "description": t.description,
        })).collect();

        let summary = graph.summary();
        let result = json!({
            "tasks": tasks_json,
            "summary": {
                "total": summary.total_nodes,
                "todo": summary.todo,
                "in_progress": summary.in_progress,
                "done": summary.done,
                "blocked": summary.blocked,
                "ready": summary.ready,
            }
        });

        Ok(ToolResult::text(serde_json::to_string_pretty(&result)?))
    }

    fn tool_task_update(&self, args: Value) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let mut graph = load_graph(&path)?;

        let id = args.get("id").and_then(|v| v.as_str())
            .context("Missing required parameter: id")?;
        let status_str = args.get("status").and_then(|v| v.as_str())
            .context("Missing required parameter: status")?;

        let status: NodeStatus = status_str.parse()?;

        if !graph.update_status(id, status.clone()) {
            bail!("Node not found: {}", id);
        }

        save_graph(&graph, &path)?;

        Ok(ToolResult::text(json!({
            "success": true,
            "id": id,
            "status": status.to_string(),
        }).to_string()))
    }

    fn tool_query_deps(&self, args: Value) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let graph = load_graph(&path)?;

        let node_id = args.get("node_id").and_then(|v| v.as_str())
            .context("Missing required parameter: node_id")?;
        let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(-1);

        if graph.get_node(node_id).is_none() {
            bail!("Node not found: {}", node_id);
        }

        let engine = QueryEngine::new(&graph);
        let transitive = depth != 0;
        let deps = engine.deps(node_id, transitive);

        let nodes: Vec<_> = deps.iter().map(|n| json!({
            "id": n.id,
            "title": n.title,
            "status": n.status.to_string(),
        })).collect();

        Ok(ToolResult::text(json!({
            "node": node_id,
            "transitive": transitive,
            "dependencies": nodes,
        }).to_string()))
    }

    fn tool_query_impact(&self, args: Value) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let graph = load_graph(&path)?;

        let node_id = args.get("node_id").and_then(|v| v.as_str())
            .context("Missing required parameter: node_id")?;

        if graph.get_node(node_id).is_none() {
            bail!("Node not found: {}", node_id);
        }

        let engine = QueryEngine::new(&graph);
        let impacted = engine.impact(node_id);

        let nodes: Vec<_> = impacted.iter().map(|n| json!({
            "id": n.id,
            "title": n.title,
        })).collect();

        Ok(ToolResult::text(json!({
            "node": node_id,
            "impacted": nodes,
        }).to_string()))
    }

    fn tool_visual(&self, args: Value) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let graph = load_graph(&path)?;

        let format = args.get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("mermaid");
        
        let fmt: VisualFormat = format.parse()?;
        let result = render(&graph, fmt);

        Ok(ToolResult::text(result))
    }

    fn tool_design(&self, args: Value) -> Result<ToolResult> {
        if let Some(response) = args.get("parse").and_then(|v| v.as_str()) {
            // Parse LLM response
            let graph = parse_llm_response(response)?;
            let yaml = serde_yaml::to_string(&graph)?;
            Ok(ToolResult::text(yaml))
        } else if let Some(requirements) = args.get("requirements").and_then(|v| v.as_str()) {
            // Generate prompt
            let prompt = generate_graph_prompt(requirements);
            Ok(ToolResult::text(prompt))
        } else {
            bail!("Either 'requirements' or 'parse' parameter is required");
        }
    }

    fn tool_extract(&self, args: Value) -> Result<ToolResult> {
        use gid_core::{CodeGraph, build_unified_graph};

        let dir = args.get("dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.project_root.clone());

        let format = args.get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("summary");

        let code_graph = CodeGraph::extract_from_dir(&dir);
        let task_graph = Graph::default();
        let unified = build_unified_graph(&code_graph, &task_graph);

        let output = match format {
            "yaml" | "yml" => serde_yaml::to_string(&unified)?,
            "json" => serde_json::to_string_pretty(&unified)?,
            "summary" | _ => {
                let file_count = unified.nodes.iter()
                    .filter(|n| n.node_type.as_deref() == Some("file"))
                    .count();
                let class_count = unified.nodes.iter()
                    .filter(|n| n.node_type.as_deref() == Some("class"))
                    .count();
                let func_count = unified.nodes.iter()
                    .filter(|n| n.node_type.as_deref() == Some("function"))
                    .count();

                format!(
                    "Code Graph Summary\n{}\n\n📊 {} files, {} classes/structs, {} functions\n🔗 {} edges",
                    "=".repeat(50),
                    file_count, class_count, func_count,
                    unified.edges.len()
                )
            }
        };

        Ok(ToolResult::text(output))
    }

    fn tool_advise(&self, args: Value) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let graph = load_graph(&path)?;

        let errors_only = args.get("errors_only").and_then(|v| v.as_bool()).unwrap_or(false);

        let mut result = advise_analyze(&graph);
        
        if errors_only {
            result.items.retain(|a| a.severity == gid_core::Severity::Error);
        }

        Ok(ToolResult::text(serde_json::to_string_pretty(&result)?))
    }

    fn tool_validate(&self) -> Result<ToolResult> {
        let path = self.find_graph_path()?;
        let graph = load_graph(&path)?;
        let validator = Validator::new(&graph);
        let result = validator.validate();

        let response = json!({
            "valid": result.is_valid(),
            "issues": result.issue_count(),
            "orphan_nodes": result.orphan_nodes,
            "missing_refs": result.missing_refs.iter().map(|r| {
                json!({"from": r.edge_from, "to": r.edge_to, "missing": r.missing_node})
            }).collect::<Vec<_>>(),
            "cycles": result.cycles,
            "duplicate_nodes": result.duplicate_nodes,
        });

        Ok(ToolResult::text(serde_json::to_string_pretty(&response)?))
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Ritual Tools (feature-gated)
    // ═══════════════════════════════════════════════════════════════════════════

    #[cfg(feature = "ritual")]
    fn tool_ritual_init(&self, args: Value) -> Result<ToolResult> {
        let ritual_path = self.project_root.join(".gid/ritual.yml");
        if ritual_path.exists() {
            bail!("Ritual already exists: {}", ritual_path.display());
        }

        let template_name = args.get("template")
            .and_then(|v| v.as_str())
            .unwrap_or("full-dev-cycle");

        let registry = TemplateRegistry::for_project(&self.project_root);
        let template = registry.load(template_name)
            .with_context(|| format!("Template not found: {}", template_name))?;

        std::fs::create_dir_all(self.project_root.join(".gid"))?;
        let yaml = serde_yaml::to_string(&template)?;
        std::fs::write(&ritual_path, &yaml)?;

        Ok(ToolResult::text(json!({
            "success": true,
            "path": ritual_path.display().to_string(),
            "template": template_name,
            "phases": template.phases.len(),
        }).to_string()))
    }

    #[cfg(feature = "ritual")]
    fn tool_ritual_status(&self) -> Result<ToolResult> {
        let ritual_path = self.project_root.join(".gid/ritual.yml");
        let state_path = self.project_root.join(".gid/ritual-state.json");

        if !ritual_path.exists() {
            return Ok(ToolResult::text(json!({
                "exists": false,
                "message": "No ritual configured. Use gid_ritual_init to create one.",
            }).to_string()));
        }

        let template_dirs = vec![self.project_root.join(".gid/rituals/")];
        let definition = RitualDefinition::load(&ritual_path, &template_dirs)?;

        if !state_path.exists() {
            return Ok(ToolResult::text(json!({
                "exists": true,
                "running": false,
                "name": definition.name,
                "phases": definition.phases.len(),
            }).to_string()));
        }

        let engine = RitualEngine::resume(definition, &self.project_root)?;
        let state = engine.state();

        Ok(ToolResult::text(serde_json::to_string_pretty(&state)?))
    }

    #[cfg(feature = "ritual")]
    fn tool_ritual_templates(&self) -> Result<ToolResult> {
        let registry = TemplateRegistry::for_project(&self.project_root);
        let templates = registry.list()?;

        Ok(ToolResult::text(serde_json::to_string_pretty(&templates)?))
    }
}

/// Entry point for `gid mcp` command
pub fn cmd_mcp(project_root: &Path) -> Result<()> {
    let server = McpServer::new(project_root.to_path_buf());
    server.run_stdio()
}
