use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::{
    error::{Error, Result},
    hooks::{ConfigReadContext, ConfigReadHook},
    models::{
        FinishReason, Message, MessagePart, MessagePartContent, Model, ModelCapabilities, Plugin,
        PluginId, Provider, ProviderEvent, ProviderId, ProviderRequest, ProviderStream, Role,
        ToolCallId, ToolDefinition, Usage,
    },
    registry::PluginRegistryScope,
};
use uuid::Uuid;

pub struct OpenAiProvider {
    id: ProviderId,
    api_key: Arc<RwLock<String>>,
    base_url: Arc<RwLock<String>>,
    freeform: bool,
    client: Client,
}

impl OpenAiProvider {
    pub fn new(id: ProviderId, api_key: impl Into<String>) -> Self {
        Self {
            id,
            api_key: Arc::new(RwLock::new(api_key.into())),
            base_url: Arc::new(RwLock::new("https://openrouter.ai/api/v1".into())),
            freeform: false,
            client: Client::new(),
        }
    }

    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        self.set_base_url(base_url);
        self
    }

    pub fn with_api_key(self, api_key: impl Into<String>) -> Self {
        self.set_api_key(api_key);
        self
    }

    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    pub fn with_freeform(mut self, freeform: bool) -> Self {
        self.freeform = freeform;
        self
    }

    fn endpoint(&self, path: &str) -> String {
        let base_url = self.base_url.read().expect("OpenAI base URL lock poisoned");
        format!("{}{path}", *base_url)
    }

    fn api_key(&self) -> String {
        self.api_key
            .read()
            .expect("OpenAI API key lock poisoned")
            .clone()
    }

    fn set_base_url(&self, base_url: impl Into<String>) {
        if let Ok(mut value) = self.base_url.write() {
            *value = base_url.into().trim_end_matches('/').to_string();
        }
    }

    fn set_api_key(&self, api_key: impl Into<String>) {
        if let Ok(mut value) = self.api_key.write() {
            *value = api_key.into();
        }
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelRecord>,
}

