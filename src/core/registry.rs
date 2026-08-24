use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use super::{
    error::{Error, Result},
    hooks::RegisterHook,
    models::{
        Command, CommandId, PluginId, Provider, ProviderId, RegistrationId, Tool, ToolId,
        WorkdirLayerId,
    },
    workdir::WorkdirLayer,
};

#[derive(Clone)]
pub struct Registry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    state: RwLock<RegistryState>,
    hooks: RwLock<super::hooks::HookRegistry>,
}

struct RegistryState {
    revision: u64,
    order: u64,
    providers: HashMap<ProviderId, Registered<dyn Provider>>,
    tools: HashMap<ToolId, Registered<dyn Tool>>,
    commands: HashMap<CommandId, Registered<dyn Command>>,
    workdir_layers: HashMap<WorkdirLayerId, Registered<dyn WorkdirLayer>>,
}

struct Registered<T: ?Sized> {
    value: Arc<T>,
    owner: PluginId,
    priority: i32,
    order: u64,
    registration_id: RegistrationId,
}

#[derive(Clone, Copy)]
enum RegistrationKind {
    Provider(ProviderId),
    Tool(ToolId),
    Command(CommandId),
    WorkdirLayer(WorkdirLayerId),
}

#[derive(Clone)]
pub struct RegistrationHandle {
    registry: Registry,
    kind: RegistrationKind,
    registration_id: RegistrationId,
}

impl RegistrationHandle {
    pub async fn remove(&self) -> Result<()> {
        self.registry.remove(self.kind, self.registration_id)
    }

