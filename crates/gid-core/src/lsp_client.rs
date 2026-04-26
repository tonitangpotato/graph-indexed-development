//! LSP client for precise call edge resolution.
//!
//! Spawns language server processes (tsserver, rust-analyzer, pyright) via stdio transport
//! and uses `textDocument/definition`, `textDocument/references`, and `textDocument/implementation`
//! to resolve call sites, find callers, and discover trait implementations.
//! This replaces name-matching heuristics with compiler-level precision (~99% accuracy).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// Tracks the status of a progress token from the LSP server.
#[derive(Debug, Clone)]
struct ProgressTokenStatus {
    ended: bool,
    percentage: Option<u32>,
}

impl ProgressTokenStatus {
    fn new() -> Self {
        Self { ended: false, percentage: None }
    }
    
    fn ended() -> Self {
        Self { ended: true, percentage: Some(100) }
    }
}

/// LSP client over stdio transport (JSON-RPC 2.0).
pub struct LspClient {
    process: Child,
    /// Channel receiver for messages from the reader thread
    msg_rx: mpsc::Receiver<Result<Value, String>>,
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
    opened_files: HashSet<String>,
    /// Active work-done-progress tokens (token → true if "end" received)
    progress_tokens: HashMap<String, ProgressTokenStatus>,
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

/// A language server that's needed but not installed.
#[derive(Debug, Clone)]
pub struct LspMissingServer {
    /// Language ID (e.g., "rust", "python", "typescript")
    pub language_id: String,
    /// Number of files in this language
    pub file_count: usize,
    /// Number of call edges that can't be refined
    pub edge_count: usize,
    /// Suggested install command
    pub install_command: String,
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
    /// LSP returned a definition but `find_closest_node` could not locate a
    /// precise target within the window. The edge is left at its tree-sitter
    /// state (not refined, not removed). See ISS-016.
    pub refinement_skipped: usize,
    /// Language servers that were successfully used
    pub languages_used: Vec<String>,
    /// Language servers needed but not installed
    pub missing_servers: Vec<LspMissingServer>,
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

    /// Return the install command for a given language ID.
    /// Used when an LSP server is needed but not detected.
    pub fn install_suggestion(language_id: &str) -> String {
        match language_id {
            "rust" => "rustup component add rust-analyzer".to_string(),
            "typescript" | "javascript" => "npm install -g typescript-language-server typescript".to_string(),
            "python" => "pip install pyright".to_string(),
            _ => format!("(no known LSP server for '{}')", language_id),
        }
    }

    /// Check which languages in the project have no LSP server available.
    /// Returns a list of missing servers with install suggestions.
    ///
    /// `languages_in_project` is a map of language_id → (file_count, call_edge_count).
    pub fn check_coverage(
        available: &[Self],
        languages_in_project: &HashMap<String, (usize, usize)>,
    ) -> Vec<LspMissingServer> {
        let available_langs: HashSet<&str> = available
            .iter()
            .map(|c| c.language_id.as_str())
            .collect();

        let mut missing = Vec::new();
        for (lang, &(file_count, edge_count)) in languages_in_project {
            // Skip plaintext — no LSP for that
            if lang == "plaintext" {
                continue;
            }
            // Merge JS into TS (tsserver handles both)
            let check_lang = if lang == "javascript" { "typescript" } else { lang.as_str() };
            if !available_langs.contains(check_lang) {
                missing.push(LspMissingServer {
                    language_id: lang.clone(),
                    file_count,
                    edge_count,
                    install_command: Self::install_suggestion(lang),
                });
            }
        }
        missing.sort_by(|a, b| b.edge_count.cmp(&a.edge_count));
        missing
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
        let stdout = process.stdout.take().context("take stdout")?;
        
        // Spawn reader thread: reads LSP messages and sends them through a channel
        let (msg_tx, msg_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_lsp_message(&mut reader) {
                    Ok(msg) => {
                        if msg_tx.send(Ok(msg)).is_err() {
                            break; // Receiver dropped
                        }
                    }
                    Err(e) => {
                        let _ = msg_tx.send(Err(e.to_string()));
                        break;
                    }
                }
            }
        });
        
