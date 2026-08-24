use async_trait::async_trait;

use super::error::Result;
use super::models::{CommandContext, CommandDefinition, CommandId, CommandInput, Completion};

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
