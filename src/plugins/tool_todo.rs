use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::{
    error::{Error, Result},
    models::{Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput},
    registry::PluginRegistryScope,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
    pub priority: String,
}

pub struct ToolTodo {
    id: ToolId,
}
impl ToolTodo {
    pub fn new() -> Self {
        Self { id: ToolId::new() }
    }
}
impl Default for ToolTodo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolTodo {
    fn id(&self) -> ToolId {
        self.id
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "todo".into(),
            description: "Replace the session todo list.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["todos"],
                "properties": { "todos": { "type": "array" } }
            }),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let todos = input
            .get("todos")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Tool("todo requires todos array".into()))?;
        let mut normalized = Vec::with_capacity(todos.len());
        for todo in todos {
            let object = todo
                .as_object()
                .ok_or_else(|| Error::Tool("todo item must be an object".into()))?;
            let content = object
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Tool("todo item requires content".into()))?;
            let status = object
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let priority = object
                .get("priority")
                .and_then(Value::as_str)
                .unwrap_or("medium");
            if !matches!(
                status,
                "pending" | "in_progress" | "completed" | "cancelled"
            ) {
                return Ok(ToolOutput::Failure {
                    content: format!("invalid todo status: {status}"),
                });
            }
            normalized.push(TodoItem {
                content: content.into(),
                status: status.into(),
                priority: priority.into(),
            });
        }
        let value = serde_json::to_value(&normalized)?;
        context
            .operations
            .update_plugin_state("todo", value)
            .await?;
        Ok(ToolOutput::Success {
            content: format!("Updated todo list ({} items)", normalized.len()),
        })
    }
}

pub struct ToolTodoPlugin {
    id: PluginId,
    tool: Arc<ToolTodo>,
}
impl ToolTodoPlugin {
    pub fn new(tool: Arc<ToolTodo>) -> Self {
        Self {
            id: PluginId::new(),
            tool,
        }
    }
}

#[async_trait]
impl Plugin for ToolTodoPlugin {
    fn id(&self) -> PluginId {
        self.id
    }
    fn name(&self) -> &str {
        "tool_todo"
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
