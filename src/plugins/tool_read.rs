use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde_json::Value;

use crate::{
    core::{
        error::{Error, Result},
        models::{
            Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
            ToolInputDefinition, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::hashline,
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
            description: r#"Read a UTF-8 text file from the root-relative workdir and return hashline-annotated lines in the form `<line>:<hash>|<content>`. Use this output as the source of truth before editing: a patch must copy the hash after the colon exactly, and must not invent or modify hashline anchors. `start_line` and `end_line` are optional inclusive line limits. Binary/NUL-containing files and requests beyond the configured size or line limits fail."#.into(),
            input: ToolInputDefinition::JsonSchema(
                crate::utils::schema::json_schema::<ReadInputSchema>(),
            ),
        }
    }
    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let ToolInput::Json(input) = input else {
            return Err(Error::Tool("read input must be an object".into()));
        };
        let object = input
            .as_object()
            .ok_or_else(|| Error::Tool("read input must be an object".into()))?;
        let path = object
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("read requires path".into()))?;
        let bytes = match context.workdir.read(Path::new(path)).await {
            Ok(bytes) => bytes,
            Err(Error::Workdir(message)) => return Ok(ToolOutput::Failure { content: message }),
            Err(error) => return Err(error),
        };
        if bytes.contains(&0) {
            return Ok(ToolOutput::Failure {
                content: "cannot read binary/NUL-containing input".into(),
            });
        }
        if bytes.len() > self.max_bytes {
            return Ok(ToolOutput::Failure {
                content: format!("file exceeds read limit of {} bytes", self.max_bytes),
            });
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
            .unwrap_or(1) as usize;
        let end = object
            .get("end_line")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(all.len());
        if start == 0 || end < start {
            return Ok(ToolOutput::Failure {
                content: "invalid line range".into(),
            });
        }
        if end - start + 1 > self.max_lines {
            return Ok(ToolOutput::Failure {
                content: format!("line range exceeds read limit of {} lines", self.max_lines),
            });
        }
        let selected = all
            .into_iter()
            .filter(|line| line.line >= start && line.line <= end)
            .map(|line| format!("{}:{}|{}", line.line, line.tag, line.text))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::Success { content: selected })
    }
}

pub struct ToolReadPlugin {
    id: PluginId,
    tool: Arc<ToolRead>,
}
impl ToolReadPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolRead::new()),
        }
    }
    pub fn tool(&self) -> Arc<ToolRead> {
        self.tool.clone()
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
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
