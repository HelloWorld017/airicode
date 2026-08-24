use std::sync::{Arc, RwLock};

use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::{
    error::{Error, Result},
    models::{
        FinishReason, Message, MessagePart, Model, ModelCapabilities, Plugin, PluginId, Provider,
        ProviderEvent, ProviderId, ProviderRequest, ProviderStream, Role, Usage,
    },
    registry::PluginRegistryScope,
};

pub struct OpenAiProvider {
    id: ProviderId,
    api_key: Arc<RwLock<String>>,
    base_url: Arc<RwLock<String>>,
    client: Client,
}

impl OpenAiProvider {
    pub fn new(id: ProviderId, api_key: impl Into<String>) -> Self {
        Self {
            id,
            api_key: Arc::new(RwLock::new(api_key.into())),
            base_url: Arc::new(RwLock::new("https://openrouter.ai/v1".into())),
            client: Client::new(),
        }
    }

    pub fn from_env(id: ProviderId) -> Result<Self> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| Error::Config("OPENAI_API_KEY is not set".into()))?;
        Ok(Self::new(id, key))
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

#[derive(Deserialize)]
struct ChatChunk {
    choices: Vec<Choice>,
    usage: Option<UsageRecord>,
}

#[derive(Deserialize)]
struct Choice {
    delta: Option<Delta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ToolCallDeltaRecord>>,
}

#[derive(Deserialize)]
struct ToolCallDeltaRecord {
    index: u32,
    id: Option<String>,
    function: Option<FunctionDeltaRecord>,
}

#[derive(Deserialize)]
struct FunctionDeltaRecord {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct UsageRecord {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
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
        let body = json!({
            "model": request.model,
            "messages": request.messages.iter().map(|message| openai_message(message)).collect::<Vec<_>>(),
            "tools": request.tools.iter().map(|tool| json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })).collect::<Vec<_>>(),
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        let response = self
            .client
            .post(self.endpoint("/chat/completions"))
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
            let mut done = false;
            while !done {
                let chunk = tokio::select! {
                    _ = cancellation.cancelled() => Some(Err(Error::Cancelled)),
                    chunk = bytes.next() => chunk.map(|value| value.map_err(|error| Error::Provider(format!("OpenAI stream failed: {error}")))),
                };
                let Some(chunk) = chunk else {
                    break;
                };
                let chunk = chunk?;
                buffer.extend_from_slice(&chunk);
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
                    let chunk = serde_json::from_str::<ChatChunk>(data)
                        .map_err(|error| Error::Provider(format!("invalid OpenAI stream chunk: {error}")))?;
                    if let Some(usage) = chunk.usage {
                        yield ProviderEvent::Usage { usage: Usage {
                            input_tokens: usage.prompt_tokens,
                            output_tokens: usage.completion_tokens,
                            total_tokens: usage.total_tokens,
                        }};
                    }
                    for choice in chunk.choices {
                        if let Some(delta) = choice.delta {
                            if let Some(text) = delta.content.filter(|text| !text.is_empty()) {
                                yield ProviderEvent::TextDelta { text };
                            }
                            if let Some(text) = delta.reasoning_content.filter(|text| !text.is_empty()) {
                                yield ProviderEvent::ReasoningDelta { text };
                            }
                            for call in delta.tool_calls.unwrap_or_default() {
                                let function = call.function;
                                yield ProviderEvent::ToolCallDelta {
                                    index: call.index,
                                    id: call.id,
                                    name: function.as_ref().and_then(|value| value.name.clone()),
                                    arguments: function.and_then(|value| value.arguments).unwrap_or_default(),
                                };
                            }
                        }
                        if let Some(reason) = choice.finish_reason {
                            yield ProviderEvent::Finished { reason: finish_reason(&reason) };
                        }
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

fn openai_message(message: &Message) -> Value {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut tool_result = None;
    for part in &message.content {
        match part {
            MessagePart::Text { text: value } | MessagePart::Reasoning { text: value } => {
                text.push_str(value)
            }
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => tool_calls.push(json!({
                "id": id.to_string(),
                "type": "function",
                "function": { "name": name, "arguments": arguments.to_string() }
            })),
            MessagePart::ToolResult {
                call_id, result, ..
            } => {
                let content = result.content().unwrap_or("").to_string();
                tool_result = Some((call_id.to_string(), content));
            }
        }
    }
    if let Some((call_id, content)) = tool_result {
        json!({ "role": "tool", "tool_call_id": call_id, "content": content })
    } else {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut value = json!({ "role": role, "content": text });
        if !tool_calls.is_empty() {
            value["tool_calls"] = Value::Array(tool_calls);
        }
        value
    }
}

fn finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_string()),
    }
}

async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(Error::Provider(format!("OpenAI returned {status}: {body}")))
}

pub struct OpenAiProviderPlugin {
    id: PluginId,
    provider: Arc<OpenAiProvider>,
}

impl OpenAiProviderPlugin {
    pub fn new(provider: Arc<OpenAiProvider>) -> Self {
        Self {
            id: PluginId::new(),
            provider,
        }
    }

    pub fn provider(&self) -> Arc<OpenAiProvider> {
        self.provider.clone()
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
        registry
            .register_provider(self.provider.clone(), 0)
            .map(|_| ())
    }

    async fn configure(&self, config: &Value, _registry: PluginRegistryScope) -> Result<()> {
        if let Some(base_url) = config.get("base_url").and_then(Value::as_str) {
            self.provider.set_base_url(base_url.to_string());
        }
        let key_env = config
            .get("api_key_env")
            .and_then(Value::as_str)
            .unwrap_or("OPENAI_API_KEY");
        if let Ok(key) = std::env::var(key_env) {
            self.provider.set_api_key(key);
        }
        Ok(())
    }
}
