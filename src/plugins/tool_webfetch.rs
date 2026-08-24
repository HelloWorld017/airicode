use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use schemars::JsonSchema;
use serde_json::Value;

use crate::core::{
    error::{Error, Result},
    models::{Plugin, PluginId, Tool, ToolContext, ToolDefinition, ToolId, ToolOutput},
    registry::PluginRegistryScope,
};

#[allow(dead_code)]
#[derive(JsonSchema)]
struct WebfetchInputSchema {
    url: String,
}

pub struct ToolWebfetch {
    id: ToolId,
    client: Client,
    max_bytes: usize,
    timeout: Duration,
}

impl ToolWebfetch {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            client: Client::new(),
            max_bytes: 256 * 1024,
            timeout: Duration::from_secs(20),
        }
    }
    pub fn with_limits(mut self, timeout: Duration, max_bytes: usize) -> Self {
        self.timeout = timeout;
        self.max_bytes = max_bytes;
        self
    }
}
impl Default for ToolWebfetch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ToolWebfetch {
    fn id(&self) -> ToolId {
        self.id
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "webfetch".into(),
            description: "Fetch an HTTP or HTTPS URL and return a normalized response containing the status and body text. Requests have a timeout and maximum response size, support cancellation, and do not modify the workdir. Non-success HTTP statuses and network/limit failures are returned as tool failures.".into(),
            input_schema: crate::utils::schema::json_schema::<WebfetchInputSchema>(),
        }
    }

    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput> {
        let url = input
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Tool("webfetch requires url".into()))?;
        let parsed = reqwest::Url::parse(url)
            .map_err(|error| Error::Tool(format!("invalid URL: {error}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Ok(ToolOutput::Failure {
                content: "webfetch only supports http and https".into(),
            });
        }
        let response = tokio::time::timeout(self.timeout, self.client.get(parsed).send())
            .await
            .map_err(|_| Error::Tool("webfetch timed out".into()))?
            .map_err(|error| Error::Tool(format!("webfetch request failed: {error}")))?;
        let status = response.status();
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = tokio::select! {
            _ = context.cancellation.cancelled() => return Err(Error::Cancelled),
            chunk = stream.next() => chunk,
        } {
            let chunk =
                chunk.map_err(|error| Error::Tool(format!("webfetch stream failed: {error}")))?;
            if bytes.len() + chunk.len() > self.max_bytes {
                return Ok(ToolOutput::Failure {
                    content: format!("webfetch response exceeds {} bytes", self.max_bytes),
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
        let content = format!("HTTP {status}\n{body}");
        if !status.is_success() {
            return Ok(ToolOutput::Failure { content });
        }
        Ok(ToolOutput::Success { content })
    }
}

pub struct ToolWebfetchPlugin {
    id: PluginId,
    tool: Arc<ToolWebfetch>,
}
impl ToolWebfetchPlugin {
    pub fn new(tool: Arc<ToolWebfetch>) -> Self {
        Self {
            id: PluginId::new(),
            tool,
        }
    }
}

#[async_trait]
impl Plugin for ToolWebfetchPlugin {
    fn id(&self) -> PluginId {
        self.id
    }
    fn name(&self) -> &str {
        "tool_webfetch"
    }
    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        registry.register_tool(self.tool.clone(), 0).map(|_| ())
    }
}
