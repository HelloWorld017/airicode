use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;

use super::{
    config::Config,
    error::Result,
    models::{
        ContextContribution, ContextPriority, FileContext, Message, PluginId, Project, ToolCallId,
        ToolOutput, TurnId,
    },
    registry::PluginRegistryScope,
};

#[derive(Clone)]
pub struct ConfigReadContext {
    pub config: Config,
    pub registry: PluginRegistryScope,
}

#[derive(Clone)]
pub struct OpenProjectContext {
    pub project: Project,
    pub registry: PluginRegistryScope,
}

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

#[derive(Clone)]
pub struct BuildFileContextHookContext {
    pub path: PathBuf,
    pub source: Arc<str>,
}

#[async_trait]
pub trait ConfigReadHook: Send + Sync {
    async fn config_read(&self, context: ConfigReadContext) -> Result<()>;
}

#[async_trait]
pub trait OpenProjectHook: Send + Sync {
    async fn open_project(&self, context: OpenProjectContext) -> Result<()>;
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

#[async_trait]
pub trait BuildFileContextHook: Send + Sync {
    async fn augment_file_context(
        &self,
        context: BuildFileContextHookContext,
        file_context: &mut FileContext,
    ) -> Result<()>;
}

#[derive(Default)]
pub struct HookRegistry {
    pub config_read: Vec<(PluginId, Arc<dyn ConfigReadHook>)>,
    pub open_project: Vec<(PluginId, Arc<dyn OpenProjectHook>)>,
    pub context: Vec<(PluginId, Arc<dyn ContextContributionHook>)>,
    pub before_message: Vec<(PluginId, Arc<dyn BeforeMessageHook>)>,
    pub before_provider_request: Vec<(PluginId, Arc<dyn BeforeProviderRequestHook>)>,
    pub before_tool: Vec<(PluginId, Arc<dyn BeforeToolExecutionHook>)>,
    pub after_tool: Vec<(PluginId, Arc<dyn AfterToolExecutionHook>)>,
    pub build_file_context: Vec<(PluginId, Arc<dyn BuildFileContextHook>)>,
}

pub trait RegisterHook {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId);
}

impl RegisterHook for Arc<dyn ContextContributionHook> {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId) {
        registry.context.push((owner, self));
    }
}

impl RegisterHook for Arc<dyn BeforeMessageHook> {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId) {
        registry.before_message.push((owner, self));
    }
}

impl RegisterHook for Arc<dyn BeforeProviderRequestHook> {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId) {
        registry.before_provider_request.push((owner, self));
    }
}

impl RegisterHook for Arc<dyn BeforeToolExecutionHook> {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId) {
        registry.before_tool.push((owner, self));
    }
}

impl RegisterHook for Arc<dyn AfterToolExecutionHook> {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId) {
        registry.after_tool.push((owner, self));
    }
}

impl RegisterHook for Arc<dyn BuildFileContextHook> {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId) {
        registry.build_file_context.push((owner, self));
    }
}

impl RegisterHook for Arc<dyn ConfigReadHook> {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId) {
        registry.config_read.push((owner, self));
    }
}

impl RegisterHook for Arc<dyn OpenProjectHook> {
    fn register_into(self, registry: &mut HookRegistry, owner: PluginId) {
        registry.open_project.push((owner, self));
    }
}

impl HookRegistry {
    pub fn register<H: RegisterHook>(&mut self, hook: H, owner: PluginId) {
        hook.register_into(self, owner);
    }

    pub async fn contributions(
        &self,
        context: ContextContributionContext,
    ) -> Result<Vec<ContextContribution>> {
        let mut result = Vec::new();
        for (_, hook) in self.context.clone() {
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
