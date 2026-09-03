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

pub struct ToolFsDelete {
    id: ToolId,
}

impl ToolFsDelete {
    pub fn new() -> Self {
        Self { id: ToolId::new() }
    }
}

impl Default for ToolFsDelete {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolFsDelete {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs_delete".into(),
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
            .ok_or_else(|| Error::Tool("fs_delete requires a non-empty path".into()))?;
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
        add_output_note(&context, "fs_delete", format!("Deleted {path}"), &output).await?;
        Ok(output)
    }
}

pub struct ToolFsDeletePlugin {
    id: PluginId,
    tool: Arc<ToolFsDelete>,
}

impl ToolFsDeletePlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolFsDelete::new()),
        }
    }
}

#[async_trait]
impl Plugin for ToolFsDeletePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_fs_delete"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