        // Spawn stderr reader thread to capture server errors  
        let stderr = process.stderr.take().context("take stderr")?;
        let _stderr_handle = std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if line.contains("error") || line.contains("Error") || line.contains("FATAL")
                    || line.contains("WARN") || line.contains("panic")
                {
                    tracing::warn!("[LSP stderr] {}", line);
                } else {
                    tracing::debug!("[LSP stderr] {}", line);
                }
            }
        });

        let mut client = Self {
            process,
            msg_rx,
            writer,
            next_id: 1,
            root_uri: root_uri.clone(),
            _root_dir: root_dir,
            _notifications: Vec::new(),
            _capabilities: Value::Null,
            timeout: Duration::from_secs(30),
            opened_files: HashSet::new(),
            progress_tokens: HashMap::new(),
        };

        // Initialize handshake — use a longer timeout because rust-analyzer
        // can take minutes to process `initialize` for large workspaces
        // (e.g. 587 crates). The normal 30s request timeout is not enough.
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "window": {
                    "workDoneProgress": true
                },
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

        let saved_timeout = client.timeout;
        client.timeout = Duration::from_secs(600); // 10 min for initialize (large projects need full type analysis)
        let resp = client
            .send_request("initialize", init_params)
            .context("LSP initialize")?;
        client.timeout = saved_timeout;

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

    /// Wait for the language server to finish indexing the project.
    ///
    /// Uses the LSP `$/progress` notification protocol:
    /// 1. Server sends `window/workDoneProgress/create` to register a progress token
    /// 2. Server sends `$/progress` with `kind: "begin"` when work starts
    /// 3. Server sends `$/progress` with `kind: "report"` for updates
    /// 4. Server sends `$/progress` with `kind: "end"` when work completes
    ///
    /// We wait until all active progress tokens have reached "end".
    /// Fallback: if no progress notifications arrive within `initial_wait`, we assume
    /// the server either doesn't support progress or finished instantly.
    /// Get a summary of progress tokens for debugging
    pub fn progress_token_summary(&self) -> String {
        if self.progress_tokens.is_empty() {
            return "no tokens".to_string();
        }
        let active: Vec<_> = self.progress_tokens.iter()
            .filter(|(_, status)| !status.ended && status.percentage != Some(100))
            .map(|(token, status)| format!("{}({}%)", token, status.percentage.unwrap_or(0)))
            .collect();
        let done: Vec<_> = self.progress_tokens.iter()
            .filter(|(_, status)| status.ended || status.percentage == Some(100))
            .map(|(token, _)| token.clone())
            .collect();
        format!("{} done, {} active (active: {:?})", done.len(), active.len(), active)
    }

    pub fn wait_until_ready(&mut self, max_wait: Duration) -> Result<()> {
        let deadline = Instant::now() + max_wait;
        // After all tokens have received END, wait this long for new tokens to appear.
        // rust-analyzer fires multiple phases (Roots Scanned → cachePriming → flycheck)
        // with gaps between them, so we need a generous quiescence window.
        let quiescence_duration = Duration::from_secs(15);
        let initial_wait = Duration::from_secs(10);
        let initial_deadline = Instant::now() + initial_wait;
        let mut saw_any_progress = false;
        let mut all_ended_since: Option<Instant> = None;

        eprintln!("[LSP] Waiting for server indexing (max {}s, quiescence {}s)...", 
            max_wait.as_secs(), quiescence_duration.as_secs());

        loop {
            let now = Instant::now();
            if now > deadline {
                let active: Vec<_> = self.progress_tokens.iter()
                    .filter(|(_, status)| !status.ended)
                    .map(|(token, status)| format!("{}({}%)", token, status.percentage.unwrap_or(0)))
                    .collect();
                if !active.is_empty() {
                    eprintln!(
                        "[LSP] Indexing timeout after {}s, {} tokens still active: {:?}",
                        max_wait.as_secs(), active.len(), active
                    );
                }
                break;
            }

            // If we haven't seen any progress and past initial wait, assume ready
            if !saw_any_progress && now > initial_deadline {
                eprintln!("[LSP] No progress notifications received in {}s, assuming ready", initial_wait.as_secs());
                break;
            }

            // Check if ALL tokens have received END notification.
            // Important: percentage==100 is NOT enough — rust-analyzer may fire new
            // phases (cachePriming, flycheck) after Roots Scanned reaches 100%.
            // Only END notifications are authoritative.
            if saw_any_progress && !self.progress_tokens.is_empty() {
                let all_ended = self.progress_tokens.values().all(|status| status.ended);
                // Debug: show blocking tokens periodically
                if !all_ended {
                    let elapsed = max_wait.as_secs().saturating_sub(deadline.saturating_duration_since(now).as_secs());
                    if elapsed.is_multiple_of(15) && elapsed > 0 {
                        for (name, status) in &self.progress_tokens {
                            if !status.ended {
                                eprintln!("[LSP] Waiting for: '{}' pct={:?}", name, status.percentage);
                            }
                        }
                    }
                }
                if all_ended {
                    match all_ended_since {
                        None => {
                            all_ended_since = Some(now);
                            eprintln!("[LSP] All {} tokens ended, waiting {}s for new phases...", 
                                self.progress_tokens.len(), quiescence_duration.as_secs());
                        }
                        Some(since) if now.duration_since(since) >= quiescence_duration => {
                            eprintln!("[LSP] Quiescence achieved ({}s silence), server is ready ({} tokens seen)", 
                                quiescence_duration.as_secs(), self.progress_tokens.len());
                            break;
                        }
                        _ => {} // Still in quiescence wait
                    }
                } else {
                    all_ended_since = None;
                }
            }

            // Try to read a message with a short timeout
            match self.read_message_timeout(Duration::from_millis(200)) {
                Ok(Some(msg)) => {
                    self.handle_server_message(&msg)?;
                    if msg.get("method").and_then(|m| m.as_str()) == Some("$/progress") {
                        saw_any_progress = true;
                    }
                }
                Ok(None) => {
                    // Timeout, no message available — continue polling
                }
                Err(e) => {
                    eprintln!("[LSP] Error reading message during wait: {}", e);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }

        Ok(())
    }

    /// Handle a server-initiated message (notification or request).
    /// Processes `window/workDoneProgress/create` requests and `$/progress` notifications.
    fn handle_server_message(&mut self, msg: &Value) -> Result<()> {
        let method = match msg.get("method").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => return Ok(()), // Not a notification/request
        };

        match method {
            // Server requests to create a progress token — we must respond
            "window/workDoneProgress/create" => {
                if let Some(id) = msg.get("id") {
                    // Extract token
                    let token = msg.get("params")
                        .and_then(|p| p.get("token"))
                        .and_then(|t| {
                            if let Some(s) = t.as_str() {
                                Some(s.to_string())
                            } else {
                                t.as_u64().map(|n| n.to_string())
                            }
                        })
                        .unwrap_or_default();

                    if !token.is_empty() {
                        tracing::debug!("[LSP] Progress token created: {}", token);
                        self.progress_tokens.insert(token.clone(), ProgressTokenStatus::new());
                    }

                    // Respond with success (null result)
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": null
                    });
                    self.write_message(&resp)?;
                }
            }

            // Progress notification — track begin/report/end
            "$/progress" => {
                if let Some(params) = msg.get("params") {
                    let token = params.get("token")
                        .and_then(|t| {
                            if let Some(s) = t.as_str() {
                                Some(s.to_string())
                            } else {
                                t.as_u64().map(|n| n.to_string())
                            }
                        })
                        .unwrap_or_default();

                    let kind = params.get("value")
                        .and_then(|v| v.get("kind"))
                        .and_then(|k| k.as_str())
                        .unwrap_or("");

                    let title = params.get("value")
                        .and_then(|v| v.get("title"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");

                    let message = params.get("value")
                        .and_then(|v| v.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("");

                    match kind {
                        "begin" => {
                            eprintln!("[DEBUG-PROGRESS] BEGIN token='{}' title='{}'", token, title);
                            self.progress_tokens.insert(token, ProgressTokenStatus::new());
                        }
                        "report" => {
                            let pct = params.get("value")
                                .and_then(|v| v.get("percentage"))
                                .and_then(|p| p.as_u64())
                                .map(|p| p as u32);
                            if let Some(pct_val) = pct {
                                eprintln!("[DEBUG-PROGRESS] REPORT token='{}' {}% {}", token, pct_val, message);
                            } else {
                                eprintln!("[DEBUG-PROGRESS] REPORT token='{}' {}", token, message);
                            }
                            // Update percentage tracking
                            if let Some(status) = self.progress_tokens.get_mut(&token) {
                                if let Some(p) = pct {
                                    status.percentage = Some(p);
                                }
                            } else {
                                // Token not previously seen — create entry with percentage
                                let mut status = ProgressTokenStatus::new();
                                status.percentage = pct;
                                self.progress_tokens.insert(token, status);
                            }
                        }
                        "end" => {
                            eprintln!("[DEBUG-PROGRESS] END token='{}' msg='{}'", token, message);
                            self.progress_tokens.insert(token, ProgressTokenStatus::ended());
                        }
                        _ => {}
                    }
                }
            }

            // Other server requests we might need to handle
            "client/registerCapability" => {
                // Respond with success to capability registration requests
                if let Some(id) = msg.get("id") {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": null
                    });
                    self.write_message(&resp)?;
                }
            }

            _ => {
                // Buffer other notifications
                self._notifications.push(msg.clone());
            }
        }

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

        // Debug counter for tracking removal reasons
        static DEBUG_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let count = DEBUG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Response can be Location | Location[] | LocationLink[] | null
        let locations = if resp.is_null() {
            if count < 10 {
                eprintln!("[DEBUG-DEF] NULL response for {}:{}:{}", rel_path, line, character);
            }
            return Ok(None);
        } else if let Some(arr) = resp.as_array() {
            let arr = arr.to_vec();
            if count < 10 {
                eprintln!("[DEBUG-DEF] Array response (len={}) for {}:{}:{}", arr.len(), rel_path, line, character);
            }
            arr
        } else {
            if count < 10 {
                eprintln!("[DEBUG-DEF] Single response for {}:{}:{}", rel_path, line, character);
            }
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
            static OUTSIDE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let oc = OUTSIDE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if oc < 10 {
                eprintln!("[DEBUG-DEF] OUTSIDE project: target_uri={}, root_prefix={}", target_uri, root_prefix);
            }
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
        } else if let Some(arr) = resp.as_array() {
            arr.to_vec()
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
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("LSP response timeout for request id={}", expected_id);
            }

            let msg = match self.msg_rx.recv_timeout(remaining) {
                Ok(Ok(msg)) => msg,
                Ok(Err(e)) => bail!("LSP reader error: {}", e),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    bail!("LSP response timeout for request id={}", expected_id);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("LSP server closed connection");
                }
            };

            // Check if this is our response
            if let Some(id) = msg.get("id") {
                // It has an id — could be our response or a server request
                if msg.get("method").is_some() {
                    // Server request (has both id and method) — handle it
                    self.handle_server_message(&msg)?;
                    continue;
                }

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

            // It's a notification — handle progress, buffer others
            if msg.get("method").is_some() {
                self.handle_server_message(&msg)?;
            }
        }
    }

    /// Try to read a message with a timeout. Returns None if timeout expires.
    fn read_message_timeout(&mut self, timeout: Duration) -> Result<Option<Value>> {
        match self.msg_rx.recv_timeout(timeout) {
            Ok(Ok(msg)) => Ok(Some(msg)),
            Ok(Err(e)) => bail!("LSP reader error: {}", e),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("LSP server closed connection");
            }
        }
    }
}

