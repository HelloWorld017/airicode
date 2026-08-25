use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::super::error::Result;
use super::super::operations::Operations;
use super::id::{ProjectId, SessionGroupId, SessionId, ToolId, TurnId};
use super::workdir::Workdir;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ToolOutput {
    Success { content: String },
    Failure { content: String },
    Stop,
}

impl ToolOutput {
    pub fn content(&self) -> Option<&str> {
        match self {
            Self::Success { content } | Self::Failure { content } => Some(content),
            Self::Stop => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum ToolInputDefinition {
    JsonSchema(Value),
    Text,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolInput {
    Json(Value),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input: ToolInputDefinition,
}

#[derive(Clone)]
pub struct ToolContext {
    pub project_id: ProjectId,
    pub session_group_id: SessionGroupId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub operations: Operations,
    pub workdir: Arc<dyn Workdir>,
    pub cancellation: CancellationToken,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> ToolId;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput>;
}
