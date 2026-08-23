use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        mpsc, Arc, Mutex, Weak,
    },
    thread,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::{
    Error, Plugin, PluginId, PluginRegistrar, Result, Tool, ToolContext, ToolDefinition, ToolId,
    ToolOutput,
};

const PLUGIN_ID_PREFIX: &str = "external.mcp";

const STATE_NEW: u8 = 0;
const STATE_INITIALIZING: u8 = 1;
const STATE_READY: u8 = 2;
const STATE_CLOSED: u8 = 3;

type RpcReply = std::result::Result<Value, String>;

#[derive(Clone, Debug)]
pub struct McpConfig {
    pub server_name: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub request_timeout: Duration,
    pub max_message_bytes: usize,
    pub max_diagnostics: usize,
}

impl McpConfig {
    pub fn new(server_name: impl Into<String>, program: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            request_timeout: Duration::from_secs(30),
            max_message_bytes: 4 * 1024 * 1024,
            max_diagnostics: 200,
        }
    }
}

struct McpInner {
    server_name: String,
    writer: Arc<Mutex<Option<ChildStdin>>>,
    child: Mutex<Option<Child>>,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<RpcReply>>>>,
    next_id: AtomicU64,
    state: AtomicU8,
    timeout: Duration,
    max_message_bytes: usize,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
}

#[derive(Clone)]
struct McpClient {
    inner: Arc<McpInner>,
}

struct McpPlugin {
    config: McpConfig,
    client: Mutex<Option<McpClient>>,
}

pub fn mcp_plugin(config: McpConfig) -> Arc<dyn Plugin> {
    Arc::new(McpPlugin {
        config,
        client: Mutex::new(None),
    })
}

#[async_trait]
impl Plugin for McpPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(format!(
            "{PLUGIN_ID_PREFIX}.{}",
            identifier(&self.config.server_name)
        ))
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        if self
            .client
            .lock()
            .map_err(|_| Error::Plugin("MCP plugin state lock poisoned".into()))?
            .is_some()
        {
            return Err(Error::Plugin("MCP plugin is already initialized".into()));
        }

        // Keep the client local until discovery and every staged registration succeeds. If any
        // step fails, dropping it terminates the child and core commits no staged registrations.
        let client = McpClient::spawn(self.config.clone())?;
        client.initialize("airicode").await?;
        let definitions = client.list_tools().await?;
        let adapters = client.tool_adapters(definitions)?;
        for adapter in adapters {
            registrar.register_tool(0, adapter)?;
        }
        *self
            .client
            .lock()
            .map_err(|_| Error::Plugin("MCP plugin state lock poisoned".into()))? = Some(client);
        Ok(())
    }
}