#[derive(Deserialize)]
struct ModelRecord {
    id: String,
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        self.id
    }

    async fn get_models(&self) -> Result<Vec<Model>> {
        let response = self
            .client
            .get(self.endpoint("/models"))
            .bearer_auth(self.api_key())
            .send()
            .await
            .map_err(|error| Error::Provider(format!("OpenAI model request failed: {error}")))?;
        let response = ensure_success(response).await?;
        let models = response
            .json::<ModelsResponse>()
            .await
            .map_err(|error| Error::Provider(format!("invalid OpenAI model response: {error}")))?;
        Ok(models
            .data
            .into_iter()
            .map(|model| Model {
                display_name: model.id.clone(),
                id: model.id,
                capabilities: ModelCapabilities {
                    context_window: None,
                    tools: true,
                    reasoning: true,
                },
            })
            .collect())
    }

    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream> {
        let provider_id = self.id;
        let body = responses_body(&request, provider_id, self.freeform);
        let response = self
            .client
            .post(self.endpoint("/responses"))
            .bearer_auth(self.api_key())
            .json(&body)
            .send()
            .await
            .map_err(|error| Error::Provider(format!("OpenAI request failed: {error}")))?;
        let response = ensure_success(response).await?;
        let mut bytes = response.bytes_stream();
        let cancellation = request.cancellation;
        let stream = try_stream! {
            let mut buffer = Vec::new();
            let mut output_items = BTreeMap::new();
            let mut saw_tool_call = false;
            let mut done = false;
            while !done {
                let chunk = tokio::select! {
                    _ = cancellation.cancelled() => Some(Err(Error::Cancelled)),
                    chunk = bytes.next() => chunk.map(|value| value.map_err(|error| Error::Provider(format!("OpenAI stream failed: {error}")))),
                };
                if let Some(chunk) = chunk {
                    let chunk = chunk?;
                    buffer.extend_from_slice(&chunk);
                } else {
                    if buffer.is_empty() {
                        break;
                    }
                    buffer.push(b'\n');
                }
                while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line = buffer.drain(..=position).collect::<Vec<_>>();
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim();
                    if !line.starts_with("data:") {
                        continue;
                    }
                    let data = line[5..].trim();
                    if data == "[DONE]" {
                        done = true;
                        break;
                    }
                    if data.is_empty() {
                        continue;
                    }
                    let event = serde_json::from_str::<Value>(data).map_err(|error| {
                        Error::Provider(format!("invalid OpenAI Responses event: {error}"))
                    })?;
                    for event in response_events_with_tools(
                        event,
                        provider_id,
                        &request.tools,
                        &mut output_items,
                        &mut saw_tool_call,
                    )? {
                        yield event;
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

fn responses_body(request: &ProviderRequest, provider_id: ProviderId, freeform: bool) -> Value {
    json!({
        "model": request.model,
        "input": responses_input(&request.messages, provider_id, &request.tools, freeform),
        "tools": request.tools.iter().map(|tool| responses_tool(tool, freeform)).collect::<Vec<_>>(),
        "stream": true,
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "reasoning": { "summary": "auto", "effort": "high" },
    })
}

fn uses_freeform(tool: &ToolDefinition, freeform: bool) -> bool {
    freeform && tool.input.freeform_parser.is_some()
}

fn responses_tool(tool: &ToolDefinition, freeform: bool) -> Value {
    if uses_freeform(tool, freeform) {
        json!({
            "type": "custom",
            "name": tool.name,
            "description": tool.description,
        })
    } else {
        json!({
            "type": "function",
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input.schema,
        })
    }
}

fn responses_input(
    messages: &[Arc<Message>],
    provider_id: ProviderId,
    tools: &[ToolDefinition],
    freeform: bool,
) -> Vec<Value> {
    let mut input = Vec::new();
    for message in messages {
        for part in &message.content {
            if let Some(provider_data) = &part.provider_data {
                if provider_data.provider_id == provider_id {
                    input.push(provider_data.data.clone());
                    continue;
                }
            }

            let Some(content) = &part.content else {
                continue;
            };
            match content {
                MessagePartContent::Text { text } => input.push(json!({
                    "type": "message",
                    "role": response_role(message.role.clone()),
                    "content": [{ "type": "input_text", "text": text }],
                })),
                MessagePartContent::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    let text_input = tools
                        .iter()
                        .any(|tool| tool.name == *name && uses_freeform(tool, freeform));
                    if text_input {
                        input.push(json!({
                            "type": "custom_tool_call",
                            "call_id": id.to_string(),
                            "name": name,
                            "input": response_text_input(arguments),
                        }));
                    } else {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": id.to_string(),
                            "name": name,
                            "arguments": response_function_arguments(arguments),
                        }));
                    }
                }
                MessagePartContent::ToolResult {
                    call_id, result, ..
                } => input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id.to_string(),
                    "output": result.content().unwrap_or(""),
                })),
                MessagePartContent::Reasoning { .. } => {}
            }
        }
    }
    input
}

fn response_role(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "user",
    }
}

fn response_text_input(arguments: &Value) -> String {
    match arguments {
        Value::String(arguments) => arguments.clone(),
        arguments => arguments.to_string(),
    }
}

fn response_function_arguments(arguments: &Value) -> String {
    serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string())
}

#[cfg(test)]
fn response_events(
    event: Value,
    provider_id: ProviderId,
    output_items: &mut BTreeMap<u32, Value>,
    saw_tool_call: &mut bool,
) -> Result<Vec<ProviderEvent>> {
    response_events_with_tools(event, provider_id, &[], output_items, saw_tool_call)
}

