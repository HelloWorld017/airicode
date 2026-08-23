use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    Context, Error, HookId, Message, PluginId, Provider, ProviderId, ProviderRequest, Result,
    RuntimeEvent, SessionStoreFactory, SessionStoreFactoryId, Tool, ToolCallId, ToolId, ToolOutput,
    TurnId, Workdir, WorkdirLayer, WorkdirLayerId,
};

pub type PluginPriority = i32;

#[derive(Clone, Debug)]
pub struct HookContext {
    pub project_id: super::ProjectId,
    pub session_id: super::SessionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BeforeHookResult {
    Continue,
    Cancel { reason: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCallDraft {
    pub id: ToolCallId,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Clone)]
pub struct ToolExecutionContext {
    pub hook_context: HookContext,
    pub turn_id: TurnId,
    pub workdir: Arc<dyn Workdir>,
    pub call: ToolCallDraft,
}

#[async_trait]
pub trait BeforeUserMessageHook: Send + Sync {
    async fn before_user_message(
        &self,
        context: &HookContext,
        message: &mut Message,
    ) -> Result<BeforeHookResult>;
}

#[async_trait]
pub trait ContextContributionHook: Send + Sync {
    async fn contribute_context(
        &self,
        hook_context: &HookContext,
        workdir: Arc<dyn Workdir>,
        context: &mut Context,
    ) -> Result<()>;
}

#[async_trait]
pub trait BeforeToolExecutionHook: Send + Sync {
    async fn before_tool_execution(
        &self,
        context: &mut ToolExecutionContext,
    ) -> Result<BeforeHookResult>;
}

#[async_trait]
pub trait AfterToolExecutionHook: Send + Sync {
    async fn after_tool_execution(
        &self,
        context: &ToolExecutionContext,
        output: &mut ToolOutput,
    ) -> Result<()>;
}

#[async_trait]
pub trait BeforeProviderRequestHook: Send + Sync {
    async fn before_provider_request(
        &self,
        context: &HookContext,
        request: &mut ProviderRequest,
    ) -> Result<BeforeHookResult>;
}

#[async_trait]
pub trait RuntimeEventHook: Send + Sync {
    async fn on_event(&self, event: &RuntimeEvent) -> Result<()>;
}

#[async_trait]
pub trait Plugin: Send + Sync {
    fn id(&self) -> PluginId;
    async fn init(self: Arc<Self>, registrar: PluginRegistrar) -> Result<()>;
}

#[derive(Clone)]
pub struct PluginRegistrar {
    pub(crate) plugin_id: PluginId,
    pub(crate) staged: Arc<Mutex<Option<StagedRegistrations>>>,
}

impl PluginRegistrar {
    pub(crate) fn new(plugin_id: PluginId) -> Self {
        Self {
            plugin_id,
            staged: Arc::new(Mutex::new(Some(StagedRegistrations::default()))),
        }
    }

    pub fn plugin_id(&self) -> &PluginId {
        &self.plugin_id
    }

    pub fn register_provider(
        &self,
        priority: PluginPriority,
        provider: Arc<dyn Provider>,
    ) -> Result<()> {
        let id = provider.id();
        let mut guard = self.staged.lock().expect("plugin registrar lock poisoned");
        let staged = guard.as_mut().ok_or(Error::PluginRegistrarClosed)?;
        if staged.providers.iter().any(|entry| entry.id == id) {
            return Err(Error::DuplicateProvider(id));
        }
        staged.providers.push(StagedProvider {
            id,
            priority,
            provider,
        });
        Ok(())
    }

    pub fn register_tool(&self, priority: PluginPriority, tool: Arc<dyn Tool>) -> Result<()> {
        let id = tool.id();
        let mut guard = self.staged.lock().expect("plugin registrar lock poisoned");
        let staged = guard.as_mut().ok_or(Error::PluginRegistrarClosed)?;
        if staged.tools.iter().any(|entry| entry.id == id) {
            return Err(Error::DuplicateTool(id));
        }
        staged.tools.push(StagedTool { id, priority, tool });
        Ok(())
    }

    pub fn register_before_user_message(
        &self,
        id: impl Into<HookId>,
        priority: PluginPriority,
        hook: Arc<dyn BeforeUserMessageHook>,
    ) -> Result<()> {
        self.stage_hook(id.into(), priority, Hook::BeforeUserMessage(hook))
    }

    pub fn register_context_contribution(
        &self,
        id: impl Into<HookId>,
        priority: PluginPriority,
        hook: Arc<dyn ContextContributionHook>,
    ) -> Result<()> {
        self.stage_hook(id.into(), priority, Hook::ContextContribution(hook))
    }

    pub fn register_before_tool_execution(
        &self,
        id: impl Into<HookId>,
        priority: PluginPriority,
        hook: Arc<dyn BeforeToolExecutionHook>,
    ) -> Result<()> {
        self.stage_hook(id.into(), priority, Hook::BeforeToolExecution(hook))
    }

    pub fn register_after_tool_execution(
        &self,
        id: impl Into<HookId>,
        priority: PluginPriority,
        hook: Arc<dyn AfterToolExecutionHook>,
    ) -> Result<()> {
        self.stage_hook(id.into(), priority, Hook::AfterToolExecution(hook))
    }

    pub fn register_before_provider_request(
        &self,
        id: impl Into<HookId>,
        priority: PluginPriority,
        hook: Arc<dyn BeforeProviderRequestHook>,
    ) -> Result<()> {
        self.stage_hook(id.into(), priority, Hook::BeforeProviderRequest(hook))
    }

    pub fn register_runtime_event(
        &self,
        id: impl Into<HookId>,
        priority: PluginPriority,
        hook: Arc<dyn RuntimeEventHook>,
    ) -> Result<()> {
        self.stage_hook(id.into(), priority, Hook::RuntimeEvent(hook))
    }

    pub fn register_session_store_factory(
        &self,
        priority: PluginPriority,
        factory: Arc<dyn SessionStoreFactory>,
    ) -> Result<()> {
        let id = factory.id();
        let mut guard = self.staged.lock().expect("plugin registrar lock poisoned");
        let staged = guard.as_mut().ok_or(Error::PluginRegistrarClosed)?;
        if staged.store_factories.iter().any(|entry| entry.id == id) {
            return Err(Error::DuplicateSessionStoreFactory(id));
        }
        staged.store_factories.push(StagedStoreFactory {
            id,
            priority,
            factory,
        });
        Ok(())
    }

    pub fn register_workdir_layer(
        &self,
        priority: PluginPriority,
        layer: Arc<dyn WorkdirLayer>,
    ) -> Result<()> {
        let id = layer.id();
        let mut guard = self.staged.lock().expect("plugin registrar lock poisoned");
        let staged = guard.as_mut().ok_or(Error::PluginRegistrarClosed)?;
        if staged.workdir_layers.iter().any(|entry| entry.id == id) {
            return Err(Error::DuplicateWorkdirLayer(id));
        }
        staged.workdir_layers.push(StagedWorkdirLayer {
            id,
            priority,
            layer,
        });
        Ok(())
    }

    fn stage_hook(&self, id: HookId, priority: PluginPriority, hook: Hook) -> Result<()> {
        let kind = hook.kind();
        let mut guard = self.staged.lock().expect("plugin registrar lock poisoned");
        let staged = guard.as_mut().ok_or(Error::PluginRegistrarClosed)?;
        if staged
            .hooks
            .iter()
            .any(|entry| entry.id == id && entry.hook.kind() == kind)
        {
            return Err(Error::DuplicateHook(id));
        }
        staged.hooks.push(StagedHook { id, priority, hook });
        Ok(())
    }

    pub(crate) fn take(self) -> StagedRegistrations {
        self.staged
            .lock()
            .expect("plugin registrar lock poisoned")
            .take()
            .expect("plugin registrar already closed")
    }
}

#[derive(Default)]
pub(crate) struct StagedRegistrations {
    pub(crate) providers: Vec<StagedProvider>,
    pub(crate) tools: Vec<StagedTool>,
    pub(crate) hooks: Vec<StagedHook>,
    pub(crate) store_factories: Vec<StagedStoreFactory>,
    pub(crate) workdir_layers: Vec<StagedWorkdirLayer>,
}

pub(crate) struct StagedProvider {
    pub(crate) id: ProviderId,
    pub(crate) priority: PluginPriority,
    pub(crate) provider: Arc<dyn Provider>,
}

pub(crate) struct StagedTool {
    pub(crate) id: ToolId,
    pub(crate) priority: PluginPriority,
    pub(crate) tool: Arc<dyn Tool>,
}

pub(crate) struct StagedHook {
    pub(crate) id: HookId,
    pub(crate) priority: PluginPriority,
    pub(crate) hook: Hook,
}

pub(crate) struct StagedStoreFactory {
    pub(crate) id: SessionStoreFactoryId,
    pub(crate) priority: PluginPriority,
    pub(crate) factory: Arc<dyn SessionStoreFactory>,
}

pub(crate) struct StagedWorkdirLayer {
    pub(crate) id: WorkdirLayerId,
    pub(crate) priority: PluginPriority,
    pub(crate) layer: Arc<dyn WorkdirLayer>,
}

pub(crate) enum Hook {
    BeforeUserMessage(Arc<dyn BeforeUserMessageHook>),
    ContextContribution(Arc<dyn ContextContributionHook>),
    BeforeToolExecution(Arc<dyn BeforeToolExecutionHook>),
    AfterToolExecution(Arc<dyn AfterToolExecutionHook>),
    BeforeProviderRequest(Arc<dyn BeforeProviderRequestHook>),
    RuntimeEvent(Arc<dyn RuntimeEventHook>),
}

impl Hook {
    fn kind(&self) -> u8 {
        match self {
            Self::BeforeUserMessage(_) => 0,
            Self::ContextContribution(_) => 1,
            Self::BeforeToolExecution(_) => 2,
            Self::AfterToolExecution(_) => 3,
            Self::BeforeProviderRequest(_) => 4,
            Self::RuntimeEvent(_) => 5,
        }
    }
}
