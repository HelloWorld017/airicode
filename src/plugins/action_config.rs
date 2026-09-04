use std::{io::Write as _, path::Path, process::Stdio, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;
use tempfile::Builder;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::core::{
    config::{Config, ConfigPaths, load_config, load_config_file, write_json_atomic},
    error::{Error, Result},
    hooks::{ConfigReadContext, ConfigReadHook},
    models::{
        Plugin, PluginId, ShellAction, ShellActionContext, ShellActionDefinition, ShellActionId,
        ShellActionInput, ShellActionOutput,
    },
    registry::PluginRegistryScope,
};

pub struct ActionConfig {
    id: ShellActionId,
    config: Config,
}

impl ActionConfig {
    pub fn new(config: Config) -> Self {
        Self {
            id: ShellActionId::new(),
            config,
        }
    }
}

#[async_trait]
impl ShellAction for ActionConfig {
    fn id(&self) -> ShellActionId {
        self.id
    }

    fn definition(&self) -> ShellActionDefinition {
        ShellActionDefinition::new(
            "config",
            "Edit Airicode configuration with its generated JSON schema.",
            serde_json::json!({ "arguments": { "type": "string", "enum": ["--global"], "maxItems": 1 } }),
        )
    }

    async fn execute(
        &self,
        input: ShellActionInput,
        context: ShellActionContext,
    ) -> Result<ShellActionOutput> {
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let paths = ConfigPaths::for_project(&context.project_root);
        let target = match input.arguments.as_slice() {
            [] => paths.project.clone(),
            [flag] if flag == "--global" => paths.global.clone().ok_or_else(|| {
                Error::Config("XDG_CONFIG_HOME or HOME is required for global configuration".into())
            })?,
            _ => return Err(Error::Command("usage: config [--global]".into())),
        };

        let loaded = load_config_file(&target).await;
        let mut editable = loaded.raw;
        let server = SchemaServer::start(self.config.schema().clone()).await?;
        let schema_url = server.url.clone();
        editable
            .as_object_mut()
            .expect("loaded configuration is an object")
            .insert("$schema".into(), Value::String(schema_url));

        let mut temporary = Builder::new()
            .prefix("airicode-config-")
            .suffix(".json")
            .tempfile()?;
        serde_json::to_writer_pretty(temporary.as_file_mut(), &editable)?;
        temporary.write_all(b"\n")?;
        temporary.flush()?;
        let temporary_path = temporary.path().to_path_buf();

        let editor_result = open_editor(&temporary_path, context.cancellation.clone()).await;
        server.shutdown().await;
        editor_result?;

        let bytes = tokio::fs::read(&temporary_path).await?;
        let mut updated = serde_json::from_slice::<Value>(&bytes).map_err(|error| {
            Error::Config(format!(
                "editor output is not valid JSON; configuration was not saved: {error}"
            ))
        })?;
        let object = updated.as_object_mut().ok_or_else(|| {
            Error::Config("editor output must be a JSON object; configuration was not saved".into())
        })?;
        object.remove("$schema");
        write_json_atomic(&target, &updated).await?;

        let merged = load_config(&paths).await;
        if let Err(error) = self.config.validate(&merged.raw) {
            return Err(Error::Config(format!(
                "saved {}, but configuration validation failed: {error}",
                target.display()
            )));
        }

        let mut output = format!("saved {}", target.display());
        let diagnostics = loaded
            .diagnostics
            .into_iter()
            .chain(merged.diagnostics)
            .collect::<Vec<_>>();
        if !diagnostics.is_empty() {
            output.push_str("\nWarnings:\n");
            output.push_str(&diagnostics.join("\n"));
        }
        Ok(output)
    }
}

pub struct ActionConfigPlugin {
    id: PluginId,
}

impl ActionConfigPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
        }
    }
}

impl Default for ActionConfigPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ActionConfigPlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "action_config"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        let hook: Arc<dyn ConfigReadHook> = self;
        registry.register_hook(hook)
    }
}

#[async_trait]
impl ConfigReadHook for ActionConfigPlugin {
    async fn config_read(&self, context: ConfigReadContext) -> Result<()> {
        context
            .registry
            .register_shell_action(Arc::new(ActionConfig::new(context.config)), 0)
            .map(|_| ())
    }
}

struct SchemaServer {
    url: String,
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl SchemaServer {
    async fn start(schema: Value) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/schema.json", listener.local_addr()?);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let schema = Arc::new(serde_json::to_vec(&schema)?);
        let task = tokio::spawn(async move {
            serve_schema(listener, schema, task_cancellation).await;
        });
        Ok(Self {
            url,
            cancellation,
            task: Some(task),
        })
    }

    async fn shutdown(mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for SchemaServer {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn serve_schema(
    listener: TcpListener,
    schema: Arc<Vec<u8>>,
    cancellation: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    break;
                };
                let schema = schema.clone();
                tokio::spawn(async move {
                    let _ = respond_schema(stream, schema).await;
                });
            }
        }
    }
}

async fn respond_schema(mut stream: TcpStream, schema: Arc<Vec<u8>>) -> std::io::Result<()> {
    let mut request = Vec::with_capacity(1024);
    let mut buffer = [0; 1024];
    while request.len() < 8 * 1024 {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(2).any(|window| window == b"\r\n") {
            break;
        }
    }
    let request_line = std::str::from_utf8(&request)
        .ok()
        .and_then(|request| request.lines().next())
        .unwrap_or_default();
    let (status, content_type, body) = if request_line.starts_with("GET /schema.json ") {
        ("200 OK", "application/schema+json", schema.as_slice())
    } else {
        (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found".as_slice(),
        )
    };
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

async fn open_editor(path: &Path, cancellation: CancellationToken) -> Result<()> {
    let editor =
        std::env::var_os("EDITOR").ok_or_else(|| Error::Config("EDITOR is not set".into()))?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exec $EDITOR \"$1\"")
        .arg("airicode-config-editor")
        .arg(path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .env("EDITOR", editor)
        .spawn()?;
    let status = tokio::select! {
        status = child.wait() => status?,
        _ = cancellation.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(Error::Cancelled);
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(Error::Config(format!("EDITOR exited with {status}")))
    }
}