fn response_events_with_tools(
    event: Value,
    provider_id: ProviderId,
    tools: &[ToolDefinition],
    output_items: &mut BTreeMap<u32, Value>,
    saw_tool_call: &mut bool,
) -> Result<Vec<ProviderEvent>> {
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Provider("OpenAI Responses event has no type".into()))?;
    let mut result = Vec::new();
    match kind {
        "response.output_item.added" => {
            let index = required_u32(&event, "output_index")?;
            let item = event
                .get("item")
                .cloned()
                .ok_or_else(|| Error::Provider("output item event has no item".into()))?;
            *saw_tool_call |= matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call") | Some("custom_tool_call")
            );
            output_items.insert(index, item);
        }
        "response.output_item.done" => {
            let index = required_u32(&event, "output_index")?;
            let item = event
                .get("item")
                .cloned()
                .ok_or_else(|| Error::Provider("output item event has no item".into()))?;
            *saw_tool_call |= matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call") | Some("custom_tool_call")
            );
            output_items.insert(index, item.clone());
            result.push(ProviderEvent::OutputPart {
                index,
                part: output_item_part(provider_id, item, tools)?,
            });
        }
        "response.output_text.delta" => {
            let delta = required_string(&event, "delta")?;
            if !delta.is_empty() {
                result.push(ProviderEvent::TextDelta { text: delta });
            }
        }
        "response.reasoning_summary_text.delta" => {
            let delta = required_string(&event, "delta")?;
            if !delta.is_empty() {
                result.push(ProviderEvent::ReasoningDelta { text: delta });
            }
        }
        "response.function_call_arguments.delta" => {
            let index = required_u32(&event, "output_index")?;
            let item = output_items.get(&index);
            result.push(ProviderEvent::ToolCallDelta {
                index,
                id: item
                    .and_then(|item| item.get("call_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                name: item
                    .and_then(|item| item.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                arguments: required_string(&event, "delta")?,
            });
        }
        "response.custom_tool_call_input.delta" => {
            let index = required_u32(&event, "output_index")?;
            let item = output_items.get(&index);
            result.push(ProviderEvent::CustomToolCallInputDelta {
                index,
                id: item
                    .and_then(|item| item.get("call_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                name: item
                    .and_then(|item| item.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                input: required_string(&event, "delta")?,
            });
        }
        "response.custom_tool_call_input.done" => {
            let index = required_u32(&event, "output_index")?;
            let input = required_string(&event, "input")?;
            if let Some(item) = output_items.get_mut(&index) {
                item["input"] = Value::String(input.clone());
            }
            result.push(ProviderEvent::CustomToolCallInputDone { index, input });
        }
        "response.completed" => {
            append_usage(&mut result, event.get("response"))?;
            result.push(ProviderEvent::Finished {
                reason: if *saw_tool_call {
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
                },
            });
        }
        "response.incomplete" => {
            append_usage(&mut result, event.get("response"))?;
            let reason = event
                .get("response")
                .and_then(|response| response.get("incomplete_details"))
                .and_then(|details| details.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("incomplete");
            result.push(ProviderEvent::Finished {
                reason: if reason == "max_output_tokens" || reason == "content_filter" {
                    if reason == "max_output_tokens" {
                        FinishReason::Length
                    } else {
                        FinishReason::ContentFilter
                    }
                } else {
                    FinishReason::Other(reason.to_string())
                },
            });
        }
        "response.failed" | "error" => {
            let message = event
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .or_else(|| {
                    event
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                })
                .or_else(|| event.get("message").and_then(Value::as_str))
                .unwrap_or("OpenAI Responses request failed");
            return Err(Error::Provider(message.to_string()));
        }
        _ => {}
    }
    Ok(result)
}

fn output_item_part(
    provider_id: ProviderId,
    item: Value,
    tools: &[ToolDefinition],
) -> Result<MessagePart> {
    Ok(match item.get("type").and_then(Value::as_str) {
        Some("message") => {
            let text = item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|content| {
                    content.get("type").and_then(Value::as_str) == Some("output_text")
                })
                .filter_map(|content| content.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            if text.is_empty() {
                MessagePart::provider_only(provider_id, item)
            } else {
                MessagePart::text(text).with_provider_data(provider_id, item)
            }
        }
        Some("reasoning") => {
            let summary = item
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|summary| summary.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if summary.is_empty() {
                MessagePart::provider_only(provider_id, item)
            } else {
                MessagePart::reasoning(summary).with_provider_data(provider_id, item)
            }
        }
        Some("function_call") => {
            let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                return Ok(MessagePart::provider_only(provider_id, item));
            };
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                return Ok(MessagePart::provider_only(provider_id, item));
            };
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .map(|arguments| {
                    serde_json::from_str(arguments)
                        .unwrap_or_else(|_| Value::String(arguments.to_string()))
                })
                .unwrap_or(Value::Null);
            MessagePart::tool_call(
                ToolCallId::from_external(call_id),
                name.to_string(),
                arguments,
            )
            .with_provider_data(provider_id, item)
        }
        Some("custom_tool_call") => {
            let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                return Ok(MessagePart::provider_only(provider_id, item));
            };
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                return Ok(MessagePart::provider_only(provider_id, item));
            };
            let Some(input) = item.get("input").and_then(Value::as_str) else {
                return Ok(MessagePart::provider_only(provider_id, item));
            };
            let arguments = match tools.iter().find(|tool| tool.name == name) {
                Some(tool) => tool.input.parse_freeform(input).map_err(|error| {
                    Error::Provider(format!("invalid freeform input for {name}: {error}"))
                })?,
                None => Value::String(input.to_string()),
            };
            MessagePart::tool_call(
                ToolCallId::from_external(call_id),
                name.to_string(),
                arguments,
            )
            .with_provider_data(provider_id, item)
        }
        _ => MessagePart::provider_only(provider_id, item),
    })
}

fn required_u32(event: &Value, field: &str) -> Result<u32> {
    event
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| Error::Provider(format!("OpenAI Responses event has no valid {field}")))
}

