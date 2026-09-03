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

pub struct ToolFsRename {
    id: ToolId,
}

impl ToolFsRename {
    pub fn new() -> Self {
        Self { id: ToolId::new() }
    }
}

impl Default for ToolFsRename {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolFsRename {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs_rename".into(),
            description: "Rename or move one file. Pass `{ \"from\": \"...\", \"to\": \"...\" }`."
                .into(),
            input: ToolInputDefinition::new(json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["from", "to"]
            })),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let from = input
            .get("from")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| Error::Tool("fs_rename requires a non-empty from path".into()))?;
        let to = input
            .get("to")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| Error::Tool("fs_rename requires a non-empty to path".into()))?;
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let output = match context.workdir.rename(Path::new(from), Path::new(to)).await {
            Ok(()) => ToolOutput::Success {
                content: format!("Renamed {from} to {to}"),
            },
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => ToolOutput::Failure {
                content: error.to_string(),
            },
        };
        add_output_note(
            &context,
            "fs_rename",
            format!("Renamed {from} to {to}"),
            &output,
        )
        .await?;
        Ok(output)
    }
}

pub struct ToolFsRenamePlugin {
    id: PluginId,
    tool: Arc<ToolFsRename>,
}

impl ToolFsRenamePlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolFsRename::new()),
        }
    }
}

#[async_trait]
impl Plugin for ToolFsRenamePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_fs_rename"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
