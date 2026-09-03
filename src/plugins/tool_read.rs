use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde_json::Value;

use crate::{
    core::{
        error::{Error, Result},
        hooks::{ConfigReadContext, ConfigReadHook},
        models::{
            Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
            ToolInputDefinition, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::{PathCorrectionKind, hashline, note::add_output_note, path_correction},
};

#[allow(dead_code)]
#[derive(JsonSchema)]
struct ReadInputSchema {
    path: String,
    #[schemars(range(min = 1))]
    start_line: Option<usize>,
    #[schemars(range(min = 1))]
    end_line: Option<usize>,
}

pub struct ToolRead {
    id: ToolId,
    max_lines: usize,
    max_bytes: usize,
    enable_hashline: bool,
}

impl ToolRead {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_lines: 2_000,
            max_bytes: 256 * 1024,
            enable_hashline: false,
        }
    }
    pub fn with_limits(mut self, max_lines: usize, max_bytes: usize) -> Self {
        self.max_lines = max_lines;
        self.max_bytes = max_bytes;
        self
    }
    pub fn with_hashline(mut self, enable_hashline: bool) -> Self {
        self.enable_hashline = enable_hashline;
        self
    }
}

impl Default for ToolRead {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolRead {
    fn id(&self) -> ToolId {
        self.id
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read".into(),
            description: if self.enable_hashline {
                "Read a UTF-8 text file from the current workdir and return hashline-annotated lines in the form `<line>:<3-character-hash>|<content>`. Copy only the `<line>:<hash>` prefix into a patch_hashline anchor. `start_line` and `end_line` are optional inclusive line limits. Binary/NUL-containing files and requests beyond the configured size or line limits fail."
            } else {
                "Read a UTF-8 text file from the current workdir and return numbered lines in the form `<line>|<content>`. `start_line` and `end_line` are optional inclusive line limits. Binary/NUL-containing files and requests beyond the configured size or line limits fail."
            }.into(),
            input: ToolInputDefinition::new(crate::utils::schema::json_schema::<ReadInputSchema>()),
        }
    }
    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let object = input
            .as_object()
            .ok_or_else(|| Error::Tool("read input must be an object".into()))?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("read requires path".into()))?;
        let bytes = match context.workdir.read(Path::new(path)).await {
            Ok(bytes) => bytes,
            Err(Error::Workdir(message)) => {
                let content = match path_correction(
                    Path::new(path),
                    context.workdir.as_ref(),
                    PathCorrectionKind::File,
                )
                .await
                {
                    Ok(Some(correction)) => {
                        format!("{message}\nDid you mean? {}", correction.path.display())
                    }
                    _ => message,
                };
                let output = ToolOutput::Failure { content };
                add_output_note(&context, "read", "Read failed", &output).await?;
                return Ok(output);
            }
            Err(error) => return Err(error),
        };
        if bytes.contains(&0) {
            let output = ToolOutput::Failure {
                content: "cannot read binary/NUL-containing input".into(),
            };
            add_output_note(&context, "read", "Read failed", &output).await?;
            return Ok(output);
        }
        if bytes.len() > self.max_bytes {
            let output = ToolOutput::Failure {
                content: format!("file exceeds read limit of {} bytes", self.max_bytes),
            };
            add_output_note(&context, "read", "Read failed", &output).await?;
            return Ok(output);
        }
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Tool("cannot read non-UTF-8 input".into()))?;
        let all = hashline::render(text);
        let start = object
            .get("start_line")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let end = object
            .get("end_line")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(all.len());
        if end < start {
            let output = ToolOutput::Failure {
                content: "invalid line range".into(),
            };
            add_output_note(&context, "read", "Read failed", &output).await?;
            return Ok(output);
        }
        if end - start + 1 > self.max_lines {
            let output = ToolOutput::Failure {
                content: format!("line range exceeds read limit of {} lines", self.max_lines),
            };
            add_output_note(&context, "read", "Read failed", &output).await?;
            return Ok(output);
        }
        let selected = all
            .into_iter()
            .filter(|line| line.line >= start && line.line <= end)
            .map(|line| {
                if self.enable_hashline {
                    format!("{}:{}|{}", line.line, line.tag, line.text)
                } else {
                    format!("{}|{}", line.line, line.text)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let range = if let Some(end) = object.get("end_line").and_then(Value::as_u64) {
            format!(":{start}-{end}")
        } else if start > 1 {
            format!(":{start}-")
        } else {
            String::new()
        };
        let output = ToolOutput::Success { content: selected };
        add_output_note(&context, "read", format!("Read {path}{range}"), &output).await?;
        Ok(output)
    }
}

pub struct ToolReadPlugin {
    id: PluginId,
}
impl ToolReadPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
        }
    }
}

#[async_trait]
impl Plugin for ToolReadPlugin {
    fn id(&self) -> PluginId {
        self.id
    }
    fn name(&self) -> &str {
        "tool_read"
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        let hook: Arc<dyn ConfigReadHook> = self;
        registry.register_hook(hook)
    }
}

#[async_trait]
impl ConfigReadHook for ToolReadPlugin {
    async fn config_read(&self, context: ConfigReadContext) -> Result<()> {
        context
            .registry
            .register_tool(
                Arc::new(ToolRead::new().with_hashline(context.config.tool.enable_hashline)),
                0,
            )
            .map(|_| ())
    }
}