fn required_string(event: &Value, field: &str) -> Result<String> {
    event
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Provider(format!("OpenAI Responses event has no valid {field}")))
}

fn append_usage(events: &mut Vec<ProviderEvent>, response: Option<&Value>) -> Result<()> {
    let Some(usage) = response.and_then(|response| response.get("usage")) else {
        return Ok(());
    };
    let input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Provider("OpenAI Responses usage has no input_tokens".into()))?;
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Provider("OpenAI Responses usage has no output_tokens".into()))?;
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Provider("OpenAI Responses usage has no total_tokens".into()))?;
    events.push(ProviderEvent::Usage {
        usage: Usage {
            input_tokens,
            output_tokens,
            total_tokens,
        },
    });
    Ok(())
}

async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(Error::Provider(format!("OpenAI returned {status}: {body}")))
}

fn stable_openai_provider_id() -> ProviderId {
    ProviderId::from_uuid(Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        b"airicode/provider/openai",
    ))
}

pub struct OpenAiProviderPlugin {
    id: PluginId,
    provider_id: ProviderId,
    provider: RwLock<Option<Arc<OpenAiProvider>>>,
}

impl OpenAiProviderPlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            provider_id: stable_openai_provider_id(),
            provider: RwLock::new(None),
        }
    }

    pub fn provider_id(&self) -> ProviderId {
        self.provider_id
    }

    pub fn provider(&self) -> Option<Arc<OpenAiProvider>> {
        self.provider
            .read()
            .expect("OpenAI provider lock poisoned")
            .clone()
    }
}

pub type ProviderOpenAIPlugin = OpenAiProviderPlugin;
pub type ProviderOpenAI = OpenAiProvider;
pub type OpenAIProviderPlugin = OpenAiProviderPlugin;
pub type OpenAIProvider = OpenAiProvider;

#[async_trait]
impl Plugin for OpenAiProviderPlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "provider_openai"
    }

    fn config_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "base_url": { "type": "string" },
                "api_key_env": { "type": "string" }
            }
        })
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        let hook: Arc<dyn ConfigReadHook> = self;
        registry.register_hook(hook)
    }
}

