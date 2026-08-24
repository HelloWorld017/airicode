use std::sync::Arc;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::super::operations::Operations;
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
use super::id::{ProjectId, SessionGroupId, SessionId, TurnId};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
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
