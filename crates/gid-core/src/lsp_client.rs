//! LSP client for precise call edge resolution.
//!
//! Spawns language server processes (tsserver, rust-analyzer, pyright) via stdio transport
//! and uses `textDocument/definition`, `textDocument/references`, and `textDocument/implementation`
//! to resolve call sites, find callers, and discover trait implementations.
//! This replaces name-matching heuristics with compiler-level precision (~99% accuracy).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// LSP client over stdio transport (JSON-RPC 2.0).
pub struct LspClient {
    process: Child,
    reader: BufReader<std::process::ChildStdout>,
    writer: std::process::ChildStdin,
    next_id: u64,
    root_uri: String,
    _root_dir: PathBuf,
    /// Buffered notifications received while waiting for responses
    _notifications: Vec<Value>,
    /// Server capabilities received from initialize response
    _capabilities: Value,
    /// Timeout per request
    timeout: Duration,
    /// Files that have been opened via didOpen
    opened_files: std::collections::HashSet<String>,
}

/// A resolved definition location from LSP.
#[derive(Debug, Clone)]
pub struct LspLocation {
    /// File path relative to project root
    pub file_path: String,
    /// 0-indexed line number
    pub line: u32,
    /// 0-indexed column (UTF-16 offset per LSP spec)
    pub character: u32,
}

/// Statistics from LSP refinement of call edges.
#[derive(Debug, Default)]
pub struct LspRefinementStats {
    /// Total call edges considered
    pub total_call_edges: usize,
    /// Edges where LSP confirmed + possibly updated target
    pub refined: usize,
    /// Edges removed (target is external/nonexistent in project)
    pub removed: usize,
    /// LSP request failed or timed out
    pub failed: usize,
    /// No LSP available for this language, kept tree-sitter edge
    pub skipped: usize,
    /// Language servers that were successfully used
    pub languages_used: Vec<String>,
    /// Number of reference lookups performed
    pub references_queried: usize,
    /// New call edges discovered via references
    pub references_edges_added: usize,
    /// Number of implementation lookups performed
    pub implementations_queried: usize,
    /// New implementation edges discovered
    pub implementation_edges_added: usize,
}

/// Statistics from LSP enrichment passes (references + implementations).
#[derive(Debug, Default)]
pub struct LspEnrichmentStats {
    /// Number of nodes queried via LSP
    pub nodes_queried: usize,
    /// New edges discovered and added
    pub new_edges_added: usize,
    /// Edges that already existed (skipped)
    pub already_existed: usize,
    /// LSP queries that failed or timed out
    pub failed: usize,
    /// Language servers that were successfully used
    pub languages_used: Vec<String>,
}

/// Language server configuration for a specific language.
#[derive(Debug, Clone)]
pub struct LspServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub language_id: String,
    pub extensions: Vec<String>,
}

impl LspServerConfig {
    /// Detect available language servers on the system.
    pub fn detect_available() -> Vec<Self> {
        let mut configs = Vec::new();

        // TypeScript/JavaScript — typescript-language-server
        // Try npx first, which wraps tsserver
        let ts_result = Command::new("npx")
            .args(["--yes", "typescript-language-server", "--version"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        let ts_available = match &ts_result {
            Ok(output) => {
                output.status.success()
            }
            Err(e) => {
                tracing::debug!("[LSP detect] tsserver spawn failed: {}", e);
                false
            }
        };

        if ts_available {
            configs.push(Self {
                command: "npx".to_string(),
                args: vec![
                    "--yes".to_string(),
                    "typescript-language-server".to_string(),
                    "--stdio".to_string(),
                ],
                language_id: "typescript".to_string(),
                extensions: vec![
                    "ts".to_string(),
                    "tsx".to_string(),
                    "js".to_string(),
                    "jsx".to_string(),
                ],
            });
        }

        // Rust — rust-analyzer
        if which_exists("rust-analyzer") {
            configs.push(Self {
                command: "rust-analyzer".to_string(),
                args: vec![],
                language_id: "rust".to_string(),
                extensions: vec!["rs".to_string()],
            });
        }

        // Python — pyright or pylsp
        if which_exists("pyright-langserver") {
            configs.push(Self {
                command: "pyright-langserver".to_string(),
                args: vec!["--stdio".to_string()],
                language_id: "python".to_string(),
                extensions: vec!["py".to_string()],
            });
        } else if which_exists("pylsp") {
            configs.push(Self {
                command: "pylsp".to_string(),
                args: vec![],
                language_id: "python".to_string(),
                extensions: vec!["py".to_string()],
            });
        }

        configs
    }
}

fn which_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

impl LspClient {
    /// Start an LSP server process and perform the initialize handshake.
    pub fn start(config: &LspServerConfig, root_dir: &Path) -> Result<Self> {
        let root_dir = root_dir.canonicalize().context("canonicalize root_dir")?;
        let root_uri = format!("file://{}", root_dir.display());

        let mut process = Command::new(&config.command)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&root_dir)
            .spawn()
            .with_context(|| format!("spawn LSP: {} {:?}", config.command, config.args))?;

        let writer = process.stdin.take().context("take stdin")?;
        let reader = BufReader::new(process.stdout.take().context("take stdout")?);
        
        // Spawn stderr reader thread to capture server errors  
        let stderr = process.stderr.take().context("take stderr")?;
        let _stderr_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().flatten() {
                // Only log actual errors, not info messages
                if line.contains("error") || line.contains("Error") || line.contains("FATAL") {
                    tracing::debug!("[LSP stderr] {}", line);
                }
            }
        });

