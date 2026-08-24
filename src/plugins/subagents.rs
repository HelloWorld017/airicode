use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::{
    Context, Error, Message, MessagePart, Plugin, PluginId, PluginRegistrar, ProviderEvent,
    ProviderMode, ProviderRequest, Result, Role, Tool, ToolCallId, ToolContext, ToolDefinition,
    ToolId, ToolOutput,
};

const PLUGIN_ID: &str = "builtin.subagents";
const TOOL_ID: &str = "builtin.subagents.subagent";
const TOOL_NAME: &str = "subagent";

#[derive(Clone, Debug)]
pub struct SubagentConfig {
    /// Uses the current turn's model when unset.
    pub model: Option<String>,
    pub max_depth: usize,
    pub max_rounds: usize,
    pub max_output_bytes: usize,
    pub max_tool_calls: usize,
    /// Reserved provider-facing names. Core currently does not expose its ToolRegistry through
    /// ToolContext, so subagents deny all tool calls even when a name is listed here.
    pub tool_allowlist: BTreeSet<String>,
}

impl SubagentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            max_depth: 2,
            max_rounds: 8,
            max_output_bytes: 256 * 1024,
            max_tool_calls: 32,
            tool_allowlist: BTreeSet::new(),
        }
    }
}

struct SubagentPlugin {
    config: SubagentConfig,
}

pub fn subagents_plugin(config: SubagentConfig) -> Arc<dyn Plugin> {
    Arc::new(SubagentPlugin { config })
}

#[async_trait]
impl Plugin for SubagentPlugin {
    fn id(&self) -> PluginId {
        PluginId::new(PLUGIN_ID)
    }

    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()> {
        validate_config(&self.config)?;
        registrar.register_tool(
            0,
            Arc::new(SubagentTool {
                config: self.config.clone(),
            }),
        )
    }
}

struct SubagentTool {
    config: SubagentConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubagentInput {
    prompt: String,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    depth: usize,
}

#[derive(Serialize)]
struct SubagentOutput {
    text: String,
    rounds: usize,
    tool_calls: usize,
    truncated: bool,
}

#[async_trait]
impl Tool for SubagentTool {
    fn id(&self) -> ToolId {
        ToolId::new(TOOL_ID)
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: TOOL_NAME.into(),
            description: "Run an isolated read-only subagent with bounded depth, rounds, output, and tool-call handling. Registered core tools are unavailable to subagents because ToolContext does not expose the ToolRegistry.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "prompt": { "type": "string", "minLength": 1 },
                    "system": { "type": "string" },
                    "depth": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Nesting depth; direct children use zero."
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let input: SubagentInput = serde_json::from_value(input)
            .map_err(|error| Error::Tool(format!("invalid subagent input: {error}")))?;
        if input.prompt.is_empty() {
            return Err(Error::Tool("subagent prompt may not be empty".into()));
        }
        if input.depth >= self.config.max_depth {
            return Err(Error::Tool(format!(
                "subagent depth {} reaches configured maximum {}",
                input.depth, self.config.max_depth
            )));
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
            return Err(Error::Provider("subagent model may not be empty".into()));
        }
        let cancellation = context.cancellation.child_token();

        // The trailing assistant message contains the call currently executing and would be an
        // unresolved tool call in the child transcript.
        let mut messages = context.messages().to_vec();
        if messages
            .last()
            .is_some_and(|message| message.role == Role::Assistant)
        {
            messages.pop();
        }
        let mut prefix = vec![Message::text(
            Role::System,
            format!(
                "You are an isolated read-only subagent. Working directory: {}. No tools are available.",
                context.workdir.root().display()
            ),
        )];
        if let Some(system) = input.system.filter(|value| !value.is_empty()) {
            prefix.push(Message::text(Role::System, system));
        }
        prefix.append(&mut messages);
        let mut messages = prefix;
        messages.push(Message::text(Role::User, input.prompt));

        let mut output = SubagentOutput {
            text: String::new(),
            rounds: 0,
            tool_calls: 0,
            truncated: false,
        };
        for round in 1..=self.config.max_rounds {
            output.rounds = round;
            let request = ProviderRequest {
                mode: ProviderMode::Subagent,
                model: model.clone(),
                messages: messages.clone(),
                tools: Vec::new(),
                context: Context::default(),
                cancellation: cancellation.clone(),
            };
            let mut stream = tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                response = provider.request(request) => response?,
            };
            let mut assistant = AssistantBuffer::default();
            while let Some(event) = tokio::select! {
                _ = cancellation.cancelled() => return Err(Error::Cancelled),
                event = stream.next() => event,
            } {
                assistant.push(event?, self.config.max_output_bytes)?;
            }
            let (assistant_message, calls, text, truncated) = assistant.finish()?;
            append_bounded(
                &mut output.text,
                &text,
                self.config.max_output_bytes,
                &mut output.truncated,
            );
            output.truncated |= truncated;
            messages.push(assistant_message);
            if calls.is_empty() {
                let content = serde_json::to_string(&output).map_err(|error| {
                    Error::Tool(format!("could not encode subagent output: {error}"))
                })?;
                return Ok(ToolOutput {
                    content,
                    is_error: false,
                });
            }
            if output.tool_calls.saturating_add(calls.len()) > self.config.max_tool_calls {
                return Err(Error::Provider(format!(
                    "subagent exceeded {} tool calls",
                    self.config.max_tool_calls
                )));
            }
            for (call_id, name) in calls {
                output.tool_calls += 1;
                let reason = if self.config.tool_allowlist.contains(&name) {
                    format!("tool {name} cannot run: core ToolRegistry is unavailable to subagents")
                } else {
                    format!("tool {name} is not allowed for this subagent")
                };
                messages.push(Message::new(
                    Role::Tool,
                    vec![MessagePart::ToolResult {
                        call_id,
                        content: reason,
                        is_error: true,
                    }],
                ));
            }
        }
        Err(Error::Provider(format!(
            "subagent tool loop exceeded {} provider rounds",
            self.config.max_rounds
        )))
    }
}

