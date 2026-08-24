use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::{
    error::Result,
    hooks::{ContextContributionContext, ContextContributionHook},
    models::{
        ContextContribution, ContextContributionPosition, ContextPriority, Plugin, PluginId,
        DEFAULT_MODE,
    },
    registry::PluginRegistryScope,
};

pub const DEFAULT_BASE_INSTRUCTION: &str = include_str!("../prompts/system.txt");
const BUILD_INSTRUCTION: &str = include_str!("../prompts/mode_build.txt");
const PLAN_INSTRUCTION: &str = include_str!("../prompts/mode_plan.txt");

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
        context: ContextContributionContext,
    ) -> Result<Vec<ContextContribution>> {
        let mut contributions = vec![ContextContribution {
            priority: ContextPriority::Persistent,
            position: ContextContributionPosition::Start,
            text: self.instruction.to_string(),
            metadata: Default::default(),
        }];
        let mode = context
            .messages
            .iter()
            .max_by_key(|message| message.created_at)
            .map(|message| message.mode.as_str())
            .unwrap_or(DEFAULT_MODE);
        if let Some(instruction) = mode_instruction(mode) {
            contributions.push(ContextContribution {
                priority: ContextPriority::Persistent,
                position: ContextContributionPosition::End,
                text: instruction.to_string(),
                metadata: Default::default(),
            });
        }
        Ok(contributions)
    }
}

fn mode_instruction(mode: &str) -> Option<&'static str> {
    match mode {
        "build" => Some(BUILD_INSTRUCTION),
        "plan" => Some(PLAN_INSTRUCTION),
        _ => None,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::{
        hooks::ContextContributionContext,
        models::{Message, Role, TurnId},
    };

    #[tokio::test]
    async fn loads_system_and_plan_prompts_at_their_positions() {
        let plugin = InstructionBasePlugin::new();
        let message = Message::text(Role::User, "plan this", "plan", None);
        let contributions = plugin
            .contribute(ContextContributionContext {
                turn_id: TurnId::new(),
                messages: vec![Arc::new(message)],
            })
            .await
            .expect("instruction contribution should succeed");

        assert_eq!(contributions.len(), 2);
        assert_eq!(
            contributions[0].position,
            ContextContributionPosition::Start
        );
        assert_eq!(contributions[0].text, include_str!("../prompts/system.txt"));
        assert_eq!(contributions[1].position, ContextContributionPosition::End);
        assert_eq!(
            contributions[1].text,
            include_str!("../prompts/mode_plan.txt")
        );
    }
}
