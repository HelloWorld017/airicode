use async_trait::async_trait;
use serde_json::Value;

use super::error::Result;
use super::models::{ToolContext, ToolDefinition, ToolId, ToolOutput};

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> ToolId;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput>;
}
