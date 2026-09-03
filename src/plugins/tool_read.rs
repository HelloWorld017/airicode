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
        let file_context = match context
            .operations
            .build_file_context(crate::core::models::BuildFileContextRequest {
                path: PathBuf::from(&input.path),
                start_line: input.start_line,
                end_line: input.end_line,
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
        let range = if let Some(end) = input.end_line {
            format!(":{}-{end}", input.start_line.unwrap_or(1))
        } else if let Some(start) = input.start_line {
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
                Arc::new(ToolRead::new().with_hashline(context.config.tool.enable_hashline)),
                0,
            )
            .map(|_| ())
    }
}
