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

pub struct ToolRename {
    id: ToolId,
}

impl ToolRename {
    pub fn new() -> Self {
        Self { id: ToolId::new() }
    }
}

impl Default for ToolRename {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolRename {
    fn id(&self) -> ToolId {
        self.id
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "rename".into(),
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
            .ok_or_else(|| Error::Tool("rename requires a non-empty from path".into()))?;
        let to = input
            .get("to")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| Error::Tool("rename requires a non-empty to path".into()))?;
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
            "rename",
            format!("Renamed {from} to {to}"),
            &output,
        )
        .await?;
        Ok(output)
    }
}

pub struct ToolRenamePlugin {
    id: PluginId,
    tool: Arc<ToolRename>,
}

impl ToolRenamePlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolRename::new()),
        }
    }
}

#[async_trait]
impl Plugin for ToolRenamePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "tool_rename"
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