    pub fn registration_id(&self) -> RegistrationId {
        self.registration_id
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: RwLock::new(RegistryState {
                    revision: 0,
                    order: 0,
                    providers: HashMap::new(),
                    tools: HashMap::new(),
                    commands: HashMap::new(),
                    workdir_layers: HashMap::new(),
                }),
                hooks: RwLock::new(super::hooks::HookRegistry::default()),
            }),
        }
    }

    pub fn revision(&self) -> u64 {
        self.inner.state.read().expect("registry poisoned").revision
    }

    pub fn scope(&self, owner: PluginId) -> PluginRegistryScope {
        PluginRegistryScope {
            registry: self.clone(),
            owner,
        }
    }

    pub fn provider(&self, id: ProviderId) -> Option<Arc<dyn Provider>> {
        self.inner
            .state
            .read()
            .ok()?
            .providers
            .get(&id)
            .map(|entry| entry.value.clone())
    }

    pub fn tool(&self, id: ToolId) -> Option<Arc<dyn Tool>> {
        self.inner
            .state
            .read()
            .ok()?
            .tools
            .get(&id)
            .map(|entry| entry.value.clone())
    }

    pub fn tool_by_name(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let tools = self
            .inner
            .state
            .read()
            .ok()?
            .tools
            .values()
            .map(|entry| entry.value.clone())
            .collect::<Vec<_>>();
        tools
            .into_iter()
            .find(|tool| tool.definition().name == name)
    }

    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut values = self
            .inner
            .state
            .read()
            .expect("registry poisoned")
            .tools
            .values()
            .map(|entry| Registered {
                value: entry.value.clone(),
                owner: entry.owner,
                priority: entry.priority,
                order: entry.order,
                registration_id: entry.registration_id,
            })
            .collect::<Vec<_>>();
        values.sort_by_key(|entry| (-entry.priority, entry.order));
        values.into_iter().map(|entry| entry.value).collect()
    }

    pub fn command(&self, id: CommandId) -> Option<Arc<dyn Command>> {
        self.inner
            .state
            .read()
            .ok()?
            .commands
            .get(&id)
            .map(|entry| entry.value.clone())
    }

    pub fn commands(&self) -> Vec<Arc<dyn Command>> {
        let mut values = self
            .inner
            .state
            .read()
            .expect("registry poisoned")
            .commands
            .values()
            .map(|entry| Registered {
                value: entry.value.clone(),
                owner: entry.owner,
                priority: entry.priority,
                order: entry.order,
                registration_id: entry.registration_id,
            })
            .collect::<Vec<_>>();
        values.sort_by_key(|entry| (-entry.priority, entry.order));
        values.into_iter().map(|entry| entry.value).collect()
    }

    pub fn workdir_layers(&self) -> Vec<Arc<dyn WorkdirLayer>> {
        let mut values = self
            .inner
            .state
            .read()
            .expect("registry poisoned")
            .workdir_layers
            .values()
            .map(|entry| {
                (
                    entry.value.clone(),
                    entry.value.phase(),
                    entry.priority,
                    entry.order,
                )
            })
            .collect::<Vec<_>>();
        values.sort_by_key(|(_, phase, priority, order)| (*phase, -*priority, *order));
        values.into_iter().map(|(value, _, _, _)| value).collect()
    }

    pub fn layer_workdir(
        &self,
        context: &super::models::WorkdirLayerContext,
        base: Arc<dyn super::workdir::Workdir>,
    ) -> Arc<dyn super::workdir::Workdir> {
        self.workdir_layers()
            .into_iter()
            .fold(base, |inner, layer| layer.layer(context, inner))
    }

    pub(crate) fn hooks(&self) -> Arc<super::hooks::HookRegistry> {
        let hooks = self.inner.hooks.read().expect("hooks poisoned");
        Arc::new(super::hooks::HookRegistry {
            context: hooks.context.clone(),
            before_message: hooks.before_message.clone(),
            before_provider_request: hooks.before_provider_request.clone(),
            before_tool: hooks.before_tool.clone(),
            after_tool: hooks.after_tool.clone(),
        })
    }

    fn remove(&self, kind: RegistrationKind, registration_id: RegistrationId) -> Result<()> {
        let mut state = self
            .inner
            .state
            .write()
            .map_err(|_| Error::Registry("registry poisoned".into()))?;
        let removed = match kind {
            RegistrationKind::Provider(id) => {
                state
                    .providers
                    .get(&id)
                    .is_some_and(|entry| entry.registration_id == registration_id)
                    && state.providers.remove(&id).is_some()
            }
            RegistrationKind::Tool(id) => {
                state
                    .tools
                    .get(&id)
                    .is_some_and(|entry| entry.registration_id == registration_id)
                    && state.tools.remove(&id).is_some()
            }
            RegistrationKind::Command(id) => {
                state
                    .commands
                    .get(&id)
                    .is_some_and(|entry| entry.registration_id == registration_id)
                    && state.commands.remove(&id).is_some()
            }
            RegistrationKind::WorkdirLayer(id) => {
                state
                    .workdir_layers
                    .get(&id)
                    .is_some_and(|entry| entry.registration_id == registration_id)
                    && state.workdir_layers.remove(&id).is_some()
            }
        };
        if !removed {
            return Err(Error::Registry(
                "registration is already removed or replaced".into(),
            ));
        }
        state.revision += 1;
        Ok(())
    }

    fn remove_owner(&self, owner: PluginId) -> Result<()> {
        let mut state = self
            .inner
            .state
            .write()
            .map_err(|_| Error::Registry("registry poisoned".into()))?;
        state.providers.retain(|_, value| value.owner != owner);
        state.tools.retain(|_, value| value.owner != owner);
        state.commands.retain(|_, value| value.owner != owner);
        state.workdir_layers.retain(|_, value| value.owner != owner);
        state.revision += 1;
        Ok(())
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct PluginRegistryScope {
    registry: Registry,
    owner: PluginId,
}

impl PluginRegistryScope {
    pub fn register_hook<H>(&self, hook: H) -> Result<()>
    where
        H: RegisterHook,
    {
        self.registry
            .inner
            .hooks
            .write()
            .map_err(|_| Error::Registry("hooks poisoned".into()))?
            .register(hook);
        Ok(())
    }

    pub fn register_provider(
        &self,
        provider: Arc<dyn Provider>,
        priority: i32,
    ) -> Result<RegistrationHandle> {
        let id = provider.id();
        let mut state = self
            .registry
            .inner
            .state
            .write()
            .map_err(|_| Error::Registry("registry poisoned".into()))?;
        if state.providers.contains_key(&id) {
            return Err(Error::Registry(format!("duplicate provider {id}")));
        }
        state.order += 1;
        let registration_id = RegistrationId::new();
        let order = state.order;
        state.providers.insert(
            id,
            Registered {
                value: provider,
                owner: self.owner,
                priority,
                order,
                registration_id,
            },
        );
        state.revision += 1;
        Ok(RegistrationHandle {
            registry: self.registry.clone(),
            kind: RegistrationKind::Provider(id),
            registration_id,
        })
    }

    pub fn register_tool(&self, tool: Arc<dyn Tool>, priority: i32) -> Result<RegistrationHandle> {
        let id = tool.id();
        let mut state = self
            .registry
            .inner
            .state
            .write()
            .map_err(|_| Error::Registry("registry poisoned".into()))?;
        if state.tools.contains_key(&id) {
            return Err(Error::Registry(format!("duplicate tool {id}")));
        }
        state.order += 1;
        let registration_id = RegistrationId::new();
        let order = state.order;
        state.tools.insert(
            id,
            Registered {
                value: tool,
                owner: self.owner,
                priority,
                order,
                registration_id,
            },
        );
        state.revision += 1;
        Ok(RegistrationHandle {
            registry: self.registry.clone(),
            kind: RegistrationKind::Tool(id),
            registration_id,
        })
    }

    pub fn register_command(
        &self,
        command: Arc<dyn Command>,
        priority: i32,
    ) -> Result<RegistrationHandle> {
        let id = command.id();
        let mut state = self
            .registry
            .inner
            .state
            .write()
            .map_err(|_| Error::Registry("registry poisoned".into()))?;
        if state.commands.contains_key(&id) {
            return Err(Error::Registry(format!("duplicate command {id}")));
        }
        state.order += 1;
        let registration_id = RegistrationId::new();
        let order = state.order;
        state.commands.insert(
            id,
            Registered {
                value: command,
                owner: self.owner,
                priority,
                order,
                registration_id,
            },
        );
        state.revision += 1;
        Ok(RegistrationHandle {
            registry: self.registry.clone(),
            kind: RegistrationKind::Command(id),
            registration_id,
        })
    }

    pub fn register_workdir_layer(
        &self,
        layer: Arc<dyn WorkdirLayer>,
        priority: i32,
    ) -> Result<RegistrationHandle> {
        let id = layer.id();
        let mut state = self
            .registry
            .inner
            .state
            .write()
            .map_err(|_| Error::Registry("registry poisoned".into()))?;
        if state.workdir_layers.contains_key(&id) {
            return Err(Error::Registry(format!("duplicate workdir layer {id}")));
        }
        state.order += 1;
        let registration_id = RegistrationId::new();
        let order = state.order;
        state.workdir_layers.insert(
            id,
            Registered {
                value: layer,
                owner: self.owner,
                priority,
                order,
                registration_id,
            },
        );
        state.revision += 1;
        Ok(RegistrationHandle {
            registry: self.registry.clone(),
            kind: RegistrationKind::WorkdirLayer(id),
            registration_id,
        })
    }

    pub async fn unregister_all(&self) -> Result<()> {
        self.registry.remove_owner(self.owner)
    }
}