#[async_trait]
impl ConfigReadHook for OpenAiProviderPlugin {
    async fn config_read(&self, context: ConfigReadContext) -> Result<()> {
        let config = context
            .config
            .namespace(self.name())
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let key_env = config
            .get("api_key_env")
            .and_then(Value::as_str)
            .unwrap_or("OPENAI_API_KEY");
        let key =
            std::env::var(key_env).map_err(|_| Error::Config(format!("{key_env} is not set")))?;
        let provider = Arc::new(
            OpenAiProvider::new(self.provider_id, key).with_freeform(context.config.tool.freeform),
        );
        if let Some(base_url) = config.get("base_url").and_then(Value::as_str) {
            provider.set_base_url(base_url.to_string());
        }
        context
            .registry
            .register_provider(provider.clone(), 0)
            .map(|_| ())?;
        *self
            .provider
            .write()
            .map_err(|_| Error::Plugin("OpenAI provider lock poisoned".into()))? = Some(provider);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::Result;
    use crate::core::models::{ToolDefinition, ToolInputDefinition, ToolOutput};
    use crate::plugins::{
        tool_fs_write::parse_fs_write_freeform, tool_patch_apply_patch::parse_apply_patch_freeform,
        tool_patch_hashline::parse_patch_hashline_freeform, tool_shell::parse_shell_freeform,
    };
    use tokio_util::sync::CancellationToken;

    #[test]
    fn responses_input_replays_matching_native_items_and_encodes_semantics() {
        let provider_id = ProviderId::new();
        let other_provider = ProviderId::new();
        let native = serde_json::json!({
            "type": "message",
            "id": "msg_native",
            "role": "assistant",
            "content": []
        });
        let mut native_message = Message::text(Role::Assistant, "ignored", "build", None);
        native_message.content[0] =
            MessagePart::text("ignored").with_provider_data(provider_id, native.clone());
        let mut other_message = Message::text(Role::Assistant, "visible", "build", None);
        other_message.content[0] = MessagePart::text("visible")
            .with_provider_data(other_provider, serde_json::json!({ "opaque": true }));
        let reasoning = Message {
            content: vec![MessagePart::reasoning("do not synthesize")],
            ..Message::text(Role::Assistant, "", "build", None)
        };
        let result = Message {
            content: vec![MessagePart::tool_result(
                ToolCallId::from_external("call_1"),
                "ok".into(),
                ToolOutput::Success {
                    content: "tool output".into(),
                },
            )],
            ..Message::text(Role::Tool, "", "build", None)
        };

        let input = responses_input(
            &[
                Arc::new(native_message),
                Arc::new(other_message),
                Arc::new(reasoning),
                Arc::new(result),
            ],
            provider_id,
            &[],
            false,
        );

        assert_eq!(input[0], native);
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["text"], "visible");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["output"], "tool output");
        assert_eq!(input.len(), 3);
    }

    #[test]
    fn responses_body_uses_stateless_streaming_configuration() {
        let request = ProviderRequest {
            model: "gpt-test".into(),
            messages: vec![Arc::new(Message::text(Role::User, "hello", "build", None))],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "read a file".into(),
                input: ToolInputDefinition::new(serde_json::json!({ "type": "object" })),
            }],
            cancellation: CancellationToken::new(),
        };
        let body = responses_body(&request, ProviderId::new(), false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read");
        assert!(body.get("previous_response_id").is_none());
    }

    #[test]
    fn responses_body_uses_custom_tools_only_when_freeform_is_enabled() {
        let tools = vec![
            ToolDefinition {
                name: "shell".into(),
                description: "run a command".into(),
                input: ToolInputDefinition::new(serde_json::json!({ "type": "object" }))
                    .with_freeform_parser(parse_shell_freeform),
            },
            ToolDefinition {
                name: "fs_write".into(),
                description: "write a file".into(),
                input: ToolInputDefinition::new(serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                }))
                .with_freeform_parser(parse_fs_write_freeform),
            },
            ToolDefinition {
                name: "apply_patch".into(),
                description: "apply a patch".into(),
                input: ToolInputDefinition::new(serde_json::json!({ "type": "object" }))
                    .with_freeform_parser(parse_apply_patch_freeform),
            },
            ToolDefinition {
                name: "patch_hashline".into(),
                description: "apply a hashline patch".into(),
                input: ToolInputDefinition::new(serde_json::json!({ "type": "object" }))
                    .with_freeform_parser(parse_patch_hashline_freeform),
            },
            ToolDefinition {
                name: "patch".into(),
                description: "apply replacements".into(),
                input: ToolInputDefinition::new(serde_json::json!({ "type": "object" })),
            },
        ];
        let request = ProviderRequest {
            model: "gpt-test".into(),
            messages: vec![Arc::new(Message::text(Role::User, "hello", "build", None))],
            tools,
            cancellation: CancellationToken::new(),
        };
        let json_body = responses_body(&request, ProviderId::new(), false);
        for tool in json_body["tools"].as_array().unwrap() {
            assert_eq!(tool["type"], "function");
            assert_eq!(tool["parameters"]["type"], "object");
        }

        let freeform_body = responses_body(&request, ProviderId::new(), true);
        for tool in &freeform_body["tools"].as_array().unwrap()[..4] {
            assert_eq!(tool["type"], "custom");
            assert!(tool.get("parameters").is_none());
        }
        assert_eq!(freeform_body["tools"][4]["type"], "function");
    }

    #[test]
    fn responses_input_replays_text_tool_calls_as_custom_items() {
        let shell = ToolDefinition {
            name: "shell".into(),
            description: "run a command".into(),
            input: ToolInputDefinition::new(serde_json::json!({ "type": "object" }))
                .with_freeform_parser(parse_shell_freeform),
        };
        let message = Message {
            content: vec![MessagePart::tool_call(
                ToolCallId::from_external("call_shell"),
                "shell".into(),
                Value::String("printf hello".into()),
            )],
            ..Message::text(Role::Assistant, "", "build", None)
        };
        let input = responses_input(&[Arc::new(message)], ProviderId::new(), &[shell], true);
        assert_eq!(input[0]["type"], "custom_tool_call");
        assert_eq!(input[0]["name"], "shell");
        assert_eq!(input[0]["input"], "printf hello");
        assert!(input[0].get("arguments").is_none());
    }

    #[test]
    fn response_output_items_preserve_native_data_and_call_ids() -> Result<()> {
        let provider_id = ProviderId::new();
        let reasoning_item = serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{ "type": "summary_text", "text": "summary" }],
            "encrypted_content": "encrypted"
        });
        let function_item = serde_json::json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "read",
            "arguments": "{\"path\":\"README.md\"}"
        });
        let mut output_items = BTreeMap::new();
        let mut saw_tool_call = false;
        let reasoning_events = response_events(
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": reasoning_item
            }),
            provider_id,
            &mut output_items,
            &mut saw_tool_call,
        )?;
        let function_events = response_events(
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": function_item
            }),
            provider_id,
            &mut output_items,
            &mut saw_tool_call,
        )?;

        let ProviderEvent::OutputPart { part, .. } = &reasoning_events[0] else {
            panic!("expected finalized reasoning part")
        };
        assert!(matches!(
            part.content.as_ref(),
            Some(MessagePartContent::Reasoning { text }) if text == "summary"
        ));
        assert_eq!(part.provider_data.as_ref().unwrap().data, reasoning_item);

        let ProviderEvent::OutputPart { part, .. } = &function_events[0] else {
            panic!("expected finalized function part")
        };
        assert!(matches!(
            part.content.as_ref(),
            Some(MessagePartContent::ToolCall { id, name, arguments })
                if id.to_string() == "call_1"
                    && name == "read"
                    && arguments["path"] == "README.md"
        ));
        assert_eq!(part.provider_data.as_ref().unwrap().data, function_item);
        assert!(saw_tool_call);
        Ok(())
    }

    #[test]
    fn custom_tool_streaming_events_accumulate_raw_input() -> Result<()> {
        let provider_id = ProviderId::new();
        let mut output_items = BTreeMap::new();
        let mut saw_tool_call = false;
        response_events(
            serde_json::json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "type": "custom_tool_call",
                    "id": "ctc_1",
                    "call_id": "call_shell",
                    "name": "shell",
                    "input": ""
                }
            }),
            provider_id,
            &mut output_items,
            &mut saw_tool_call,
        )?;
        let delta = response_events(
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "output_index": 0,
                "delta": "printf "
            }),
            provider_id,
            &mut output_items,
            &mut saw_tool_call,
        )?;
        assert!(matches!(
            delta.as_slice(),
            [ProviderEvent::CustomToolCallInputDelta {
                id: Some(id),
                name: Some(name),
                input,
                ..
            }] if id == "call_shell" && name == "shell" && input == "printf "
        ));
        let done = response_events(
            serde_json::json!({
                "type": "response.custom_tool_call_input.done",
                "output_index": 0,
                "input": "printf hello"
            }),
            provider_id,
            &mut output_items,
            &mut saw_tool_call,
        )?;
        assert!(matches!(
            done.as_slice(),
            [ProviderEvent::CustomToolCallInputDone { index: 0, input }] if input == "printf hello"
        ));
        let final_events = response_events(
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "custom_tool_call",
                    "id": "ctc_1",
                    "call_id": "call_shell",
                    "name": "shell",
                    "input": "printf hello"
                }
            }),
            provider_id,
            &mut output_items,
            &mut saw_tool_call,
        )?;
        let ProviderEvent::OutputPart { part, .. } = &final_events[0] else {
            panic!("expected finalized custom tool part")
        };
        assert!(matches!(
            part.content.as_ref(),
            Some(MessagePartContent::ToolCall { id, name, arguments })
                if id.to_string() == "call_shell"
                    && name == "shell"
                    && arguments == &Value::String("printf hello".into())
        ));
        assert!(saw_tool_call);
        Ok(())
    }

    #[test]
    fn custom_tool_calls_store_parsed_canonical_arguments() -> Result<()> {
        let provider_id = ProviderId::new();
        let shell = ToolDefinition {
            name: "shell".into(),
            description: "run a command".into(),
            input: ToolInputDefinition::new(serde_json::json!({ "type": "object" }))
                .with_freeform_parser(parse_shell_freeform),
        };
        let mut output_items = BTreeMap::new();
        let mut saw_tool_call = false;
        let events = response_events_with_tools(
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "custom_tool_call",
                    "call_id": "call_shell",
                    "name": "shell",
                    "input": "printf hello"
                }
            }),
            provider_id,
            &[shell],
            &mut output_items,
            &mut saw_tool_call,
        )?;
        let ProviderEvent::OutputPart { part, .. } = &events[0] else {
            panic!("expected finalized custom tool part")
        };
        assert!(matches!(
            part.content.as_ref(),
            Some(MessagePartContent::ToolCall { arguments, .. })
                if arguments == &serde_json::json!({ "command": "printf hello" })
        ));
        assert_eq!(
            part.provider_data.as_ref().unwrap().data["input"],
            "printf hello"
        );
        Ok(())
    }

    #[test]
    fn encrypted_only_and_unknown_items_become_provider_only_parts() -> Result<()> {
        let provider_id = ProviderId::new();
        for item in [
            serde_json::json!({ "type": "reasoning", "encrypted_content": "opaque" }),
            serde_json::json!({ "type": "unknown_item", "value": 1 }),
        ] {
            let mut output_items = BTreeMap::new();
            let mut saw_tool_call = false;
            let events = response_events(
                serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": item
                }),
                provider_id,
                &mut output_items,
                &mut saw_tool_call,
            )?;
            let ProviderEvent::OutputPart { part, .. } = &events[0] else {
                panic!("expected finalized part")
            };
            assert!(part.content.is_none());
            assert!(part.provider_data.is_some());
        }
        Ok(())
    }
}