impl McpClient {
    fn spawn(config: McpConfig) -> Result<Self> {
        if config.server_name.trim().is_empty()
            || config.program.trim().is_empty()
            || config.request_timeout.is_zero()
            || config.max_message_bytes == 0
            || config.max_diagnostics == 0
        {
            return Err(Error::Plugin("invalid MCP process configuration".into()));
        }
        let mut command = Command::new(&config.program);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            Error::Plugin(format!(
                "could not spawn MCP server {}: {error}",
                config.server_name
            ))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Plugin("MCP stdin was not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Plugin("MCP stdout was not piped".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Plugin("MCP stderr was not piped".into()))?;
        let writer = Arc::new(Mutex::new(Some(stdin)));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics = Arc::new(Mutex::new(VecDeque::new()));

        {
            let pending = pending.clone();
            let writer = writer.clone();
            let diagnostics = diagnostics.clone();
            let max = config.max_message_bytes;
            let max_diagnostics = config.max_diagnostics;
            thread::Builder::new()
                .name(format!("mcp-{}-stdout", config.server_name))
                .spawn(move || {
                    read_responses(stdout, writer, pending, diagnostics, max_diagnostics, max)
                })
                .map_err(|error| Error::Plugin(format!("could not start MCP reader: {error}")))?;
        }
        {
            let diagnostics = diagnostics.clone();
            let max_diagnostics = config.max_diagnostics;
            let max_diagnostic_bytes = config.max_message_bytes;
            thread::Builder::new()
                .name(format!("mcp-{}-stderr", config.server_name))
                .spawn(move || {
                    read_diagnostics(stderr, diagnostics, max_diagnostics, max_diagnostic_bytes)
                })
                .map_err(|error| {
                    Error::Plugin(format!("could not start MCP stderr reader: {error}"))
                })?;
        }

        Ok(Self {
            inner: Arc::new(McpInner {
                server_name: config.server_name,
                writer,
                child: Mutex::new(Some(child)),
                pending,
                next_id: AtomicU64::new(1),
                state: AtomicU8::new(STATE_NEW),
                timeout: config.request_timeout,
                max_message_bytes: config.max_message_bytes,
                diagnostics,
            }),
        })
    }

    async fn initialize(&self, client_name: impl Into<String>) -> Result<Value> {
        if self
            .inner
            .state
            .compare_exchange(
                STATE_NEW,
                STATE_INITIALIZING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Err(Error::Plugin(
                "MCP client is already initialized or closed".into(),
            ));
        }
        let client_name = client_name.into();
        let client = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            client.request_blocking(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": client_name, "version": env!("CARGO_PKG_VERSION") }
                }),
            )
        })
        .await
        .map_err(|error| Error::Plugin(format!("MCP initialize task failed: {error}")))?;
        match result {
            Ok(value) => {
                self.inner.state.store(STATE_READY, Ordering::Release);
                self.notify("notifications/initialized", json!({}))?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.inner.state.compare_exchange(
                    STATE_INITIALIZING,
                    STATE_NEW,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                Err(error)
            }
        }
    }

    async fn list_tools(&self) -> Result<Vec<McpToolDefinition>> {
        self.require_ready()?;
        let client = self.clone();
        let value =
            tokio::task::spawn_blocking(move || client.request_blocking("tools/list", json!({})))
                .await
                .map_err(|error| Error::Plugin(format!("MCP tools/list task failed: {error}")))??;
        let response: McpToolsResponse = serde_json::from_value(value)
            .map_err(|error| Error::Plugin(format!("invalid MCP tools/list result: {error}")))?;
        Ok(response.tools)
    }

    fn tool_adapters(&self, definitions: Vec<McpToolDefinition>) -> Result<Vec<Arc<dyn Tool>>> {
        self.require_ready()?;
        let mut exposed = std::collections::BTreeSet::new();
        definitions
            .into_iter()
            .map(|definition| {
                if definition.name.trim().is_empty() {
                    return Err(Error::Plugin("MCP tool has an empty name".into()));
                }
                let name = namespaced_name(&self.inner.server_name, &definition.name);
                if !exposed.insert(name.clone()) {
                    return Err(Error::Plugin(format!(
                        "duplicate namespaced MCP tool: {name}"
                    )));
                }
                Ok(Arc::new(McpToolAdapter {
                    client: Arc::downgrade(&self.inner),
                    exposed_name: name,
                    remote: definition,
                }) as Arc<dyn Tool>)
            })
            .collect()
    }

    #[allow(dead_code)]
    fn diagnostics(&self) -> Vec<String> {
        self.inner
            .diagnostics
            .lock()
            .expect("MCP diagnostics lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    async fn close(&self, grace: Duration) -> Result<()> {
        if self.inner.state.swap(STATE_CLOSED, Ordering::AcqRel) == STATE_CLOSED {
            return Ok(());
        }
        self.inner
            .writer
            .lock()
            .expect("MCP writer lock poisoned")
            .take();
        fail_pending(&self.inner.pending, "MCP client closed");
        let child = self
            .inner
            .child
            .lock()
            .expect("MCP child lock poisoned")
            .take();
        if let Some(mut child) = child {
            tokio::task::spawn_blocking(move || -> Result<()> {
                let deadline = Instant::now() + grace;
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => return Ok(()),
                        Ok(None) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Ok(None) => {
                            child.kill().map_err(|error| {
                                Error::Plugin(format!("could not kill MCP server: {error}"))
                            })?;
                            child.wait().map_err(|error| {
                                Error::Plugin(format!("could not reap MCP server: {error}"))
                            })?;
                            return Ok(());
                        }
                        Err(error) => {
                            return Err(Error::Plugin(format!(
                                "could not inspect MCP server: {error}"
                            )))
                        }
                    }
                }
            })
            .await
            .map_err(|error| Error::Plugin(format!("MCP close task failed: {error}")))??;
        }
        Ok(())
    }

    fn require_ready(&self) -> Result<()> {
        if self.inner.state.load(Ordering::Acquire) != STATE_READY {
            return Err(Error::Plugin("MCP client is not initialized".into()));
        }
        Ok(())
    }

    fn request_blocking(&self, method: &str, params: Value) -> Result<Value> {
        if self.inner.state.load(Ordering::Acquire) == STATE_CLOSED {
            return Err(Error::Plugin("MCP client is closed".into()));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.inner
            .pending
            .lock()
            .expect("MCP pending lock poisoned")
            .insert(id, sender);
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(error) =
            write_message(&self.inner.writer, &message, self.inner.max_message_bytes)
        {
            self.inner
                .pending
                .lock()
                .expect("MCP pending lock poisoned")
                .remove(&id);
            return Err(error);
        }
        match receiver.recv_timeout(self.inner.timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(Error::Plugin(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.inner
                    .pending
                    .lock()
                    .expect("MCP pending lock poisoned")
                    .remove(&id);
                Err(Error::Plugin(format!("MCP request {method} timed out")))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(Error::Plugin("MCP response channel closed".into()))
            }
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        write_message(
            &self.inner.writer,
            &json!({
                "jsonrpc": "2.0", "method": method, "params": params
            }),
            self.inner.max_message_bytes,
        )
    }
}

impl Drop for McpInner {
    fn drop(&mut self) {
        self.state.store(STATE_CLOSED, Ordering::Release);
        if let Ok(mut writer) = self.writer.try_lock() {
            writer.take();
        }
        let child = match self.child.get_mut() {
            Ok(child) => child.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(mut child) = child else {
            return;
        };

        let _ = child.kill();
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                Ok(None) | Err(_) => break,
            }
        }
        // Reaping can theoretically block on a misbehaving platform, so finish it off-thread.
        let _ = thread::Builder::new()
            .name("mcp-child-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct McpToolDefinition {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Deserialize)]
struct McpToolsResponse {
    tools: Vec<McpToolDefinition>,
}

struct McpToolAdapter {
    client: Weak<McpInner>,
    exposed_name: String,
    remote: McpToolDefinition,
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn id(&self) -> ToolId {
        ToolId::new(format!("external.mcp.{}", self.exposed_name))
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.exposed_name.clone(),
            description: self.remote.description.clone(),
            input_schema: self.remote.input_schema.clone(),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let client = McpClient {
            inner: self
                .client
                .upgrade()
                .ok_or_else(|| Error::Tool("MCP plugin is no longer running".into()))?,
        };
        client.require_ready()?;
        let remote_name = self.remote.name.clone();
        let task = tokio::task::spawn_blocking(move || {
            client.request_blocking(
                "tools/call",
                json!({ "name": remote_name, "arguments": input }),
            )
        });
        let value = tokio::select! {
            _ = context.cancellation.cancelled() => return Err(Error::Cancelled),
            value = task => value.map_err(|error| Error::Tool(format!("MCP tool task failed: {error}")))??,
        };
        parse_tool_result(value)
    }
}

fn parse_tool_result(value: Value) -> Result<ToolOutput> {
    let is_error = value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Tool("MCP tools/call result has no content array".into()))?;
    let mut pieces = Vec::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("text") {
            pieces.push(
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            );
        } else {
            pieces.push(
                serde_json::to_string(item).map_err(|error| {
                    Error::Tool(format!("could not encode MCP content: {error}"))
                })?,
            );
        }
    }
    Ok(ToolOutput {
        content: pieces.join("\n"),
        is_error,
    })
}

fn namespaced_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", identifier(server), identifier(tool))
}

fn identifier(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            output.push(character);
        } else {
            output.push('_');
        }
    }
    output
}

