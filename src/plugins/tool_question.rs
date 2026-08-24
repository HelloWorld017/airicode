use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::core::{
    error::{Error, Result},
    models::{Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput},
    registry::PluginRegistryScope,
};

pub struct ToolQuestion {
    id: ToolId,
}
impl ToolQuestion {
    pub fn new() -> Self {
        Self { id: ToolId::new() }
    }
}
impl Default for ToolQuestion {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolQuestion {
    fn id(&self) -> ToolId {
        self.id
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "question".into(),
            description: "Ask the user a question and stop this turn.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["question"],
                "properties": { "question": { "type": "string" }, "choices": { "type": "array", "items": { "type": "string" } } }
            }),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let question = input
            .get("question")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("question requires question".into()))?;
        let choices = input
            .get("choices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if choices.iter().any(|choice| !choice.is_string()) {
            return Ok(ToolOutput::Failure {
                content: "question choices must be strings".into(),
            });
        }
        context
            .operations
            .update_plugin_state(
                "question",
                json!({
                    "question": question,
                    "choices": choices,
                    "turn_id": context.turn_id.to_string(),
                }),
            )
            .await?;
        Ok(ToolOutput::Stop)
    }
}

pub struct ToolQuestionPlugin {
    id: PluginId,
    tool: Arc<ToolQuestion>,
}
impl ToolQuestionPlugin {
    pub fn new(tool: Arc<ToolQuestion>) -> Self {
        Self {
            id: PluginId::new(),
            tool,
        }
    }
}

#[async_trait]
impl Plugin for ToolQuestionPlugin {
    fn id(&self) -> PluginId {
        self.id
    }
    fn name(&self) -> &str {
        "tool_question"
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
