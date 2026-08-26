use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::{
    error::{Error, Result},
    models::{
        Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolInput,
        ToolInputDefinition, ToolOutput,
    },
    registry::PluginRegistryScope,
};
use crate::{core::models::NoteContent, plugins::add_tool_note};

#[allow(dead_code)]
#[derive(JsonSchema)]
struct TodoInputSchema {
    todos: Vec<TodoItemSchema>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
struct TodoItemSchema {
    content: String,
    status: Option<String>,
    priority: Option<String>,
}

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
            description: "Replace the durable session todo list with the complete array supplied in `todos`; send the full desired state rather than a delta. Each item needs `content` and may use `pending`, `in_progress`, `completed`, or `cancelled` status plus a priority. The updated list is stored in the todo plugin namespace and a short count is returned.".into(),
            input: ToolInputDefinition::JsonSchema(
                crate::utils::schema::json_schema::<TodoInputSchema>(),
            ),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let ToolInput::Json(input) = input else {
            return Err(Error::Tool("todo input must be an object".into()));
        };
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
                let output = ToolOutput::Failure {
                    content: format!("invalid todo status: {status}"),
                };
                add_tool_note(
                    &context,
                    NoteContent::Alert {
                        content: output.content().unwrap_or("Todo update failed").into(),
                    },
                    "todo",
                )
                .await?;
                return Ok(output);
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
        let content = normalized
            .iter()
            .map(|todo| {
                let marker = match todo.status.as_str() {
                    "completed" => "x",
                    "cancelled" => "-",
                    "in_progress" => ">",
                    _ => " ",
                };
                format!("- [{marker}] {}", todo.content)
            })
            .collect::<Vec<_>>()
            .join("\n");
        add_tool_note(
            &context,
            NoteContent::Info {
                content: format!("Todo list ({} items)\n\n{content}", normalized.len()),
            },
            "todo",
        )
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
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolTodo::new()),
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
