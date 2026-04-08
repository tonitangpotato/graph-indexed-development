//! LSP Daemon — Persistent language server process manager.
//!
//! Keeps rust-analyzer (and other LSP servers) alive between `gid extract` calls
//! so the expensive cold-start analysis (~5-10 min for large projects) only happens once.
//!
//! Architecture:
//! - First `gid extract --lsp` spawns LSP processes and saves their PIDs to `.gid/lsp-daemon/`
//! - Subsequent calls reuse the running processes via a proxy that multiplexes stdio
//! - `gid lsp stop` or timeout (1 hour idle) shuts them down
//!
//! Since LSP servers use stdio transport (not sockets), we can't share a running process's
//! stdin/stdout across invocations. Instead we keep the LspClient in-process and expose
//! a file-based lock so only one `gid extract` runs LSP at a time, while the client
//! persists via a long-running background process.
//!
//! Simpler approach chosen: **socket-based daemon process**.
//! A background process owns the LSP servers and accepts commands over a Unix socket.
//! Each `gid extract` connects to the daemon socket and sends definition/reference queries.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::code_graph::FileDelta;
use crate::lsp_client::{LspClient, LspLocation, LspServerConfig, extension_to_language_id};

// ═══════════════════════════════════════════════════════════════════════════════
// Protocol: messages between client and daemon over Unix socket
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DaemonRequest {
    /// Check if daemon is alive and has a ready LSP for this language.
    Ping { lang_id: String },
    /// Open a file in the LSP server.
    OpenFile {
        lang_id: String,
        rel_path: String,
        content: String,
    },
    /// Get definition at position.
    GetDefinition {
        lang_id: String,
        rel_path: String,
        line: u32,
        character: u32,
    },
    /// Get references at position.
    GetReferences {
        lang_id: String,
        rel_path: String,
        line: u32,
        character: u32,
        include_declaration: bool,
    },
    /// Get implementations at position.
    GetImplementations {
        lang_id: String,
        rel_path: String,
        line: u32,
        character: u32,
    },
    /// Close a file.
    CloseFile {
        lang_id: String,
        rel_path: String,
    },
    /// Shut down a specific language server.
    ShutdownLang { lang_id: String },
    /// Shut down all servers and exit daemon.
    ShutdownAll,
    /// Get status of all managed servers.
    Status,
    /// Incrementally refine: notify LSP of added/modified/deleted files.
    RefineIncremental {
        added: Vec<String>,
        modified: Vec<String>,
        deleted: Vec<String>,
        root_dir: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DaemonResponse {
    Ok,
    Pong {
        ready: bool,
        uptime_secs: u64,
    },
    Definition {
        location: Option<LocationDto>,
    },
    References {
        locations: Vec<LocationDto>,
    },
    Implementations {
        locations: Vec<LocationDto>,
    },
    Status {
        servers: Vec<ServerStatus>,
    },
    Error {
        message: String,
    },
    Refined {
        files_processed: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocationDto {
    file_path: String,
    line: u32,
    character: u32,
}

impl From<LspLocation> for LocationDto {
    fn from(loc: LspLocation) -> Self {
        Self {
            file_path: loc.file_path,
            line: loc.line,
            character: loc.character,
        }
    }
}

impl From<LocationDto> for LspLocation {
    fn from(dto: LocationDto) -> Self {
        Self {
            file_path: dto.file_path,
            line: dto.line,
            character: dto.character,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ServerStatus {
    lang_id: String,
    ready: bool,
    uptime_secs: u64,
    files_opened: usize,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Daemon server — runs as a background process
// ═══════════════════════════════════════════════════════════════════════════════

struct ManagedServer {
    client: LspClient,
    started_at: Instant,
    last_used: Instant,
    files_opened: usize,
}

/// The daemon process that owns LSP servers.
pub struct LspDaemon {
    servers: HashMap<String, ManagedServer>,
    project_root: PathBuf,
    socket_path: PathBuf,
    idle_timeout: Duration,
}

/// Get the daemon socket path for a project.
pub fn daemon_socket_path(project_root: &Path) -> PathBuf {
    project_root.join(".gid").join("lsp-daemon.sock")
}

/// Get the daemon PID file path.
pub fn daemon_pid_path(project_root: &Path) -> PathBuf {
    project_root.join(".gid").join("lsp-daemon.pid")
}

impl LspDaemon {
    /// Create a new daemon for the given project root.
    pub fn new(project_root: &Path) -> Self {
        let socket_path = daemon_socket_path(project_root);
        Self {
            servers: HashMap::new(),
            project_root: project_root.to_path_buf(),
            socket_path,
            idle_timeout: Duration::from_secs(3600), // 1 hour idle timeout
        }
    }

    /// Start a language server if not already running.
    pub fn ensure_server(&mut self, lang_id: &str) -> Result<()> {
        if self.servers.contains_key(lang_id) {
            return Ok(());
        }

        let configs = LspServerConfig::detect_available();
        let config = configs
            .iter()
            .find(|c| c.language_id == lang_id)
            .ok_or_else(|| anyhow::anyhow!("No LSP server available for {}", lang_id))?;

        eprintln!("[LSP daemon] Starting {} server for {}...", lang_id, self.project_root.display());
        let client = LspClient::start(config, &self.project_root)?;

        self.servers.insert(
            lang_id.to_string(),
            ManagedServer {
                client,
                started_at: Instant::now(),
                last_used: Instant::now(),
                files_opened: 0,
            },
        );

        Ok(())
    }

    /// Wait for a specific server to be ready.
    pub fn wait_ready(&mut self, lang_id: &str) -> Result<()> {
        let server = self.servers.get_mut(lang_id)
            .ok_or_else(|| anyhow::anyhow!("No server for {}", lang_id))?;
        server.client.wait_until_ready(Duration::from_secs(600))
    }

    /// Handle a single request.
    fn handle_request(&mut self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::Ping { lang_id } => {
                let ready = self.servers.get(&lang_id).is_some();
                let uptime = self.servers.get(&lang_id)
                    .map(|s| s.started_at.elapsed().as_secs())
                    .unwrap_or(0);
                DaemonResponse::Pong { ready, uptime_secs: uptime }
            }

            DaemonRequest::OpenFile { lang_id, rel_path, content } => {
                if let Err(e) = self.ensure_server(&lang_id) {
                    return DaemonResponse::Error { message: e.to_string() };
                }
                let server = self.servers.get_mut(&lang_id).unwrap();
                server.last_used = Instant::now();
                match server.client.open_file(&rel_path, &content, &lang_id) {
                    Ok(()) => {
                        server.files_opened += 1;
                        DaemonResponse::Ok
                    }
                    Err(e) => DaemonResponse::Error { message: e.to_string() },
                }
            }

            DaemonRequest::GetDefinition { lang_id, rel_path, line, character } => {
                let server = match self.servers.get_mut(&lang_id) {
                    Some(s) => s,
                    None => return DaemonResponse::Error {
                        message: format!("No server for {}", lang_id),
                    },
                };
                server.last_used = Instant::now();
                match server.client.get_definition(&rel_path, line, character) {
                    Ok(loc) => DaemonResponse::Definition {
                        location: loc.map(LocationDto::from),
                    },
                    Err(e) => DaemonResponse::Error { message: e.to_string() },
                }
            }

            DaemonRequest::GetReferences { lang_id, rel_path, line, character, include_declaration } => {
                let server = match self.servers.get_mut(&lang_id) {
                    Some(s) => s,
                    None => return DaemonResponse::Error {
                        message: format!("No server for {}", lang_id),
                    },
                };
                server.last_used = Instant::now();
                match server.client.get_references(&rel_path, line, character, include_declaration) {
                    Ok(locs) => DaemonResponse::References {
                        locations: locs.into_iter().map(LocationDto::from).collect(),
                    },
                    Err(e) => DaemonResponse::Error { message: e.to_string() },
                }
            }

            DaemonRequest::GetImplementations { lang_id, rel_path, line, character } => {
                let server = match self.servers.get_mut(&lang_id) {
                    Some(s) => s,
                    None => return DaemonResponse::Error {
                        message: format!("No server for {}", lang_id),
                    },
                };
                server.last_used = Instant::now();
                match server.client.get_implementations(&rel_path, line, character) {
                    Ok(locs) => DaemonResponse::Implementations {
                        locations: locs.into_iter().map(LocationDto::from).collect(),
                    },
                    Err(e) => DaemonResponse::Error { message: e.to_string() },
                }
            }

            DaemonRequest::CloseFile { lang_id, rel_path } => {
                if let Some(server) = self.servers.get_mut(&lang_id) {
                    server.last_used = Instant::now();
                    let _ = server.client.close_file(&rel_path);
                }
                DaemonResponse::Ok
            }

            DaemonRequest::ShutdownLang { lang_id } => {
                if let Some(server) = self.servers.remove(&lang_id) {
                    let _ = server.client.shutdown();
                }
                DaemonResponse::Ok
            }

            DaemonRequest::ShutdownAll => {
                let keys: Vec<String> = self.servers.keys().cloned().collect();
                for key in keys {
                    if let Some(server) = self.servers.remove(&key) {
                        let _ = server.client.shutdown();
                    }
                }
                DaemonResponse::Ok
            }

            DaemonRequest::Status => {
                let servers = self.servers.iter().map(|(lang_id, server)| {
                    ServerStatus {
                        lang_id: lang_id.clone(),
                        ready: true,
                        uptime_secs: server.started_at.elapsed().as_secs(),
                        files_opened: server.files_opened,
                    }
                }).collect();
                DaemonResponse::Status { servers }
            }

            DaemonRequest::RefineIncremental { added, modified, deleted, root_dir } => {
                let root = PathBuf::from(&root_dir);
                let mut processed: usize = 0;

                // For deleted files: close in all servers that might have them open
                for rel_path in &deleted {
                    for server in self.servers.values_mut() {
                        server.last_used = Instant::now();
                        let _ = server.client.close_file(rel_path);
                    }
                    processed += 1;
                }

                // For modified files: close then re-open with new content
                for rel_path in &modified {
                    let abs_path = root.join(rel_path);
                    let content = match std::fs::read_to_string(&abs_path) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("[LSP daemon] Failed to read modified file {}: {}", rel_path, e);
                            continue;
                        }
                    };
                    let ext = Path::new(rel_path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    let lang_id = extension_to_language_id(ext);

                    if let Some(server) = self.servers.get_mut(lang_id) {
                        server.last_used = Instant::now();
                        let _ = server.client.close_file(rel_path);
                        match server.client.open_file(rel_path, &content, lang_id) {
                            Ok(()) => { server.files_opened += 1; }
                            Err(e) => {
                                eprintln!("[LSP daemon] Failed to re-open modified file {}: {}", rel_path, e);
                                continue;
                            }
                        }
                    }
                    processed += 1;
                }

                // For added files: open with content
                for rel_path in &added {
                    let abs_path = root.join(rel_path);
                    let content = match std::fs::read_to_string(&abs_path) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("[LSP daemon] Failed to read added file {}: {}", rel_path, e);
                            continue;
                        }
                    };
                    let ext = Path::new(rel_path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    let lang_id = extension_to_language_id(ext);

                    if let Err(e) = self.ensure_server(lang_id) {
                        eprintln!("[LSP daemon] Failed to ensure server for {}: {}", lang_id, e);
                        continue;
                    }

                    if let Some(server) = self.servers.get_mut(lang_id) {
                        server.last_used = Instant::now();
                        match server.client.open_file(rel_path, &content, lang_id) {
                            Ok(()) => { server.files_opened += 1; }
                            Err(e) => {
                                eprintln!("[LSP daemon] Failed to open added file {}: {}", rel_path, e);
                                continue;
                            }
                        }
                    }
                    processed += 1;
                }

                DaemonResponse::Refined { files_processed: processed }
            }
        }
    }

    /// Run the daemon, listening on a Unix socket.
    pub fn run(&mut self) -> Result<()> {
        // Clean up stale socket
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        // Ensure .gid directory exists
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write PID file
        let pid_path = daemon_pid_path(&self.project_root);
        std::fs::write(&pid_path, std::process::id().to_string())?;

        let listener = UnixListener::bind(&self.socket_path)
            .with_context(|| format!("bind socket: {}", self.socket_path.display()))?;

        // Set non-blocking so we can check idle timeout
        listener.set_nonblocking(true)?;

        eprintln!("[LSP daemon] Listening on {}", self.socket_path.display());

        let mut last_activity = Instant::now();

        loop {
            // Check idle timeout
            if !self.servers.is_empty() {
                let all_idle = self.servers.values()
                    .all(|s| s.last_used.elapsed() > self.idle_timeout);
                if all_idle {
                    eprintln!("[LSP daemon] All servers idle for {}s, shutting down",
                        self.idle_timeout.as_secs());
                    break;
                }
            } else if last_activity.elapsed() > Duration::from_secs(300) {
                // No servers and no activity for 5 min
                eprintln!("[LSP daemon] No servers and idle for 5 min, shutting down");
                break;
            }

            match listener.accept() {
                Ok((stream, _)) => {
                    last_activity = Instant::now();
                    if let Err(e) = self.handle_connection(stream) {
                        eprintln!("[LSP daemon] Connection error: {}", e);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No connection pending, sleep briefly
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    eprintln!("[LSP daemon] Accept error: {}", e);
                    break;
                }
            }
        }

        // Cleanup
        self.handle_request(DaemonRequest::ShutdownAll);
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&pid_path);

        eprintln!("[LSP daemon] Stopped.");
        Ok(())
    }

    fn handle_connection(&mut self, stream: UnixStream) -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        stream.set_write_timeout(Some(Duration::from_secs(60)))?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let mut writer = stream;

        // Read requests until connection closes
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break, // Connection closed
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => break,
                Err(e) => return Err(e.into()),
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let req: DaemonRequest = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(e) => {
                    let resp = DaemonResponse::Error {
                        message: format!("Invalid request: {}", e),
                    };
                    let mut resp_line = serde_json::to_string(&resp)?;
                    resp_line.push('\n');
                    writer.write_all(resp_line.as_bytes())?;
                    writer.flush()?;
                    continue;
                }
            };

            let is_shutdown_all = matches!(req, DaemonRequest::ShutdownAll);
            let resp = self.handle_request(req);

            let mut resp_line = serde_json::to_string(&resp)?;
            resp_line.push('\n');
            writer.write_all(resp_line.as_bytes())?;
            writer.flush()?;

            if is_shutdown_all {
                return Ok(());
            }
        }

        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Daemon client — used by `gid extract` to talk to the daemon
// ═══════════════════════════════════════════════════════════════════════════════

/// Client that connects to a running LSP daemon via Unix socket.
/// Implements the same interface as LspClient for drop-in use.
pub struct DaemonLspClient {
    stream: BufReader<UnixStream>,
    writer: UnixStream,
    lang_id: String,
}

impl DaemonLspClient {
    /// Connect to a running daemon.
    pub fn connect(project_root: &Path, lang_id: &str) -> Result<Self> {
        let socket_path = daemon_socket_path(project_root);
        let stream = UnixStream::connect(&socket_path)
            .with_context(|| format!("connect to LSP daemon at {}", socket_path.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;

        Ok(Self {
            stream: BufReader::new(stream.try_clone()?),
            writer: stream,
            lang_id: lang_id.to_string(),
        })
    }

    fn send_recv(&mut self, req: DaemonRequest) -> Result<DaemonResponse> {
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;

        let mut resp_line = String::new();
        self.stream.read_line(&mut resp_line)?;

        let resp: DaemonResponse = serde_json::from_str(resp_line.trim())?;
        Ok(resp)
    }

    /// Check if the daemon has a ready server for our language.
    pub fn ping(&mut self) -> Result<bool> {
        match self.send_recv(DaemonRequest::Ping { lang_id: self.lang_id.clone() })? {
            DaemonResponse::Pong { ready, .. } => Ok(ready),
            DaemonResponse::Error { message } => bail!("Daemon error: {}", message),
            _ => bail!("Unexpected response to ping"),
        }
    }

    /// Open a file.
    pub fn open_file(&mut self, rel_path: &str, content: &str) -> Result<()> {
        match self.send_recv(DaemonRequest::OpenFile {
            lang_id: self.lang_id.clone(),
            rel_path: rel_path.to_string(),
            content: content.to_string(),
        })? {
            DaemonResponse::Ok => Ok(()),
            DaemonResponse::Error { message } => bail!("{}", message),
            _ => bail!("Unexpected response"),
        }
    }

    /// Get definition.
    pub fn get_definition(&mut self, rel_path: &str, line: u32, character: u32) -> Result<Option<LspLocation>> {
        match self.send_recv(DaemonRequest::GetDefinition {
            lang_id: self.lang_id.clone(),
            rel_path: rel_path.to_string(),
            line,
            character,
        })? {
            DaemonResponse::Definition { location } => Ok(location.map(LspLocation::from)),
            DaemonResponse::Error { message } => bail!("{}", message),
            _ => bail!("Unexpected response"),
        }
    }

    /// Get references.
    pub fn get_references(
        &mut self,
        rel_path: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<Vec<LspLocation>> {
        match self.send_recv(DaemonRequest::GetReferences {
            lang_id: self.lang_id.clone(),
            rel_path: rel_path.to_string(),
            line,
            character,
            include_declaration,
        })? {
            DaemonResponse::References { locations } => {
                Ok(locations.into_iter().map(LspLocation::from).collect())
            }
            DaemonResponse::Error { message } => bail!("{}", message),
            _ => bail!("Unexpected response"),
        }
    }

    /// Get implementations.
    pub fn get_implementations(&mut self, rel_path: &str, line: u32, character: u32) -> Result<Vec<LspLocation>> {
        match self.send_recv(DaemonRequest::GetImplementations {
            lang_id: self.lang_id.clone(),
            rel_path: rel_path.to_string(),
            line,
            character,
        })? {
            DaemonResponse::Implementations { locations } => {
                Ok(locations.into_iter().map(LspLocation::from).collect())
            }
            DaemonResponse::Error { message } => bail!("{}", message),
            _ => bail!("Unexpected response"),
        }
    }

    /// Close a file.
    pub fn close_file(&mut self, rel_path: &str) -> Result<()> {
        let _ = self.send_recv(DaemonRequest::CloseFile {
            lang_id: self.lang_id.clone(),
            rel_path: rel_path.to_string(),
        });
        Ok(())
    }

    /// Incrementally refine: notify the daemon of file changes from a FileDelta.
    /// The daemon will close deleted files, re-open modified files, and open added files
    /// in the appropriate LSP servers.
    /// Returns the number of files processed.
    pub fn refine_incremental(&mut self, delta: &FileDelta, root_dir: &Path) -> Result<usize> {
        match self.send_recv(DaemonRequest::RefineIncremental {
            added: delta.added.clone(),
            modified: delta.modified.clone(),
            deleted: delta.deleted.clone(),
            root_dir: root_dir.to_string_lossy().to_string(),
        })? {
            DaemonResponse::Refined { files_processed } => Ok(files_processed),
            DaemonResponse::Error { message } => bail!("Refine incremental error: {}", message),
            _ => bail!("Unexpected response to refine_incremental"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers for gid extract integration
// ═══════════════════════════════════════════════════════════════════════════════

/// Check if a daemon is already running for this project.
pub fn is_daemon_running(project_root: &Path) -> bool {
    let socket_path = daemon_socket_path(project_root);

    if !socket_path.exists() {
        return false;
    }

    // Try connecting — if it works, daemon is alive
    if UnixStream::connect(&socket_path).is_ok() {
        return true;
    }

    // Stale socket, clean up
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&daemon_pid_path(project_root));
    false
}

/// Start daemon as a background process (fork).
/// Returns Ok(true) if we started a new daemon, Ok(false) if one was already running.
pub fn ensure_daemon(project_root: &Path) -> Result<bool> {
    if is_daemon_running(project_root) {
        return Ok(false);
    }

    eprintln!("[LSP] Starting daemon for {}...", project_root.display());

    // Fork a background process
    // We use std::process::Command to spawn ourselves with a special flag
    // But since we can't easily re-exec, we'll use the simpler approach:
    // spawn a thread that runs the daemon in the background
    let root = project_root.to_path_buf();
    std::thread::spawn(move || {
        let mut daemon = LspDaemon::new(&root);
        if let Err(e) = daemon.run() {
            eprintln!("[LSP daemon] Error: {}", e);
        }
    });

    // Wait for socket to appear
    let socket_path = daemon_socket_path(project_root);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if socket_path.exists() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    bail!("Daemon did not start within 5 seconds")
}

/// Start the daemon, wait for LSP server to be ready, then return a client.
/// This is the main entry point for `gid extract --lsp`.
pub fn get_or_start_daemon_client(
    project_root: &Path,
    lang_id: &str,
) -> Result<DaemonLspClient> {
    ensure_daemon(project_root)?;
    DaemonLspClient::connect(project_root, lang_id)
}

/// Stop the daemon for a project.
pub fn stop_daemon(project_root: &Path) -> Result<()> {
    if !is_daemon_running(project_root) {
        return Ok(());
    }

    let socket_path = daemon_socket_path(project_root);
    if let Ok(stream) = UnixStream::connect(&socket_path) {
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut writer = stream;
        let req = serde_json::to_string(&DaemonRequest::ShutdownAll)?;
        let _ = writer.write_all(format!("{}\n", req).as_bytes());
        let _ = writer.flush();
    }

    // Wait for cleanup
    std::thread::sleep(Duration::from_millis(500));

    // Force cleanup if needed
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&daemon_pid_path(project_root));

    Ok(())
}
