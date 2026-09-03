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

pub struct ToolDelete {
    id: ToolId,
}

impl ToolDelete {
    pub fn new() -> Self {
        Self { id: ToolId::new() }
    }
}

impl Default for ToolDelete {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolDelete {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete".into(),
            description: "Delete one file. Pass `{ \"path\": \"...\" }`.".into(),
            input: ToolInputDefinition::new(json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            })),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| Error::Tool("delete requires a non-empty path".into()))?;
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let output = match context.workdir.remove(Path::new(path)).await {
            Ok(()) => ToolOutput::Success {
                content: format!("Deleted {path}"),
            },
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => ToolOutput::Failure {
                content: error.to_string(),
            },
        };
        add_output_note(&context, "delete", format!("Deleted {path}"), &output).await?;
        Ok(output)
    }
}

pub struct ToolDeletePlugin {
    id: PluginId,
    tool: Arc<ToolDelete>,
}

impl ToolDeletePlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolDelete::new()),
        }
    }
}

#[async_trait]
impl Plugin for ToolDeletePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_delete"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
