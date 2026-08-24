use std::sync::Arc;

use async_trait::async_trait;

use super::{
    error::Result,
    models::{ContextContribution, ContextPriority, Message, ToolCallId, ToolOutput, TurnId},
};

#[derive(Clone)]
pub struct ContextContributionContext {
    pub turn_id: TurnId,
    pub messages: Vec<Arc<Message>>,
}

#[derive(Clone)]
pub struct BeforeMessageContext {
    pub turn_id: TurnId,
    pub message: Arc<Message>,
}

#[derive(Clone)]
pub struct BeforeProviderRequestContext {
    pub turn_id: TurnId,
    pub model: String,
}

#[derive(Clone)]
pub struct BeforeToolExecutionContext {
    pub turn_id: TurnId,
    pub call_id: ToolCallId,
    pub tool_name: String,
}

#[derive(Clone)]
pub struct AfterToolExecutionContext {
    pub turn_id: TurnId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub output: ToolOutput,
}

#[async_trait]
pub trait ContextContributionHook: Send + Sync {
    async fn contribute(
        &self,
        context: ContextContributionContext,
    ) -> Result<Vec<ContextContribution>>;
}

#[async_trait]
pub trait BeforeMessageHook: Send + Sync {
    async fn before_message(&self, context: BeforeMessageContext) -> Result<()>;
}

#[async_trait]
pub trait BeforeProviderRequestHook: Send + Sync {
    async fn before_provider_request(&self, context: BeforeProviderRequestContext) -> Result<()>;
}

#[async_trait]
pub trait BeforeToolExecutionHook: Send + Sync {
    async fn before_tool_execution(&self, context: BeforeToolExecutionContext) -> Result<()>;
}

#[async_trait]
pub trait AfterToolExecutionHook: Send + Sync {
    async fn after_tool_execution(&self, context: AfterToolExecutionContext) -> Result<()>;
}

#[derive(Default)]
pub struct HookRegistry {
    pub context: Vec<Arc<dyn ContextContributionHook>>,
    pub before_message: Vec<Arc<dyn BeforeMessageHook>>,
    pub before_provider_request: Vec<Arc<dyn BeforeProviderRequestHook>>,
    pub before_tool: Vec<Arc<dyn BeforeToolExecutionHook>>,
    pub after_tool: Vec<Arc<dyn AfterToolExecutionHook>>,
}

pub trait RegisterHook {
    fn register_into(self, registry: &mut HookRegistry);
}

impl RegisterHook for Arc<dyn ContextContributionHook> {
    fn register_into(self, registry: &mut HookRegistry) {
        registry.context.push(self);
    }
}

impl RegisterHook for Arc<dyn BeforeMessageHook> {
    fn register_into(self, registry: &mut HookRegistry) {
        registry.before_message.push(self);
    }
}

impl RegisterHook for Arc<dyn BeforeProviderRequestHook> {
    fn register_into(self, registry: &mut HookRegistry) {
        registry.before_provider_request.push(self);
    }
}

impl RegisterHook for Arc<dyn BeforeToolExecutionHook> {
    fn register_into(self, registry: &mut HookRegistry) {
        registry.before_tool.push(self);
    }
}

impl RegisterHook for Arc<dyn AfterToolExecutionHook> {
    fn register_into(self, registry: &mut HookRegistry) {
        registry.after_tool.push(self);
    }
}

impl HookRegistry {
    pub(crate) fn register<H: RegisterHook>(&mut self, hook: H) {
        hook.register_into(self);
    }

    pub async fn contributions(
        &self,
        context: ContextContributionContext,
    ) -> Result<Vec<ContextContribution>> {
        let mut result = Vec::new();
        for hook in self.context.clone() {
            result.extend(hook.contribute(context.clone()).await?);
        }
        result.sort_by_key(|item| match item.priority {
            ContextPriority::Persistent => 0,
            ContextPriority::High => 1,
            ContextPriority::Low => 2,
        });
        Ok(result)
    }
}
