use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};

use crate::core::{
    Error, FinishReason, Message, MessagePart, Model, Provider, ProviderEvent, ProviderId,
    ProviderRequest, ProviderStream, Result, Role, Usage,
};

pub(crate) struct OpenAiCompatible {
    id: ProviderId,
    base_url: String,
    api_key: String,
    headers: Vec<(&'static str, String)>,
    client: Client,
}

impl OpenAiCompatible {
    pub(crate) fn new(
        id: impl Into<ProviderId>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        headers: Vec<(&'static str, String)>,
    ) -> Self {
        Self::with_client(id, base_url, api_key, headers, Client::new())
    }

    fn with_client(
        id: impl Into<ProviderId>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        headers: Vec<(&'static str, String)>,
        client: Client,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            headers,
            client,
        }
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = if self.api_key.is_empty() {
            request
        } else {
            request.bearer_auth(&self.api_key)
        };
        self.headers.iter().fold(request, |request, (name, value)| {
            request.header(*name, value)
        })
    }

    #[cfg(test)]
    pub(crate) fn test_request(&self) -> reqwest::Request {
        self.authenticated(self.client.get(&self.base_url))
            .build()
            .expect("test request should be valid")
    }
}

#[async_trait]
impl Provider for OpenAiCompatible {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn get_models(&self) -> Result<Vec<Model>> {
        let response = self
            .authenticated(self.client.get(format!("{}/models", self.base_url)))
            .send()
            .await
            .map_err(provider_http_error)?;
        let response = checked_response(response).await?;
        let body: Value = response.json().await.map_err(provider_http_error)?;
        let models = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Provider("models response is missing data".into()))?
            .iter()
            .filter_map(|model| {
                let id = model.get("id")?.as_str()?.to_owned();
                let display_name = model
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned();
                Some(Model { id, display_name })
            })
            .collect();
        Ok(models)
    }

    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream> {
        let body = request_body(&request);
        let cancellation = request.cancellation.clone();
        let send = self
            .authenticated(
                self.client
                    .post(format!("{}/chat/completions", self.base_url))
                    .json(&body),
            )
            .send();
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(Error::Cancelled),
            response = send => response.map_err(provider_http_error)?,
        };
        let response = checked_response(response).await?;

        Ok(Box::pin(async_stream::stream! {
            let mut chunks = response.bytes_stream();
            let mut decoder = SseDecoder::default();
            loop {
                let chunk = tokio::select! {
                    _ = cancellation.cancelled() => {
                        yield Err(Error::Cancelled);
                        return;
                    }
                    chunk = chunks.next() => chunk,
                };
                let Some(chunk) = chunk else { break };
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield Err(provider_http_error(error));
                        return;
                    }
                };
                for data in match decoder.push(&chunk) {
                    Ok(data) => data,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                } {
                    match parse_data(&data) {
                        Ok(ParsedData::Done) => return,
                        Ok(ParsedData::Events(events)) => {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
            }
            for data in match decoder.finish() {
                Ok(data) => data,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            } {
                match parse_data(&data) {
                    Ok(ParsedData::Done) => return,
                    Ok(ParsedData::Events(events)) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }
            }
        }))
    }
}

fn request_body(request: &ProviderRequest) -> Value {
    let mut messages = Vec::new();
    if !request.context.parts().is_empty() {
        messages.push(json!({
            "role": "system",
            "content": request.context.parts().iter()
                .map(|part| part.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }));
    }
    for message in &request.messages {
        messages.extend(wire_messages(message));
    }

    let tools: Vec<_> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect();
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    body
}

fn wire_messages(message: &Message) -> Vec<Value> {
    if message.role == Role::Tool {
        return message
            .content
            .iter()
            .filter_map(|part| match part {
                MessagePart::ToolResult {
                    call_id, content, ..
                } => Some(json!({
                    "role": "tool",
                    "tool_call_id": call_id.as_str(),
                    "content": content,
                })),
                _ => None,
            })
            .collect();
    }

    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => unreachable!(),
    };
    let content = message
        .content
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let tool_calls: Vec<_> = message
        .content
        .iter()
        .filter_map(|part| match part {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => Some(json!({
                "id": id.as_str(),
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into()),
                }
            })),
            _ => None,
        })
        .collect();
    let mut value = json!({ "role": role, "content": content });
    if !tool_calls.is_empty() {
        value["tool_calls"] = Value::Array(tool_calls);
    }
    vec![value]
}

