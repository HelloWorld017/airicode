use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    core::{
        error::{Error, Result},
        models::{PluginId, ToolContext, ToolDefinition, ToolId, ToolOutput},
        plugin::Plugin,
        registry::PluginRegistryScope,
        tool::Tool,
    },
    utils::hashline,
};

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
            description: "Read a text file with stable hashline anchors.".into(),
            input_schema: serde_json::json!({ "type": "object", "required": ["path"], "properties": { "path": { "type": "string" }, "start_line": { "type": "integer", "minimum": 1 }, "end_line": { "type": "integer", "minimum": 1 } } }),
        }
    }
    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
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
    pub fn new(tool: Arc<ToolRead>) -> Self {
        Self {
            id: PluginId::new(),
            tool,
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
