use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::super::error::Result;
use super::super::operations::Operations;
use super::id::{ToolId, TurnId};

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
pub type ToolInput = Value;

#[derive(Clone, Debug, PartialEq)]
pub struct ToolInputDefinition {
    pub schema: Value,
}

impl ToolInputDefinition {
    pub fn new(schema: Value) -> Self {
        Self { schema }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input: ToolInputDefinition,
}

#[derive(Clone)]
pub struct ToolContext {
    pub turn_id: TurnId,
    pub operations: Operations,
    pub cancellation: CancellationToken,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> ToolId;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: ToolInput, context: ToolContext) -> Result<ToolOutput>;
}