async fn checked_response(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    Err(Error::Provider(format_status_error(status, &body)))
}

fn format_status_error(status: StatusCode, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {body}")
    }
}

fn provider_http_error(error: reqwest::Error) -> Error {
    Error::Provider(error.to_string())
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.buffer.extend_from_slice(bytes);
        self.take_frames(false)
    }

    fn finish(&mut self) -> Result<Vec<String>> {
        self.take_frames(true)
    }

    fn take_frames(&mut self, flush: bool) -> Result<Vec<String>> {
        let mut frames = Vec::new();
        while let Some((position, delimiter_len)) = frame_boundary(&self.buffer) {
            let frame = self.buffer.drain(..position).collect::<Vec<_>>();
            self.buffer.drain(..delimiter_len);
            if let Some(data) = frame_data(&frame)? {
                frames.push(data);
            }
        }
        if flush && !self.buffer.is_empty() {
            let frame = std::mem::take(&mut self.buffer);
            if let Some(data) = frame_data(&frame)? {
                frames.push(data);
            }
        }
        Ok(frames)
    }
}

fn frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|p| (p, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|p| (p, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 < right.0 { left } else { right }),
        (Some(boundary), None) | (None, Some(boundary)) => Some(boundary),
        (None, None) => None,
    }
}

fn frame_data(frame: &[u8]) -> Result<Option<String>> {
    let frame = std::str::from_utf8(frame)
        .map_err(|error| Error::Provider(format!("SSE is not valid UTF-8: {error}")))?;
    let data = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    Ok((!data.is_empty()).then_some(data))
}

enum ParsedData {
    Done,
    Events(Vec<ProviderEvent>),
}

fn parse_data(data: &str) -> Result<ParsedData> {
    if data.trim() == "[DONE]" {
        return Ok(ParsedData::Done);
    }
    let chunk: Value = serde_json::from_str(data)
        .map_err(|error| Error::Provider(format!("invalid chat completion event: {error}")))?;
    if let Some(error) = chunk.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string());
        return Err(Error::Provider(message));
    }
    let mut events = Vec::new();
    if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
        events.push(ProviderEvent::Usage {
            usage: Usage {
                input_tokens: usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                output_tokens: usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                total_tokens: usage
                    .get("total_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
            },
        });
    }
    for choice in chunk
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            events.push(ProviderEvent::TextDelta { text: text.into() });
        }
        let reasoning = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str);
        if let Some(text) = reasoning {
            events.push(ProviderEvent::ReasoningDelta { text: text.into() });
        }
        for tool_call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let function = tool_call.get("function").unwrap_or(&Value::Null);
            events.push(ProviderEvent::ToolCallDelta {
                index: tool_call
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as u32,
                id: tool_call
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                name: function
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                arguments: function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            events.push(ProviderEvent::Finished {
                reason: match reason {
                    "stop" => FinishReason::Stop,
                    "length" => FinishReason::Length,
                    "tool_calls" | "function_call" => FinishReason::ToolCalls,
                    "content_filter" => FinishReason::ContentFilter,
                    other => FinishReason::Other(other.into()),
                },
            });
        }
    }
    Ok(ParsedData::Events(events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_fragmented_sse_and_done() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: {\"cho").unwrap().is_empty());
        let frames = decoder.push(b"ices\":[]}\r\n\r\ndata: [DONE]\n\n").unwrap();
        assert_eq!(frames, vec![r#"{"choices":[]}"#, "[DONE]"]);
        assert!(matches!(parse_data(&frames[1]).unwrap(), ParsedData::Done));
    }

    #[test]
    fn parses_text_reasoning_tool_usage_and_finish_events() {
        let data = r#"{
            "choices":[{"delta":{
                "content":"hi",
                "reasoning_content":"think",
                "tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\\\"pa"}}]
            },"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}
        }"#;
        let ParsedData::Events(events) = parse_data(data).unwrap() else {
            panic!("expected events");
        };
        assert!(matches!(events[0], ProviderEvent::Usage { .. }));
        assert_eq!(events[1], ProviderEvent::TextDelta { text: "hi".into() });
        assert_eq!(
            events[2],
            ProviderEvent::ReasoningDelta {
                text: "think".into()
            }
        );
        assert!(matches!(events[3], ProviderEvent::ToolCallDelta { .. }));
        assert_eq!(
            events[4],
            ProviderEvent::Finished {
                reason: FinishReason::ToolCalls
            }
        );
    }
}
