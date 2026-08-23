use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::core::{
    Error, Plugin, PluginId, PluginRegistrar, Result, SessionId, Tool, ToolContext, ToolDefinition,
    ToolId, ToolOutput,
};

const PLUGIN_ID: &str = "builtin.tool-todo";
const TODO_TOOL_ID: &str = "builtin.todo";
const TODO_TOOL_NAME: &str = "todo";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
struct TodoItem {
    id: u64,
    content: String,
    status: TodoStatus,
}

#[derive(Clone, Default)]
struct TodoList {
    next_id: u64,
    items: Vec<TodoItem>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum TodoInput {
    List,
    Update { changes: Vec<TodoChange> },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoChange {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    status: Option<TodoStatus>,
    #[serde(default)]
    delete: bool,
}

#[derive(Debug, Serialize)]
struct TodoResponse {
    scope: &'static str,
    items: Vec<TodoItem>,
}

struct TodoPlugin {
    lists: Arc<Mutex<BTreeMap<SessionId, TodoList>>>,
}

struct TodoTool {
    lists: Arc<Mutex<BTreeMap<SessionId, TodoList>>>,
}

pub fn todo_plugin() -> Arc<dyn Plugin> {
    Arc::new(TodoPlugin {
        lists: Arc::new(Mutex::new(BTreeMap::new())),
    })
}

#[async_trait]
impl Plugin for TodoPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        registrar.register_tool(
            0,
            Arc::new(TodoTool {
                lists: self.lists.clone(),
            }),
        )
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn id(&self) -> ToolId {
        ToolId::new(TODO_TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TODO_TOOL_NAME.into(),
            description: "List or update the in-memory todo list for this session.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["action"],
                "properties": {
                    "action": { "type": "string", "enum": ["list", "update"] },
                    "changes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "id": { "type": "integer", "minimum": 1 },
                                "content": { "type": "string", "minLength": 1, "maxLength": 4096 },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] },
                                "delete": { "type": "boolean", "default": false }
                            }
                        }
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput> {
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let input: TodoInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid todo input: {error}")))?;
        let mut all_lists = self
            .lists
            .lock()
            .map_err(|_| Error::Tool("todo list lock is poisoned".into()))?;
        let list = all_lists.entry(context.session_id).or_default();

        if let TodoInput::Update { changes } = input {
            if changes.is_empty() {
                return Err(Error::Tool(
                    "todo update requires at least one change".into(),
                ));
            }
            let mut updated = list.clone();
            for change in changes {
                apply_change(&mut updated, change)?;
            }
            *list = updated;
        }

        let content = serde_json::to_string(&TodoResponse {
            scope: "session",
            items: list.items.clone(),
        })
        .map_err(|error| Error::Tool(format!("could not encode todo output: {error}")))?;
        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

fn apply_change(list: &mut TodoList, change: TodoChange) -> Result<()> {
    match change.id {
        None => {
            if change.delete {
                return Err(Error::Tool("a new todo cannot be deleted".into()));
            }
            let content = validate_content(change.content.as_deref())?;
            list.next_id = list.next_id.saturating_add(1).max(1);
            list.items.push(TodoItem {
                id: list.next_id,
                content: content.to_owned(),
                status: change.status.unwrap_or(TodoStatus::Pending),
            });
        }
        Some(id) => {
            let index = list
                .items
                .iter()
                .position(|item| item.id == id)
                .ok_or_else(|| Error::Tool(format!("todo item {id} was not found")))?;
            if change.delete {
                if change.content.is_some() || change.status.is_some() {
                    return Err(Error::Tool(
                        "a deleted todo may not also set content or status".into(),
                    ));
                }
                list.items.remove(index);
                return Ok(());
            }
            if change.content.is_none() && change.status.is_none() {
                return Err(Error::Tool(format!("todo item {id} has no changes")));
            }
            if let Some(content) = change.content.as_deref() {
                list.items[index].content = validate_content(Some(content))?.to_owned();
            }
            if let Some(status) = change.status {
                list.items[index].status = status;
            }
        }
    }
    Ok(())
}

fn validate_content(content: Option<&str>) -> Result<&str> {
    match content {
        Some(content) if !content.trim().is_empty() && content.len() <= 4096 => Ok(content),
        _ => Err(Error::Tool(
            "todo content must contain non-whitespace text and be at most 4096 bytes".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{core::Core, testkit::StubWorkdir};

    fn context(root: &std::path::Path, session_id: SessionId) -> ToolContext {
        ToolContext {
            project_id: crate::core::ProjectId::new(),
            session_id,
            turn_id: crate::core::TurnId::new(),
            workdir: Arc::new(StubWorkdir::new(root)),
            cancellation: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn creates_lists_and_updates_structured_items() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = SessionId::new();
        let core = Core::new()
            .with_plugin(todo_plugin())
            .build()
            .await
            .unwrap();
        let tool = core.tools().get(&ToolId::new(TODO_TOOL_ID)).unwrap();
        let output = tool
            .execute(
                serde_json::json!({
                    "action": "update",
                    "changes": [{"content": "Run tests"}]
                }),
                context(temp.path(), session_id),
            )
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&output.content).unwrap();
        assert_eq!(response["items"][0]["id"], 1);
        assert_eq!(response["items"][0]["status"], "pending");

        let output = tool
            .execute(
                serde_json::json!({
                    "action": "update",
                    "changes": [{"id": 1, "status": "completed"}]
                }),
                context(temp.path(), session_id),
            )
            .await
            .unwrap();
        assert!(output.content.contains("completed"));
    }

    #[tokio::test]
    async fn list_is_shared_for_same_session_within_plugin() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = SessionId::new();
        let core = Core::new()
            .with_plugin(todo_plugin())
            .build()
            .await
            .unwrap();
        let tool = core.tools().get(&ToolId::new(TODO_TOOL_ID)).unwrap();
        tool.execute(
            serde_json::json!({
                "action": "update",
                "changes": [{"content": "Shared item"}]
            }),
            context(temp.path(), session_id),
        )
        .await
        .unwrap();
        let output = tool
            .execute(
                serde_json::json!({"action": "list"}),
                context(temp.path(), session_id),
            )
            .await
            .unwrap();
        assert!(output.content.contains("Shared item"));
    }

    #[tokio::test]
    async fn separate_sessions_in_the_same_workdir_have_separate_lists() {
        let temp = tempfile::tempdir().unwrap();
        let first_session = SessionId::new();
        let second_session = SessionId::new();
        let core = Core::new()
            .with_plugin(todo_plugin())
            .build()
            .await
            .unwrap();
        let tool = core.tools().get(&ToolId::new(TODO_TOOL_ID)).unwrap();
        tool.execute(
            serde_json::json!({
                "action": "update",
                "changes": [{"content": "Only first"}]
            }),
            context(temp.path(), first_session),
        )
        .await
        .unwrap();
        let output = tool
            .execute(
                serde_json::json!({"action": "list"}),
                context(temp.path(), second_session),
            )
            .await
            .unwrap();
        assert!(!output.content.contains("Only first"));
    }

    #[tokio::test]
    async fn invalid_batch_is_not_partially_applied() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = SessionId::new();
        let core = Core::new()
            .with_plugin(todo_plugin())
            .build()
            .await
            .unwrap();
        let tool = core.tools().get(&ToolId::new(TODO_TOOL_ID)).unwrap();
        let result = tool
            .execute(
                serde_json::json!({
                    "action": "update",
                    "changes": [
                        {"content": "Would otherwise be created"},
                        {"id": 999, "status": "completed"}
                    ]
                }),
                context(temp.path(), session_id),
            )
            .await;
        assert!(result.is_err());

        let output = tool
            .execute(
                serde_json::json!({"action": "list"}),
                context(temp.path(), session_id),
            )
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&output.content).unwrap();
        assert_eq!(response["items"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn state_is_owned_by_plugin_instance_and_omission_removes_tool() {
        let temp = tempfile::tempdir().unwrap();
        let session_id = SessionId::new();
        let first = Core::new()
            .with_plugin(todo_plugin())
            .build()
            .await
            .unwrap();
        first
            .tools()
            .get(&ToolId::new(TODO_TOOL_ID))
            .unwrap()
            .execute(
                serde_json::json!({"action": "update", "changes": [{"content": "private"}]}),
                context(temp.path(), session_id),
            )
            .await
            .unwrap();

        let second = Core::new()
            .with_plugin(todo_plugin())
            .build()
            .await
            .unwrap();
        let output = second
            .tools()
            .get(&ToolId::new(TODO_TOOL_ID))
            .unwrap()
            .execute(
                serde_json::json!({"action": "list"}),
                context(temp.path(), session_id),
            )
            .await
            .unwrap();
        assert!(!output.content.contains("private"));

        let omitted = Core::new().build().await.unwrap();
        assert!(omitted.tools().get(&ToolId::new(TODO_TOOL_ID)).is_none());
    }
}