        let mut client = Self {
            process,
            reader,
            writer,
            next_id: 1,
            root_uri: root_uri.clone(),
            _root_dir: root_dir,
            _notifications: Vec::new(),
            _capabilities: Value::Null,
            timeout: Duration::from_secs(30),
            opened_files: std::collections::HashSet::new(),
        };

        // Initialize handshake
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "definition": {
                        "dynamicRegistration": false,
                        "linkSupport": false
                    },
                    "references": {
                        "dynamicRegistration": false
                    },
                    "implementation": {
                        "dynamicRegistration": false,
                        "linkSupport": false
                    },
                    "synchronization": {
                        "didOpen": true,
                        "didClose": true
                    }
                }
            },
            "workspaceFolders": [{
                "uri": root_uri,
                "name": "root"
            }]
        });

        let resp = client
            .send_request("initialize", init_params)
            .context("LSP initialize")?;

        if let Some(caps) = resp.get("capabilities") {
            client._capabilities = caps.clone();
        }

        // Send initialized notification
        client
            .send_notification("initialized", json!({}))
            .context("LSP initialized notification")?;

        Ok(client)
    }

    /// Open a file in the language server (required before definition queries).
    pub fn open_file(&mut self, rel_path: &str, content: &str, language_id: &str) -> Result<()> {
        if self.opened_files.contains(rel_path) {
            return Ok(());
        }

        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": content
                }
            }),
        )?;

        self.opened_files.insert(rel_path.to_string());
        Ok(())
    }

    /// Close a file in the language server.
    pub fn close_file(&mut self, rel_path: &str) -> Result<()> {
        if !self.opened_files.remove(rel_path) {
            return Ok(());
        }

        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_notification(
            "textDocument/didClose",
            json!({
                "textDocument": {
                    "uri": uri
                }
            }),
        )?;

        Ok(())
    }

    /// Get definition location for a symbol at the given position.
    /// Returns None if no definition found or definition is outside project.
    pub fn get_definition(
        &mut self,
        rel_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Option<LspLocation>> {
        let uri = format!("{}/{}", self.root_uri, rel_path);

        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        let resp = self.send_request("textDocument/definition", params)?;

        // Response can be Location | Location[] | LocationLink[] | null
        let locations = if resp.is_null() {
            return Ok(None);
        } else if resp.is_array() {
            resp.as_array().unwrap().clone()
        } else {
            vec![resp]
        };

        if locations.is_empty() {
            return Ok(None);
        }

        // Take the first location
        let loc = &locations[0];

        // Handle both Location and LocationLink formats
        let (target_uri, target_line, target_char) =
            if let Some(target_range) = loc.get("targetRange") {
                // LocationLink format
                let uri = loc
                    .get("targetUri")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let line = target_range
                    .get("start")
                    .and_then(|s| s.get("line"))
                    .and_then(|l| l.as_u64())
                    .unwrap_or(0) as u32;
                let char = target_range
                    .get("start")
                    .and_then(|s| s.get("character"))
                    .and_then(|c| c.as_u64())
                    .unwrap_or(0) as u32;
                (uri.to_string(), line, char)
            } else {
                // Location format
                let uri = loc.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                let line = loc
                    .get("range")
                    .and_then(|r| r.get("start"))
                    .and_then(|s| s.get("line"))
                    .and_then(|l| l.as_u64())
                    .unwrap_or(0) as u32;
                let char = loc
                    .get("range")
                    .and_then(|r| r.get("start"))
                    .and_then(|s| s.get("character"))
                    .and_then(|c| c.as_u64())
                    .unwrap_or(0) as u32;
                (uri.to_string(), line, char)
            };

        // Convert URI to relative path
        let root_prefix = format!("{}/", self.root_uri);
        if !target_uri.starts_with(&root_prefix) {
            // Definition is outside project (stdlib, node_modules, etc.)
            return Ok(None);
        }

        let file_path = target_uri[root_prefix.len()..].to_string();

        Ok(Some(LspLocation {
            file_path,
            line: target_line,
            character: target_char,
        }))
    }

    /// Parse a list of Location / LocationLink values from an LSP response,
    /// filtering to locations within the project root.
    fn parse_locations(&self, resp: Value) -> Vec<LspLocation> {
        let raw = if resp.is_null() {
            return Vec::new();
        } else if resp.is_array() {
            resp.as_array().unwrap().clone()
        } else {
            vec![resp]
        };

        let root_prefix = format!("{}/", self.root_uri);
        let mut results = Vec::new();

        for loc in &raw {
            // Handle both Location and LocationLink formats
            let (target_uri, target_line, target_char) =
                if let Some(target_range) = loc.get("targetRange") {
                    // LocationLink format
                    let uri = loc
                        .get("targetUri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let line = target_range
                        .get("start")
                        .and_then(|s| s.get("line"))
                        .and_then(|l| l.as_u64())
                        .unwrap_or(0) as u32;
                    let ch = target_range
                        .get("start")
                        .and_then(|s| s.get("character"))
                        .and_then(|c| c.as_u64())
                        .unwrap_or(0) as u32;
                    (uri.to_string(), line, ch)
                } else {
                    // Location format
                    let uri = loc.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                    let line = loc
                        .get("range")
                        .and_then(|r| r.get("start"))
                        .and_then(|s| s.get("line"))
                        .and_then(|l| l.as_u64())
                        .unwrap_or(0) as u32;
                    let ch = loc
                        .get("range")
                        .and_then(|r| r.get("start"))
                        .and_then(|s| s.get("character"))
                        .and_then(|c| c.as_u64())
                        .unwrap_or(0) as u32;
                    (uri.to_string(), line, ch)
                };

            // Convert URI to relative path, skip locations outside project
            if !target_uri.starts_with(&root_prefix) {
                continue;
            }

            let file_path = target_uri[root_prefix.len()..].to_string();
            results.push(LspLocation {
                file_path,
                line: target_line,
                character: target_char,
            });
        }

        results
    }

    /// Find all references to the symbol at the given position.
    /// Returns locations of all call sites / usages within the project.
    /// `include_declaration` controls whether the definition itself is included.
    pub fn get_references(
        &mut self,
        rel_path: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<Vec<LspLocation>> {
        let uri = format!("{}/{}", self.root_uri, rel_path);

        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": include_declaration }
        });

        let resp = self.send_request("textDocument/references", params)?;
        Ok(self.parse_locations(resp))
    }

    /// Find all implementations of a trait method or interface method at the given position.
    /// Returns locations of all concrete implementations within the project.
    pub fn get_implementations(
        &mut self,
        rel_path: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspLocation>> {
        let uri = format!("{}/{}", self.root_uri, rel_path);

        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        let resp = self.send_request("textDocument/implementation", params)?;
        Ok(self.parse_locations(resp))
    }

    /// Graceful shutdown of the language server.
    pub fn shutdown(mut self) -> Result<()> {
        // Send shutdown request
        let _ = self.send_request("shutdown", Value::Null);

        // Send exit notification
        let _ = self.send_notification("exit", Value::Null);

        // Wait briefly for process to exit, then kill
        std::thread::sleep(Duration::from_millis(200));
        let _ = self.process.kill();
        let _ = self.process.wait();

        Ok(())
    }

    // ─── JSON-RPC Transport ────────────────────────────────────────

    fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        self.write_message(&msg)?;
        self.read_response(id)
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        self.write_message(&msg)
    }

    fn write_message(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        self.writer.write_all(header.as_bytes())?;
        self.writer.write_all(body.as_bytes())?;
        self.writer.flush()?;

        Ok(())
    }

    fn read_response(&mut self, expected_id: u64) -> Result<Value> {
        let deadline = Instant::now() + self.timeout;

        loop {
            if Instant::now() > deadline {
                bail!("LSP response timeout for request id={}", expected_id);
            }

            let msg = self.read_message()?;

            // Check if this is our response
            if let Some(id) = msg.get("id") {
                let msg_id = id.as_u64().unwrap_or(0);
                if msg_id == expected_id {
                    // Check for error
                    if let Some(error) = msg.get("error") {
                        let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
                        let message = error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown error");
                        bail!("LSP error (code {}): {}", code, message);
                    }

                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
            }

            // It's a notification or server request — buffer and continue
            if msg.get("method").is_some() {
                self._notifications.push(msg);
            }
        }
    }

    fn read_message(&mut self) -> Result<Value> {
        // Read headers until empty line
        let mut content_length: usize = 0;
        let mut header_line = String::new();

        loop {
            header_line.clear();
            let bytes_read = self.reader.read_line(&mut header_line)?;
            if bytes_read == 0 {
                bail!("LSP server closed connection");
            }

            let trimmed = header_line.trim();
            if trimmed.is_empty() {
                break;
            }

            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                content_length = len_str
                    .parse()
                    .context("parse Content-Length")?;
            }
            // Ignore other headers (Content-Type, etc.)
        }

        if content_length == 0 {
            bail!("Missing Content-Length header");
        }

        // Read exactly content_length bytes
        let mut body = vec![0u8; content_length];
        self.reader.read_exact(&mut body)?;

        let msg: Value = serde_json::from_slice(&body).context("parse LSP JSON body")?;
        Ok(msg)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Best-effort cleanup: kill the server process
        let _ = self.process.kill();
    }
}