/// Read one LSP JSON-RPC message from a buffered reader.
/// This is a standalone function used by the reader thread.
fn read_lsp_message(reader: &mut BufReader<std::process::ChildStdout>) -> Result<Value> {
    // Read headers until empty line
    let mut content_length: usize = 0;
    let mut header_line = String::new();

    loop {
        header_line.clear();
        let bytes_read = reader.read_line(&mut header_line)?;
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
    reader.read_exact(&mut body)?;

    let msg: Value = serde_json::from_slice(&body).context("parse LSP JSON body")?;
    Ok(msg)
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

/// Incrementally refine an LSP client by notifying it of file changes from a FileDelta.
/// - Closes deleted files
/// - For modified files: close + re-open with new content
/// - Opens newly added files
///
/// Returns the count of files processed.
pub fn refine_files(
    client: &mut LspClient,
    delta: &super::code_graph::FileDelta,
    root_dir: &Path,
) -> Result<usize> {
    let mut processed: usize = 0;

    // Close deleted files
    for rel_path in &delta.deleted {
        client.close_file(rel_path)?;
        processed += 1;
    }

    // Modified files: close then re-open with new content
    for rel_path in &delta.modified {
        client.close_file(rel_path)?;

        let abs_path = root_dir.join(rel_path);
        let content = std::fs::read_to_string(&abs_path)
            .with_context(|| format!("read modified file: {}", rel_path))?;
        let ext = Path::new(rel_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let lang_id = extension_to_language_id(ext);
        client.open_file(rel_path, &content, lang_id)?;
        processed += 1;
    }

    // Open added files
    for rel_path in &delta.added {
        let abs_path = root_dir.join(rel_path);
        let content = std::fs::read_to_string(&abs_path)
            .with_context(|| format!("read added file: {}", rel_path))?;
        let ext = Path::new(rel_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let lang_id = extension_to_language_id(ext);
        client.open_file(rel_path, &content, lang_id)?;
        processed += 1;
    }

    Ok(processed)
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
        let dist = line.abs_diff(target_line);
        if dist <= tolerance
            && best.as_ref().is_none_or(|(d, _)| dist < *d) {
                best = Some((dist, id.clone()));
            }
    }

    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ISS-016: Ensure the new `refinement_skipped` counter exists, defaults to
    /// zero, and is surfaced in the Debug output so operators can observe it.
    #[test]
    fn test_refinement_stats_has_refinement_skipped_field() {
        let stats = LspRefinementStats::default();
        assert_eq!(stats.refinement_skipped, 0);
        let dbg = format!("{:?}", stats);
        assert!(
            dbg.contains("refinement_skipped"),
            "Debug output missing refinement_skipped: {}",
            dbg
        );
    }

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

    #[test]
    fn test_install_suggestion_known_languages() {
        assert!(LspServerConfig::install_suggestion("rust").contains("rust-analyzer"));
        assert!(LspServerConfig::install_suggestion("typescript").contains("typescript-language-server"));
        assert!(LspServerConfig::install_suggestion("javascript").contains("typescript-language-server"));
        assert!(LspServerConfig::install_suggestion("python").contains("pyright"));
    }

    #[test]
    fn test_install_suggestion_unknown_language() {
        let suggestion = LspServerConfig::install_suggestion("cobol");
        assert!(suggestion.contains("no known LSP"));
    }

    #[test]
    fn test_check_coverage_all_covered() {
        let configs = vec![
            LspServerConfig {
                command: "rust-analyzer".to_string(),
                args: vec![],
                language_id: "rust".to_string(),
                extensions: vec!["rs".to_string()],
            },
        ];
        let mut langs = std::collections::HashMap::new();
        langs.insert("rust".to_string(), (10usize, 50usize));
        let missing = LspServerConfig::check_coverage(&configs, &langs);
        assert!(missing.is_empty());
    }

    #[test]
    fn test_check_coverage_missing_server() {
        let configs = vec![]; // No LSP servers available
        let mut langs = std::collections::HashMap::new();
        langs.insert("rust".to_string(), (10, 50));
        langs.insert("python".to_string(), (5, 20));
        let missing = LspServerConfig::check_coverage(&configs, &langs);
        assert_eq!(missing.len(), 2);
        // Sorted by edge_count descending
        assert_eq!(missing[0].language_id, "rust");
        assert_eq!(missing[0].edge_count, 50);
        assert_eq!(missing[1].language_id, "python");
        assert_eq!(missing[1].edge_count, 20);
        assert!(missing[0].install_command.contains("rust-analyzer"));
        assert!(missing[1].install_command.contains("pyright"));
    }

    #[test]
    fn test_check_coverage_js_covered_by_tsserver() {
        let configs = vec![
            LspServerConfig {
                command: "npx".to_string(),
                args: vec!["typescript-language-server".to_string()],
                language_id: "typescript".to_string(),
                extensions: vec!["ts".to_string(), "js".to_string()],
            },
        ];
        let mut langs = std::collections::HashMap::new();
        langs.insert("javascript".to_string(), (8, 30));
        let missing = LspServerConfig::check_coverage(&configs, &langs);
        // JS should be covered by tsserver
        assert!(missing.is_empty());
    }

    #[test]
    fn test_check_coverage_skips_plaintext() {
        let configs = vec![];
        let mut langs = std::collections::HashMap::new();
        langs.insert("plaintext".to_string(), (100, 0));
        let missing = LspServerConfig::check_coverage(&configs, &langs);
        assert!(missing.is_empty());
    }

    #[test]
    fn test_lsp_missing_server_fields() {
        let m = LspMissingServer {
            language_id: "rust".to_string(),
            file_count: 42,
            edge_count: 1500,
            install_command: "rustup component add rust-analyzer".to_string(),
        };
        assert_eq!(m.language_id, "rust");
        assert_eq!(m.file_count, 42);
        assert_eq!(m.edge_count, 1500);
    }
}
