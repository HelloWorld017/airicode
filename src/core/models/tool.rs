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
pub type ToolFreeformParser = fn(&str) -> Result<Value>;
pub type ToolInput = Value;

#[derive(Clone, Debug)]
pub struct ToolFreeformDefinition {
    pub description: String,
    pub parser: ToolFreeformParser,
}

#[derive(Clone, Debug)]
pub struct ToolInputDefinition {
    pub schema: Value,
    pub freeform: Option<ToolFreeformDefinition>,
}

impl PartialEq for ToolInputDefinition {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.freeform.as_ref().map(|freeform| &freeform.description)
                == other
                    .freeform
                    .as_ref()
                    .map(|freeform| &freeform.description)
    }
}

impl ToolInputDefinition {
    pub fn new(schema: Value) -> Self {
        Self {
            schema,
            freeform: None,
        }
    }

    pub fn with_freeform(
        mut self,
        description: impl Into<String>,
        parser: ToolFreeformParser,
    ) -> Self {
        self.freeform = Some(ToolFreeformDefinition {
            description: description.into(),
            parser,
        });
        self
    }

    pub fn parse_freeform(&self, input: &str) -> Result<Value> {
        let freeform = self.freeform.as_ref().ok_or_else(|| {
            super::super::error::Error::Tool("tool does not support freeform input".into())
        })?;
        (freeform.parser)(input)
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