fn write_message(writer: &Arc<Mutex<Option<ChildStdin>>>, value: &Value, max: usize) -> Result<()> {
    let body = serde_json::to_vec(value)
        .map_err(|error| Error::Plugin(format!("could not encode MCP message: {error}")))?;
    if body.len() > max {
        return Err(Error::Plugin(format!("MCP message exceeds {max} bytes")));
    }
    let mut guard = writer.lock().expect("MCP writer lock poisoned");
    let stream = guard
        .as_mut()
        .ok_or_else(|| Error::Plugin("MCP stdin is closed".into()))?;
    write!(stream, "Content-Length: {}\r\n\r\n", body.len())
        .and_then(|_| stream.write_all(&body))
        .and_then(|_| stream.flush())
        .map_err(|error| Error::Plugin(format!("could not write MCP message: {error}")))
}

fn read_responses(
    stdout: impl Read,
    writer: Arc<Mutex<Option<ChildStdin>>>,
    pending: Arc<Mutex<HashMap<u64, mpsc::Sender<RpcReply>>>>,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    max_diagnostics: usize,
    max: usize,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let message = match read_frame(&mut reader, max) {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(error) => {
                push_diagnostic(
                    &diagnostics,
                    max_diagnostics,
                    format!("stdout framing error: {error}"),
                );
                break;
            }
        };
        let value: Value = match serde_json::from_slice(&message) {
            Ok(value) => value,
            Err(error) => {
                push_diagnostic(
                    &diagnostics,
                    max_diagnostics,
                    format!("malformed JSON-RPC message: {error}"),
                );
                continue;
            }
        };
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            push_diagnostic(
                &diagnostics,
                max_diagnostics,
                "message does not declare JSON-RPC 2.0".into(),
            );
            continue;
        }
        if value.get("method").is_some() {
            if let Some(id) = value.get("id").cloned() {
                let response = json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": "client method not found" }
                });
                let _ = write_message(&writer, &response, max);
            }
            continue;
        }
        let id = match value.get("id").and_then(Value::as_u64) {
            Some(id) => id,
            None => {
                push_diagnostic(
                    &diagnostics,
                    max_diagnostics,
                    "response has no numeric id".into(),
                );
                continue;
            }
        };
        let sender = pending
            .lock()
            .expect("MCP pending lock poisoned")
            .remove(&id);
        if let Some(sender) = sender {
            let reply = if let Some(error) = value.get("error") {
                Err(format!("MCP JSON-RPC error: {error}"))
            } else if let Some(result) = value.get("result") {
                Ok(result.clone())
            } else {
                Err("MCP response has neither result nor error".into())
            };
            let _ = sender.send(reply);
        } else {
            push_diagnostic(
                &diagnostics,
                max_diagnostics,
                format!("response for unknown id {id}"),
            );
        }
    }
    fail_pending(&pending, "MCP stdout closed");
}

