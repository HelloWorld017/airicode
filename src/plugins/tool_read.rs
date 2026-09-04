use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

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
    utils::{PathCorrectionKind, note::add_output_note, path_correction, schema::json_schema},
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    #[schemars(range(min = 1))]
    line_start: Option<usize>,
    #[schemars(range(min = 1))]
    line_end: Option<usize>,
}

pub struct ToolRead {
    id: ToolId,
    max_lines: usize,
    max_bytes: usize,
}

impl ToolRead {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            max_lines: 2_000,
            max_bytes: 256 * 1024,
        }
    }
    pub fn with_limits(mut self, max_lines: usize, max_bytes: usize) -> Self {
        self.max_lines = max_lines;
        self.max_bytes = max_bytes;
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
            description:
                "Read a UTF-8 text file from the current workdir and return numbered lines in the form `<line anchor>|<content>`. `line_start` and `line_end` are optional inclusive line limits; reversed limits are normalized. Binary/NUL-containing files and requests beyond the configured size or line limits fail.".into(),
            input: ToolInputDefinition::new(json_schema::<ReadInput>()),
        }
    }
    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let input: ReadInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid read input: {error}")))?;
        if input.path.is_empty() {
            return Err(Error::Tool("read requires path".into()));
        }
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let (line_start, line_end) = match (input.line_start, input.line_end) {
            (Some(start), Some(end)) if start > end => (Some(end), Some(start)),
            range => range,
        };
        let file_context = match context
            .operations
            .build_file_context(crate::core::models::BuildFileContextRequest {
                path: PathBuf::from(&input.path),
                start_line: line_start,
                end_line: line_end,
                max_lines: Some(self.max_lines),
                max_bytes: Some(self.max_bytes),
            })
            .await
        {
            Ok(file_context) => file_context,
            Err(Error::Workdir(message)) => {
                let workdir = context.operations.workdir()?;
                let content = match path_correction(
                    Path::new(&input.path),
                    workdir.as_ref(),
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
            Err(Error::Tool(content)) => {
                let output = ToolOutput::Failure { content };
                add_output_note(&context, "read", "Read failed", &output).await?;
                return Ok(output);
            }
            Err(error) => return Err(error),
        };
        let selected = file_context
            .lines
            .into_iter()
            .map(|line| format!("{}|{}", line.display_prefix, line.text))
            .collect::<Vec<_>>()
            .join("\n");
        let range = if let Some(end) = line_end {
            format!(":{}-{end}", line_start.unwrap_or(1))
        } else if let Some(start) = line_start {
            format!(":{start}-")
        } else {
            String::new()
        };
        let output = ToolOutput::Success { content: selected };
        add_output_note(
            &context,
            "read",
            format!("Read {}{range}", input.path),
            &output,
        )
        .await?;
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
                Arc::new(ToolRead::new()),
                0,
            )
            .map(|_| ())
    }
}