#[derive(Default)]
struct PendingCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Default)]
struct AssistantBuffer {
    text: String,
    reasoning: String,
    calls: BTreeMap<u32, PendingCall>,
    truncated: bool,
}

type SubagentAssistant = (Message, Vec<(ToolCallId, String)>, String, bool);

impl AssistantBuffer {
    fn push(&mut self, event: ProviderEvent, limit: usize) -> Result<()> {
        match event {
            ProviderEvent::TextDelta { text } => {
                append_bounded(&mut self.text, &text, limit, &mut self.truncated)
            }
            ProviderEvent::ReasoningDelta { text } => {
                append_bounded(&mut self.reasoning, &text, limit, &mut self.truncated)
            }
            ProviderEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => {
                let call = self.calls.entry(index).or_default();
                if let Some(id) = id {
                    call.id = Some(id);
                }
                if let Some(name) = name {
                    call.name = Some(name);
                }
                if call.arguments.len().saturating_add(arguments.len()) > limit {
                    return Err(Error::Provider(
                        "subagent tool arguments exceed output budget".into(),
                    ));
                }
                call.arguments.push_str(&arguments);
            }
            ProviderEvent::Usage { .. } | ProviderEvent::Finished { .. } => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<SubagentAssistant> {
        let mut parts = Vec::new();
        if !self.reasoning.is_empty() {
            parts.push(MessagePart::Reasoning {
                text: self.reasoning,
            });
        }
        if !self.text.is_empty() {
            parts.push(MessagePart::Text {
                text: self.text.clone(),
            });
        }
        let mut calls = Vec::new();
        for call in self.calls.into_values() {
            let id = ToolCallId::new(
                call.id
                    .ok_or_else(|| Error::Provider("subagent tool call has no id".into()))?,
            );
            let name = call
                .name
                .ok_or_else(|| Error::Provider("subagent tool call has no name".into()))?;
            let arguments = serde_json::from_str(if call.arguments.is_empty() {
                "{}"
            } else {
                &call.arguments
            })
            .map_err(|error| {
                Error::Provider(format!(
                    "invalid arguments for subagent tool {name}: {error}"
                ))
            })?;
            parts.push(MessagePart::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments,
            });
            calls.push((id, name));
        }
        Ok((
            Message::new(Role::Assistant, parts),
            calls,
            self.text,
            self.truncated,
        ))
    }
}

fn validate_config(config: &SubagentConfig) -> Result<()> {
    if config
        .model
        .as_ref()
        .is_some_and(|model| model.trim().is_empty())
        || config.max_depth == 0
        || config.max_rounds == 0
        || config.max_output_bytes == 0
        || config.max_tool_calls == 0
        || config
            .tool_allowlist
            .iter()
            .any(|name| name.trim().is_empty())
    {
        return Err(Error::Plugin("invalid subagent configuration".into()));
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
    fn bounds_provider_output_on_utf8_boundary() {
        let mut output = String::new();
        let mut truncated = false;
        append_bounded(&mut output, "ab\u{e9}cd", 3, &mut truncated);
        assert_eq!(output, "ab");
        assert!(truncated);
    }

    #[test]
    fn schema_documents_read_only_mode() {
        let tool = SubagentTool {
            config: SubagentConfig::new("test"),
        };
        assert!(tool.definition().description.contains("read-only"));
        assert!(tool.definition().description.contains("ToolRegistry"));
    }

    #[test]
    fn rejects_exhausted_depth() {
        let config = SubagentConfig::new("test");
        assert!(config.max_depth > 0);
        assert!(validate_config(&config).is_ok());
    }
}
