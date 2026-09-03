use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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
    utils::{note::add_output_note, schema::json_schema},
};

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
}

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
            description: "Create a file or replace its entire UTF-8 content. Pass `{ \"path\": \"...\", \"content\": \"...\" }`.".into(),
            input: ToolInputDefinition::new(json_schema::<WriteInput>()).with_freeform(
                "Create a file or replace its entire UTF-8 content. Put the path on the first line and all file content after the first newline.",
                parse_write_freeform,
            ),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let input: WriteInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid write input: {error}")))?;
        if input.path.is_empty() {
            return Err(Error::Tool("write requires a non-empty path".into()));
        }
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let output = match context
            .operations
            .workdir()?
            .write(Path::new(&input.path), input.content.as_bytes())
            .await
        {
            Ok(()) => ToolOutput::Success {
                content: format!("Wrote {}", input.path),
            },
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => ToolOutput::Failure {
                content: error.to_string(),
            },
        };
        add_output_note(&context, "write", format!("Wrote {}", input.path), &output).await?;
        Ok(output)
    }
}

pub fn parse_write_freeform(input: &str) -> Result<Value> {
    let (path, content) = input
        .split_once('\n')
        .ok_or_else(|| Error::Tool("write freeform input requires a path line".into()))?;
    let path = path.strip_suffix('\r').unwrap_or(path);
    if path.is_empty() {
        return Err(Error::Tool("write path must not be empty".into()));
    }
    serde_json::to_value(WriteInput {
        path: path.into(),
        content: content.into(),
    })
    .map_err(|error| Error::Tool(format!("invalid write input: {error}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeform_uses_path_on_first_line_and_preserves_content() {
        assert_eq!(
            parse_write_freeform("dir/my file.txt\r\nfirst\n<<<EOF\nlast")
                .expect("valid write input"),
            serde_json::json!({
                "path": "dir/my file.txt",
                "content": "first\n<<<EOF\nlast"
            })
        );
        assert_eq!(
            parse_write_freeform("empty.txt\n").expect("empty content is valid"),
            serde_json::json!({ "path": "empty.txt", "content": "" })
        );
    }

    #[test]
    fn freeform_requires_a_non_empty_path_line() {
        assert!(parse_write_freeform("missing-newline").is_err());
        assert!(parse_write_freeform("\ncontent").is_err());
    }
}
