use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde_json::{json, Value};

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
struct QuestionInputSchema {
    question: String,
    choices: Option<Vec<String>>,
}

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
            description: "Ask the user for information that the agent cannot safely infer. Provide the question and optional string choices. The question is stored as pending UI/plugin state and this tool returns Stop, ending the current provider turn without cancellation; the user's next response starts a normal new turn.".into(),
            input: ToolInputDefinition::JsonSchema(
                crate::utils::schema::json_schema::<QuestionInputSchema>(),
            ),
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput> {
        let ToolInput::Json(input) = input else {
            return Err(Error::Tool("question input must be an object".into()));
        };
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
            let output = ToolOutput::Failure {
                content: "question choices must be strings".into(),
            };
            add_tool_note(
                &context,
                NoteContent::Alert {
                    content: output.content().unwrap_or("Question failed").into(),
                },
                "question",
            )
            .await?;
            return Ok(output);
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
        let choices_text = choices
            .iter()
            .filter_map(Value::as_str)
            .map(|choice| format!("- {choice}"))
            .collect::<Vec<_>>()
            .join("\n");
        let content = if choices_text.is_empty() {
            question.to_string()
        } else {
            format!("{question}\n\n{choices_text}")
        };
        add_tool_note(&context, NoteContent::Info { content }, "question").await?;
        Ok(ToolOutput::Stop)
    }
}

pub struct ToolQuestionPlugin {
    id: PluginId,
    tool: Arc<ToolQuestion>,
}
impl ToolQuestionPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            tool: Arc::new(ToolQuestion::new()),
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