fn read_frame(reader: &mut impl BufRead, max: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut first = String::new();
    loop {
        first.clear();
        if reader.read_line(&mut first)? == 0 {
            return Ok(None);
        }
        if first.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MCP header exceeds limit",
            ));
        }
        if !first.trim().is_empty() {
            break;
        }
    }
    if first.trim_start().starts_with('{') {
        let bytes = first.trim_end_matches(['\r', '\n']).as_bytes().to_vec();
        if bytes.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MCP frame exceeds limit",
            ));
        }
        return Ok(Some(bytes));
    }

    let mut content_length = None;
    let mut header_bytes = 0usize;
    let mut line = first;
    loop {
        header_bytes = header_bytes.saturating_add(line.len());
        if header_bytes > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MCP headers exceed limit",
            ));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                if content_length.is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "duplicate Content-Length",
                    ));
                }
                content_length = Some(value.trim().parse::<usize>().map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid Content-Length")
                })?);
            }
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "malformed MCP header",
            ));
        }
        line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "EOF in MCP headers",
            ));
        }
        if line.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MCP header exceeds limit",
            ));
        }
    }
    let length = content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
    })?;
    if length > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "MCP frame exceeds limit",
        ));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

fn read_diagnostics(
    stderr: impl Read,
    diagnostics: Arc<Mutex<VecDeque<String>>>,
    max_lines: usize,
    max_line_bytes: usize,
) {
    let mut reader = BufReader::new(stderr);
    loop {
        match read_bounded_line(&mut reader, max_line_bytes) {
            Ok(Some((mut line, truncated))) => {
                if truncated {
                    line.push_str(" [truncated]");
                }
                push_diagnostic(&diagnostics, max_lines, line);
            }
            Ok(None) => break,
            Err(error) => {
                push_diagnostic(
                    &diagnostics,
                    max_lines,
                    format!("stderr read error: {error}"),
                );
                break;
            }
        }
    }
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    max: usize,
) -> std::io::Result<Option<(String, bool)>> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map(|index| index + 1).unwrap_or(available.len());
        let mut content_end = newline.unwrap_or(consumed);
        if content_end > 0 && available[content_end - 1] == b'\r' {
            content_end -= 1;
        }
        let content = &available[..content_end];
        let remaining = max.saturating_sub(bytes.len());
        bytes.extend_from_slice(&content[..content.len().min(remaining)]);
        truncated |= content.len() > remaining;
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    while matches!(bytes.last(), Some(b'\r' | b'\n')) {
        bytes.pop();
    }
    Ok(Some((
        String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    )))
}

