use std::{collections::BTreeMap, sync::Arc};

use super::{
    BeforeHookResult, Context, Error, Hook, HookContext, HookId, Message, Plugin, PluginId,
    PluginPriority, Provider, ProviderId, ProviderRequest, Result, RuntimeEvent, SessionStore,
    SessionStoreContext, SessionStoreFactory, SessionStoreFactoryId, StagedRegistrations, Tool,
    ToolExecutionContext, ToolId, ToolOutput, Workdir, WorkdirLayer, WorkdirLayerContext,
    WorkdirLayerId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Registration {
    pub plugin_id: PluginId,
    pub priority: PluginPriority,
    pub order: u64,
}

struct Entry<T: ?Sized> {
    registration: Registration,
    value: Arc<T>,
}

#[derive(Clone)]
pub struct ProviderRegistry(Arc<BTreeMap<ProviderId, Entry<dyn Provider>>>);

impl ProviderRegistry {
    pub fn get(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.0.get(id).map(|entry| entry.value.clone())
    }

    pub fn ids(&self) -> Vec<ProviderId> {
        self.0.keys().cloned().collect()
    }

    pub fn registration(&self, id: &ProviderId) -> Option<&Registration> {
        self.0.get(id).map(|entry| &entry.registration)
    }
}

#[derive(Clone)]
pub struct ToolRegistry(Arc<BTreeMap<ToolId, Entry<dyn Tool>>>);

impl ToolRegistry {
    pub fn get(&self, id: &ToolId) -> Option<Arc<dyn Tool>> {
        self.0.get(id).map(|entry| entry.value.clone())
    }

    pub fn ids(&self) -> Vec<ToolId> {
        self.0.keys().cloned().collect()
    }

    pub fn registration(&self, id: &ToolId) -> Option<&Registration> {
        self.0.get(id).map(|entry| &entry.registration)
    }

    pub(crate) fn all(&self) -> Vec<Arc<dyn Tool>> {
        let mut entries: Vec<_> = self.0.values().collect();
        sort_entries(&mut entries);
        entries
            .into_iter()
            .map(|entry| entry.value.clone())
            .collect()
    }

    pub(crate) fn owner(&self, id: &ToolId) -> Option<PluginId> {
        self.0
            .get(id)
            .map(|entry| entry.registration.plugin_id.clone())
    }
}

#[derive(Clone)]
pub struct PluginRegistry(Arc<BTreeMap<PluginId, Arc<dyn Plugin>>>);

impl PluginRegistry {
    pub fn get(&self, id: &PluginId) -> Option<Arc<dyn Plugin>> {
        self.0.get(id).cloned()
    }

    pub fn ids(&self) -> Vec<PluginId> {
        self.0.keys().cloned().collect()
    }
}

struct HookEntry<T: ?Sized> {
    id: HookId,
    registration: Registration,
    hook: Arc<T>,
}

struct StoreFactoryEntry {
    id: SessionStoreFactoryId,
    registration: Registration,
    factory: Arc<dyn SessionStoreFactory>,
}

struct WorkdirLayerEntry {
    id: WorkdirLayerId,
    registration: Registration,
    layer: Arc<dyn WorkdirLayer>,
}

#[derive(Clone)]
pub struct HookRegistry(Arc<HookRegistryInner>);

struct HookRegistryInner {
    before_user_message: Vec<HookEntry<dyn super::BeforeUserMessageHook>>,
    context_contribution: Vec<HookEntry<dyn super::ContextContributionHook>>,
    before_tool_execution: Vec<HookEntry<dyn super::BeforeToolExecutionHook>>,
    after_tool_execution: Vec<HookEntry<dyn super::AfterToolExecutionHook>>,
    before_provider_request: Vec<HookEntry<dyn super::BeforeProviderRequestHook>>,
    runtime_event: Vec<HookEntry<dyn super::RuntimeEventHook>>,
    store_factories: Vec<StoreFactoryEntry>,
}

impl HookRegistry {
    pub async fn before_user_message(
        &self,
        context: &HookContext,
        message: &mut Message,
    ) -> Result<BeforeHookResult> {
        for entry in &self.0.before_user_message {
            if let BeforeHookResult::Cancel { reason } =
                entry.hook.before_user_message(context, message).await?
            {
                return Ok(BeforeHookResult::Cancel { reason });
            }
        }
        Ok(BeforeHookResult::Continue)
    }

    pub async fn contribute_context(
        &self,
        hook_context: &HookContext,
        workdir: Arc<dyn Workdir>,
        context: &mut Context,
    ) -> Result<()> {
        for entry in &self.0.context_contribution {
            entry
                .hook
                .contribute_context(hook_context, workdir.clone(), context)
                .await?;
        }
        Ok(())
    }

    pub async fn before_tool_execution(
        &self,
        context: &mut ToolExecutionContext,
    ) -> Result<BeforeHookResult> {
        for entry in &self.0.before_tool_execution {
            let result = entry.hook.before_tool_execution(context).await?;
            if let BeforeHookResult::Cancel { reason } = result {
                return Ok(BeforeHookResult::Cancel { reason });
            }
        }
        Ok(BeforeHookResult::Continue)
    }

    pub async fn after_tool_execution(
        &self,
        context: &ToolExecutionContext,
        output: &mut ToolOutput,
    ) -> Result<()> {
        for entry in &self.0.after_tool_execution {
            entry.hook.after_tool_execution(context, output).await?;
        }
        Ok(())
    }

    pub async fn before_provider_request(
        &self,
        context: &HookContext,
        request: &mut ProviderRequest,
    ) -> Result<BeforeHookResult> {
        for entry in &self.0.before_provider_request {
            if let BeforeHookResult::Cancel { reason } =
                entry.hook.before_provider_request(context, request).await?
            {
                return Ok(BeforeHookResult::Cancel { reason });
            }
        }
        Ok(BeforeHookResult::Continue)
    }

    pub async fn runtime_event(&self, event: &RuntimeEvent) -> Vec<(PluginId, Error)> {
        let mut failures = Vec::new();
        for entry in &self.0.runtime_event {
            if let Err(error) = entry.hook.on_event(event).await {
                failures.push((entry.registration.plugin_id.clone(), error));
            }
        }
        failures
    }

    pub async fn open_session_store(
        &self,
        context: &SessionStoreContext,
    ) -> Result<Option<Arc<dyn SessionStore>>> {
        for entry in &self.0.store_factories {
            if let Some(store) = entry.factory.open(context).await? {
                return Ok(Some(store));
            }
        }
        Ok(None)
    }
}

#[derive(Clone)]
pub struct WorkdirLayerRegistry(Arc<Vec<WorkdirLayerEntry>>);

impl WorkdirLayerRegistry {
    pub fn apply(&self, context: &WorkdirLayerContext, host: Arc<dyn Workdir>) -> Arc<dyn Workdir> {
        self.0
            .iter()
            .rev()
            .fold(host, |inner, entry| entry.layer.layer(context, inner))
    }

    pub fn ids(&self) -> Vec<WorkdirLayerId> {
        self.0.iter().map(|entry| entry.id.clone()).collect()
    }

    pub fn registration(&self, id: &WorkdirLayerId) -> Option<&Registration> {
        self.0
            .iter()
            .find(|entry| &entry.id == id)
            .map(|entry| &entry.registration)
    }
}

pub(crate) struct RegistryBuilder {
    providers: BTreeMap<ProviderId, Entry<dyn Provider>>,
    tools: BTreeMap<ToolId, Entry<dyn Tool>>,
    plugins: BTreeMap<PluginId, Arc<dyn Plugin>>,
    hooks: HookRegistryInner,
    workdir_layers: Vec<WorkdirLayerEntry>,
    next_order: u64,
}

impl RegistryBuilder {
    pub(crate) fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
            tools: BTreeMap::new(),
            plugins: BTreeMap::new(),
            hooks: HookRegistryInner {
                before_user_message: Vec::new(),
                context_contribution: Vec::new(),
                before_tool_execution: Vec::new(),
                after_tool_execution: Vec::new(),
                before_provider_request: Vec::new(),
                runtime_event: Vec::new(),
                store_factories: Vec::new(),
            },
            workdir_layers: Vec::new(),
            next_order: 0,
        }
    }

    pub(crate) fn contains_plugin(&self, id: &PluginId) -> bool {
        self.plugins.contains_key(id)
    }

    pub(crate) fn commit(
        &mut self,
        plugin_id: PluginId,
        plugin: Arc<dyn Plugin>,
        staged: StagedRegistrations,
    ) -> Result<()> {
        if self.plugins.contains_key(&plugin_id) {
            return Err(Error::DuplicatePlugin(plugin_id));
        }
        for entry in &staged.providers {
            if self.providers.contains_key(&entry.id) {
                return Err(Error::DuplicateProvider(entry.id.clone()));
            }
        }
        for entry in &staged.tools {
            if self.tools.contains_key(&entry.id) {
                return Err(Error::DuplicateTool(entry.id.clone()));
            }
        }
        for entry in &staged.store_factories {
            if self
                .hooks
                .store_factories
                .iter()
                .any(|current| current.id == entry.id)
            {
                return Err(Error::DuplicateSessionStoreFactory(entry.id.clone()));
            }
        }
        for entry in &staged.workdir_layers {
            if self
                .workdir_layers
                .iter()
                .any(|current| current.id == entry.id)
            {
                return Err(Error::DuplicateWorkdirLayer(entry.id.clone()));
            }
        }
        for entry in &staged.hooks {
            let duplicate = match &entry.hook {
                Hook::BeforeUserMessage(_) => self
                    .hooks
                    .before_user_message
                    .iter()
                    .any(|x| x.id == entry.id),
                Hook::ContextContribution(_) => self
                    .hooks
                    .context_contribution
                    .iter()
                    .any(|x| x.id == entry.id),
                Hook::BeforeToolExecution(_) => self
                    .hooks
                    .before_tool_execution
                    .iter()
                    .any(|x| x.id == entry.id),
                Hook::AfterToolExecution(_) => self
                    .hooks
                    .after_tool_execution
                    .iter()
                    .any(|x| x.id == entry.id),
                Hook::BeforeProviderRequest(_) => self
                    .hooks
                    .before_provider_request
                    .iter()
                    .any(|x| x.id == entry.id),
                Hook::RuntimeEvent(_) => self.hooks.runtime_event.iter().any(|x| x.id == entry.id),
            };
            if duplicate {
                return Err(Error::DuplicateHook(entry.id.clone()));
            }
        }

        for entry in staged.providers {
            let registration = self.registration(&plugin_id, entry.priority);
            self.providers.insert(
                entry.id,
                Entry {
                    registration,
                    value: entry.provider,
                },
            );
        }
        for entry in staged.tools {
            let registration = self.registration(&plugin_id, entry.priority);
            self.tools.insert(
                entry.id,
                Entry {
                    registration,
                    value: entry.tool,
                },
            );
        }
        for entry in staged.hooks {
            let registration = self.registration(&plugin_id, entry.priority);
            match entry.hook {
                Hook::BeforeUserMessage(hook) => self.hooks.before_user_message.push(HookEntry {
                    id: entry.id,
                    registration,
                    hook,
                }),
                Hook::ContextContribution(hook) => {
                    self.hooks.context_contribution.push(HookEntry {
                        id: entry.id,
                        registration,
                        hook,
                    })
                }
                Hook::BeforeToolExecution(hook) => {
                    self.hooks.before_tool_execution.push(HookEntry {
                        id: entry.id,
                        registration,
                        hook,
                    })
                }
                Hook::AfterToolExecution(hook) => self.hooks.after_tool_execution.push(HookEntry {
                    id: entry.id,
                    registration,
                    hook,
                }),
                Hook::BeforeProviderRequest(hook) => {
                    self.hooks.before_provider_request.push(HookEntry {
                        id: entry.id,
                        registration,
                        hook,
                    })
                }
                Hook::RuntimeEvent(hook) => self.hooks.runtime_event.push(HookEntry {
                    id: entry.id,
                    registration,
                    hook,
                }),
            }
        }
        for entry in staged.store_factories {
            let registration = self.registration(&plugin_id, entry.priority);
            self.hooks.store_factories.push(StoreFactoryEntry {
                id: entry.id,
                registration,
                factory: entry.factory,
            });
        }
        for entry in staged.workdir_layers {
            let registration = self.registration(&plugin_id, entry.priority);
            self.workdir_layers.push(WorkdirLayerEntry {
                id: entry.id,
                registration,
                layer: entry.layer,
            });
        }
        self.plugins.insert(plugin_id, plugin);
        Ok(())
    }

    fn registration(&mut self, plugin_id: &PluginId, priority: PluginPriority) -> Registration {
        let order = self.next_order;
        self.next_order += 1;
        Registration {
            plugin_id: plugin_id.clone(),
            priority,
            order,
        }
    }

    pub(crate) fn freeze(
        mut self,
    ) -> (
        ProviderRegistry,
        ToolRegistry,
        PluginRegistry,
        HookRegistry,
        WorkdirLayerRegistry,
    ) {
        sort_hooks(&mut self.hooks.before_user_message);
        sort_hooks(&mut self.hooks.context_contribution);
        sort_hooks(&mut self.hooks.before_tool_execution);
        sort_hooks(&mut self.hooks.after_tool_execution);
        sort_hooks(&mut self.hooks.before_provider_request);
        sort_hooks(&mut self.hooks.runtime_event);
        self.hooks.store_factories.sort_by(registration_order);
        self.workdir_layers
            .sort_by(|left, right| registration_cmp(&left.registration, &right.registration));
        (
            ProviderRegistry(Arc::new(self.providers)),
            ToolRegistry(Arc::new(self.tools)),
            PluginRegistry(Arc::new(self.plugins)),
            HookRegistry(Arc::new(self.hooks)),
            WorkdirLayerRegistry(Arc::new(self.workdir_layers)),
        )
    }
}

fn sort_entries<T: ?Sized>(entries: &mut [&Entry<T>]) {
    entries.sort_by(|left, right| registration_cmp(&left.registration, &right.registration));
}

fn sort_hooks<T: ?Sized>(entries: &mut [HookEntry<T>]) {
    entries.sort_by(|left, right| registration_cmp(&left.registration, &right.registration));
}

fn registration_order(left: &StoreFactoryEntry, right: &StoreFactoryEntry) -> std::cmp::Ordering {
    registration_cmp(&left.registration, &right.registration)
}

fn registration_cmp(left: &Registration, right: &Registration) -> std::cmp::Ordering {
    right
        .priority
        .cmp(&left.priority)
        .then(left.order.cmp(&right.order))
}