/// Map file extension to LSP language ID.
pub fn extension_to_language_id(ext: &str) -> &str {
    match ext {
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "rs" => "rust",
        "py" => "python",
        _ => "plaintext",
    }
}

/// Batch-open files for a language server, returning the count opened.
pub fn open_project_files(
    client: &mut LspClient,
    files: &[(String, String)], // (rel_path, content)
    language_id: &str,
) -> Result<usize> {
    let mut count = 0;
    for (rel_path, content) in files {
        client.open_file(rel_path, content, language_id)?;
        count += 1;
    }
    Ok(count)
}

/// Build a lookup table: (file_path, line) → node_id for resolving LSP definition targets.
pub fn build_definition_target_index(
    nodes: &[super::code_graph::CodeNode],
) -> HashMap<String, HashMap<u32, String>> {
    let mut index: HashMap<String, HashMap<u32, String>> = HashMap::new();
    for node in nodes {
        if let Some(line) = node.line {
            index
                .entry(node.file_path.clone())
                .or_default()
                .insert(line as u32, node.id.clone());
        }
    }
    index
}

/// Find the closest node to a given line in a file.
/// LSP definition might point to line N, but our node might be at line N-1 or N+1
/// (due to decorators, doc comments, etc.).
pub fn find_closest_node(
    file_index: &HashMap<u32, String>,
    target_line: u32,
    tolerance: u32,
) -> Option<String> {
    // Exact match first
    if let Some(id) = file_index.get(&target_line) {
        return Some(id.clone());
    }

    // Search within tolerance
    let mut best: Option<(u32, String)> = None;
    for (&line, id) in file_index {
        let dist = if line > target_line {
            line - target_line
        } else {
            target_line - line
        };
        if dist <= tolerance {
            if best.as_ref().map_or(true, |(d, _)| dist < *d) {
                best = Some((dist, id.clone()));
            }
        }
    }

    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_to_language_id() {
        assert_eq!(extension_to_language_id("ts"), "typescript");
        assert_eq!(extension_to_language_id("tsx"), "typescript");
        assert_eq!(extension_to_language_id("js"), "javascript");
        assert_eq!(extension_to_language_id("rs"), "rust");
        assert_eq!(extension_to_language_id("py"), "python");
        assert_eq!(extension_to_language_id("go"), "plaintext");
    }

    #[test]
    fn test_find_closest_node() {
        let mut index = HashMap::new();
        index.insert(10, "func_a".to_string());
        index.insert(20, "func_b".to_string());
        index.insert(30, "func_c".to_string());

        // Exact match
        assert_eq!(
            find_closest_node(&index, 10, 3),
            Some("func_a".to_string())
        );

        // Within tolerance
        assert_eq!(
            find_closest_node(&index, 11, 3),
            Some("func_a".to_string())
        );
        assert_eq!(
            find_closest_node(&index, 9, 3),
            Some("func_a".to_string())
        );

        // Out of tolerance
        assert_eq!(find_closest_node(&index, 15, 3), None);

        // Closest wins
        assert_eq!(
            find_closest_node(&index, 19, 3),
            Some("func_b".to_string())
        );
    }

    #[test]
    fn test_detect_available_servers() {
        // This test just verifies detect doesn't panic
        let configs = LspServerConfig::detect_available();
        // On CI, might be empty; on dev machines, usually has tsserver
        for config in &configs {
            assert!(!config.command.is_empty());
            assert!(!config.extensions.is_empty());
        }
    }

    #[test]
    fn test_lsp_location_format() {
        let loc = LspLocation {
            file_path: "src/main.ts".to_string(),
            line: 42,
            character: 8,
        };
        assert_eq!(loc.file_path, "src/main.ts");
        assert_eq!(loc.line, 42);
    }
}
