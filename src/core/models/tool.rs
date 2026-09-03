use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
pub type ToolFreeformParser = fn(&str) -> Result<Value>;
pub type ToolInput = Value;

#[derive(Clone, Debug)]
pub struct ToolInputDefinition {
    pub schema: Value,
    pub freeform_parser: Option<ToolFreeformParser>,
}

impl PartialEq for ToolInputDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.freeform_parser.is_some() == other.freeform_parser.is_some()
    }
}

impl ToolInputDefinition {
    pub fn new(schema: Value) -> Self {
        Self {
            schema,
            freeform_parser: None,
        }
    }

    pub fn with_freeform_parser(mut self, parser: ToolFreeformParser) -> Self {
        self.freeform_parser = Some(parser);
        self
    }

    pub fn parse_freeform(&self, input: &str) -> Result<Value> {
        let parser = self.freeform_parser.ok_or_else(|| {
            super::super::error::Error::Tool("tool does not support freeform input".into())
        })?;
        parser(input)
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
