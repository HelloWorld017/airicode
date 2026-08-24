use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::super::operations::Operations;
use super::id::{ProjectId, SessionGroupId, SessionId};
use super::workdir::Workdir;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandDefinition {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandInput {
    pub arguments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub value: String,
    pub description: Option<String>,
}

#[derive(Clone)]
pub struct CommandContext {
    pub project_id: ProjectId,
    pub session_group_id: SessionGroupId,
    pub session_id: SessionId,
    pub operations: Operations,
    pub workdir: std::sync::Arc<dyn Workdir>,
    pub cancellation: CancellationToken,
}

pub type CommandOutput = String;
