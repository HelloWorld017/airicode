use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::{
    Context, Error, FinishReason, Message, Plugin, PluginId, PluginRegistrar, ProviderEvent,
    ProviderRequest, Result, Role, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput, Usage,
};

const PLUGIN_ID: &str = "builtin.sidequery";
const TOOL_ID: &str = "builtin.sidequery.query";
const TOOL_NAME: &str = "sidequery";

#[derive(Clone, Debug)]
pub struct SideQueryConfig {
    /// Uses the current turn's model when unset.
    pub model: Option<String>,
    pub max_output_bytes: usize,
    pub timeout: Duration,
}

impl SideQueryConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            max_output_bytes: 128 * 1024,
            timeout: Duration::from_secs(60),
        }
    }
}

struct SideQueryPlugin {
    config: SideQueryConfig,
}

pub fn sidequery_plugin(config: SideQueryConfig) -> Arc<dyn Plugin> {
    Arc::new(SideQueryPlugin { config })
}

#[async_trait]
impl Plugin for SideQueryPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        validate_config(&self.config)?;
        registrar.register_tool(
            0,
            Arc::new(SideQueryTool {
                config: self.config.clone(),
            }),
        )
    }
}

struct SideQueryTool {
    config: SideQueryConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SideQueryInput {
    prompt: String,
    #[serde(default)]
    system: Option<String>,
}

#[derive(Serialize)]
struct SideQueryOutput {
    text: String,
    truncated: bool,
    finish_reason: Option<FinishReason>,
    usage: Option<Usage>,
}

#[async_trait]
impl Tool for SideQueryTool {
    fn id(&self) -> ToolId {
        ToolId::new(TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_NAME.into(),
            description: "Run an isolated, tool-free provider query. The query cannot modify the current conversation or workdir.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "prompt": { "type": "string", "minLength": 1 },
                    "system": { "type": "string" }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let input: SideQueryInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid sidequery input: {error}")))?;
        if input.prompt.is_empty() {
            return Err(Error::Tool("sidequery prompt may not be empty".into()));
        }
        if context.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let provider_id = context.provider_id().clone();
        let provider = context.providers().get(&provider_id).ok_or_else(|| {
            Error::Provider(format!("current provider {provider_id} is not registered"))
        })?;
        let model = self
            .config
            .model
            .as_deref()
            .unwrap_or_else(|| context.model())
            .to_owned();
        if model.trim().is_empty() {
            return Err(Error::Provider("sidequery model may not be empty".into()));
        }
        let cancellation = context.cancellation.child_token();
        let mut messages = Vec::new();
        if let Some(system) = input.system.filter(|value| !value.is_empty()) {
            messages.push(Message::text(Role::System, system));
        }
        messages.push(Message::text(Role::User, input.prompt));

        context
            .emit_feature(
                "sidequery.started",
                json!({ "provider_id": provider_id.as_str(), "model": model }),
            )
            .await?;
        let request = ProviderRequest {
            model: model.clone(),
            messages,
            tools: Vec::new(),
            context: Context::default(),
            cancellation: cancellation.clone(),
        };
        let operation = async {
            let mut stream = provider.request(request).await?;
            let mut output = SideQueryOutput {
                text: String::new(),
                truncated: false,
                finish_reason: None,
                usage: None,
            };
            while let Some(event) = tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                event = stream.next() => event,
            } {
                match event? {
                    ProviderEvent::TextDelta { text } => append_bounded(
                        &mut output.text,
                        &text,
                        self.config.max_output_bytes,
                        &mut output.truncated,
                    ),
                    ProviderEvent::Usage { usage } => output.usage = Some(usage),
                    ProviderEvent::Finished { reason } => output.finish_reason = Some(reason),
                    ProviderEvent::ReasoningDelta { .. } | ProviderEvent::ToolCallDelta { .. } => {}
                }
            }
            Ok::<_, Error>(output)
        };
        let result = tokio::select! {
            _ = cancellation.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(self.config.timeout, operation) => match result {
                Ok(result) => result,
                Err(_) => {
                    cancellation.cancel();
                    Err(Error::Provider("sidequery timed out".into()))
                }
            },
        };
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                let _ = context
                    .emit_feature(
                        "sidequery.failed",
                        json!({
                            "provider_id": provider_id.as_str(),
                            "model": model,
                            "error": error.to_string()
                        }),
                    )
                    .await;
                return Err(error);
            }
        };
        context
            .emit_feature(
                "sidequery.completed",
                json!({
                    "provider_id": provider_id.as_str(),
                    "model": model,
                    "output_bytes": output.text.len(),
                    "truncated": output.truncated
                }),
            )
            .await?;
        let content = serde_json::to_string(&output)
            .map_err(|error| Error::Tool(format!("could not encode sidequery output: {error}")))?;
        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

fn validate_config(config: &SideQueryConfig) -> Result<()> {
    if config
        .model
        .as_ref()
        .is_some_and(|model| model.trim().is_empty())
        || config.max_output_bytes == 0
        || config.timeout.is_zero()
    {
        return Err(Error::Plugin("invalid sidequery configuration".into()));
    }
    Ok(())
}

fn append_bounded(target: &mut String, value: &str, limit: usize, truncated: &mut bool) {
    let remaining = limit.saturating_sub(target.len());
    if value.len() <= remaining {
        target.push_str(value);
        return;
    }
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= remaining)
        .last()
        .unwrap_or(0);
    target.push_str(&value[..boundary]);
    *truncated = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_utf8_output() {
        let mut output = String::new();
        let mut truncated = false;
        append_bounded(&mut output, "ab\u{e9}cd", 3, &mut truncated);
        assert_eq!(output, "ab");
        assert!(truncated);
    }

    #[test]
    fn schema_declares_isolation() {
        let tool = SideQueryTool {
            config: SideQueryConfig::new("test"),
        };
        assert!(tool.definition().description.contains("tool-free"));
        assert_eq!(tool.definition().name, TOOL_NAME);
    }
}