fn push_diagnostic(diagnostics: &Arc<Mutex<VecDeque<String>>>, max: usize, value: String) {
    let mut diagnostics = diagnostics.lock().expect("MCP diagnostics lock poisoned");
    if diagnostics.len() == max {
        diagnostics.pop_front();
    }
    diagnostics.push_back(value);
}

fn fail_pending(pending: &Arc<Mutex<HashMap<u64, mpsc::Sender<RpcReply>>>>, reason: &str) {
    let senders: Vec<_> = pending
        .lock()
        .expect("MCP pending lock poisoned")
        .drain()
        .map(|(_, sender)| sender)
        .collect();
    for sender in senders {
        let _ = sender.send(Err(reason.into()));
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn parses_content_length_and_line_frames() {
        let mut framed = Cursor::new(b"Content-Length: 13\r\n\r\n{\"jsonrpc\":1}".to_vec());
        assert_eq!(
            read_frame(&mut framed, 100).unwrap().unwrap(),
            b"{\"jsonrpc\":1}"
        );
        let mut line = Cursor::new(b"{\"jsonrpc\":\"2.0\"}\n".to_vec());
        assert_eq!(
            read_frame(&mut line, 100).unwrap().unwrap(),
            b"{\"jsonrpc\":\"2.0\"}"
        );
    }

    #[test]
    fn rejects_oversized_and_malformed_frames() {
        let mut oversized = Cursor::new(b"Content-Length: 101\r\n\r\n".to_vec());
        assert!(read_frame(&mut oversized, 100).is_err());
        let mut malformed = Cursor::new(b"No colon\r\n\r\n".to_vec());
        assert!(read_frame(&mut malformed, 100).is_err());
        let mut oversized_headers = Cursor::new(b"X: 123456\r\nY: 123456\r\n\r\n".to_vec());
        assert!(read_frame(&mut oversized_headers, 16).is_err());
    }

    #[test]
    fn namespaces_tool_names() {
        assert_eq!(
            namespaced_name("docs server", "web/search"),
            "mcp__docs_server__web_search"
        );
    }

    #[test]
    fn bounds_stderr_lines() {
        let mut input = Cursor::new(b"abcdefghij\nnext\n".to_vec());
        assert_eq!(
            read_bounded_line(&mut input, 4).unwrap(),
            Some(("abcd".into(), true))
        );
        assert_eq!(
            read_bounded_line(&mut input, 4).unwrap(),
            Some(("next".into(), false))
        );
    }

    #[test]
    fn parses_text_and_structured_tool_content() {
        let output = parse_tool_result(json!({
            "content": [{"type":"text","text":"hello"},{"type":"image","data":"x"}],
            "isError": true
        }))
        .unwrap();
        assert!(output.content.starts_with("hello\n"));
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn unavailable_server_stages_no_tools() {
        let plugin = mcp_plugin(McpConfig::new(
            "missing",
            "airicode-test-mcp-program-that-does-not-exist",
        ));
        let registrar = PluginRegistrar::new(plugin.id());
        let error = plugin.init(registrar.clone()).await.unwrap_err();
        assert!(error.to_string().contains("could not spawn MCP server"));
        assert!(registrar.take().tools.is_empty());
    }
}
