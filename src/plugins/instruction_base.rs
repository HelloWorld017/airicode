use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::{
    error::Result,
    hooks::{ContextContributionContext, ContextContributionHook},
    models::{ContextContribution, ContextPriority, Plugin, PluginId},
    registry::PluginRegistryScope,
};

pub const DEFAULT_BASE_INSTRUCTION: &str = "You are AiriCode, a careful coding agent. Inspect the workspace before editing, use the provided tools, and report verification results clearly.";

pub struct InstructionBasePlugin {
    id: PluginId,
    instruction: Arc<str>,
}

impl InstructionBasePlugin {
    pub fn new() -> Self {
        Self {
            id: PluginId::new(),
            instruction: Arc::from(DEFAULT_BASE_INSTRUCTION),
        }
    }

    pub fn with_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Arc::from(instruction.into());
        self
    }
}

impl Default for InstructionBasePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ContextContributionHook for InstructionBasePlugin {
    async fn contribute(
        &self,
        _context: ContextContributionContext,
    ) -> Result<Vec<ContextContribution>> {
        Ok(vec![ContextContribution {
            priority: ContextPriority::Persistent,
            text: self.instruction.to_string(),
            metadata: Default::default(),
        }])
    }
}

#[async_trait]
impl Plugin for InstructionBasePlugin {
    fn id(&self) -> PluginId {
        self.id
    }

    fn name(&self) -> &str {
        "instruction_base"
    }

    fn config_schema(&self) -> Value {
        serde_json::json!({ "type": "object", "properties": { "instruction": { "type": "string" } } })
    }

    async fn init(self: Arc<Self>, registry: PluginRegistryScope) -> Result<()> {
        let hook: Arc<dyn ContextContributionHook> = self;
        registry.register_hook(hook)
    }
}
