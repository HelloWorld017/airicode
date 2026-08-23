use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{
    Message, ProjectId, ProviderId, ProviderRegistry, Result, SessionId, ToolId, TurnId, Workdir,
};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

pub struct ToolContext {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub workdir: std::sync::Arc<dyn Workdir>,
    pub cancellation: CancellationToken,
}

impl ToolContext {
    fn services(&self) -> &super::ToolServices {
        self.workdir
            .tool_services()
            .expect("enriched tool services are only available during core-dispatched execution")
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.services().provider_id
    }

    pub fn model(&self) -> &str {
        &self.services().model
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.services().providers
    }

    pub fn messages(&self) -> &[Message] {
        &self.services().messages
    }

    pub fn events(&self) -> Arc<dyn EventSink> {
        self.services().events.clone()
    }

    pub async fn emit_feature(
        &self,
        name: impl Into<String> + Send,
        payload: serde_json::Value,
    ) -> Result<()> {
        self.events().emit(name.into(), payload).await
    }
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, name: String, payload: serde_json::Value) -> Result<()>;
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> ToolId;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput>;
}
