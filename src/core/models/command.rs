use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::super::error::Result;
use super::super::operations::Operations;
use super::id::CommandId;

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
    pub operations: Operations,
    pub cancellation: CancellationToken,
}

pub type CommandOutput = String;

#[async_trait]
pub trait Command: Send + Sync {
    fn id(&self) -> CommandId;
    fn definition(&self) -> CommandDefinition;
    async fn execute(&self, input: CommandInput, context: CommandContext) -> Result<String>;
    async fn complete(
        &self,
        _context: CommandContext,
        _argument_index: usize,
        _prefix: &str,
    ) -> Result<Vec<Completion>> {
        Ok(Vec::new())
    }
}
