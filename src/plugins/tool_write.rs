use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    core::{
        error::{Error, Result},
        models::{
            Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
            ToolInputDefinition, ToolOutput,
        },
        registry::PluginRegistryScope,
    },
    utils::note::add_output_note,
};

pub struct ToolWrite {
    id: ToolId,
}

impl ToolWrite {
    pub fn new() -> Self {
        Self { id: ToolId::new() }
    }
}

impl Default for ToolWrite {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolWrite {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write".into(),
            description: "Create a file or replace its entire UTF-8 content. Pass `{ \"path\": \"...\", \"content\": \"...\" }`. When freeform input is enabled, use `WRITE path <<<TAG`, literal content, then `TAG` on its own line.".into(),
            input: ToolInputDefinition::new(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            })).with_freeform_parser(parse_write_freeform),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| Error::Tool("write requires a non-empty path".into()))?;
        let content = input
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("write requires content".into()))?;
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let output = match context
            .workdir
            .write(Path::new(path), content.as_bytes())
            .await
        {
            Ok(()) => ToolOutput::Success {
                content: format!("Wrote {path}"),
            },
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => ToolOutput::Failure {
                content: error.to_string(),
            },
        };
        add_output_note(&context, "write", format!("Wrote {path}"), &output).await?;
        Ok(output)
    }
}

pub fn parse_write_freeform(input: &str) -> Result<Value> {
    let (header, body) = input
        .split_once('\n')
        .ok_or_else(|| Error::Tool("write freeform input requires a header and content".into()))?;
    let header = header.strip_suffix('\r').unwrap_or(header);
    let specification = header
        .strip_prefix("WRITE ")
        .ok_or_else(|| Error::Tool("write header must start with `WRITE `".into()))?;
    let (path, tag) = specification
        .rsplit_once(" <<<")
        .ok_or_else(|| Error::Tool("write header must end with `<<<TAG`".into()))?;
    if path.trim().is_empty() || tag.is_empty() || tag.chars().any(char::is_whitespace) {
        return Err(Error::Tool(
            "write header has an invalid path or tag".into(),
        ));
    }
    let closing = format!("\n{tag}");
    let content = body
        .strip_suffix(&closing)
        .or_else(|| body.strip_suffix(&format!("\n{tag}\n")))
        .ok_or_else(|| Error::Tool("write content is missing its closing tag".into()))?;
    Ok(json!({ "path": path.trim(), "content": content }))
}

pub struct ToolWritePlugin {
    id: PluginId,
    tool: Arc<ToolWrite>,
}

impl ToolWritePlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolWrite::new()),
        }
    }
}

#[async_trait]
impl Plugin for ToolWritePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_write"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
