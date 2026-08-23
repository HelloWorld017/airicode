use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::{
    HookRegistry, Plugin, PluginId, PluginRegistrar, PluginRegistry, Project, ProviderRegistry,
    RegistryBuilder, Result, RuntimeEvent, ToolRegistry, Workdir, WorkdirLayerContext,
    WorkdirLayerRegistry,
};

struct CoreInner {
    providers: ProviderRegistry,
    tools: ToolRegistry,
    plugins: PluginRegistry,
    hooks: HookRegistry,
    workdir_layers: WorkdirLayerRegistry,
    events: broadcast::Sender<RuntimeEvent>,
    cancellation: CancellationToken,
}

#[derive(Clone)]
pub struct Core(Arc<CoreInner>);

pub struct CoreBuilder {
    plugins: Vec<Arc<dyn Plugin>>,
}

impl Core {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> CoreBuilder {
        CoreBuilder {
            plugins: Vec::new(),
        }
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.0.providers
    }
    pub fn tools(&self) -> &ToolRegistry {
        &self.0.tools
    }
    pub fn plugins(&self) -> &PluginRegistry {
        &self.0.plugins
    }
    pub fn hooks(&self) -> &HookRegistry {
        &self.0.hooks
    }
    pub fn workdir_layers(&self) -> &WorkdirLayerRegistry {
        &self.0.workdir_layers
    }
    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.0.events.subscribe()
    }
    pub fn shutdown(&self) {
        self.0.cancellation.cancel();
    }

    pub fn open_project(&self, name: impl Into<String>, workdir: Arc<dyn Workdir>) -> Project {
        let project_id = super::ProjectId::new();
        let name = name.into();
        let workdir = self.0.workdir_layers.apply(
            &WorkdirLayerContext {
                project_id,
                project_name: name.clone(),
            },
            workdir,
        );
        let project = Project::new(
            self.clone(),
            project_id,
            name,
            workdir,
            self.0.cancellation.child_token(),
        );
        let core = self.clone();
        let event = RuntimeEvent::ProjectOpened {
            project_id: project.id(),
        };
        let _ = self.0.events.send(event.clone());
        tokio::spawn(async move { core.dispatch_runtime_hooks(event).await });
        project
    }

    pub(crate) async fn emit(&self, event: RuntimeEvent) {
        let _ = self.0.events.send(event.clone());
        self.dispatch_runtime_hooks(event).await;
    }

    async fn dispatch_runtime_hooks(&self, event: RuntimeEvent) {
        for (plugin_id, error) in self.0.hooks.runtime_event(&event).await {
            let _ = self.0.events.send(RuntimeEvent::HookFailed {
                plugin_id,
                error: error.to_string(),
            });
        }
    }
}

impl CoreBuilder {
    pub fn with_plugin(mut self, plugin: Arc<dyn Plugin>) -> Self {
        self.plugins.push(plugin);
        self
    }

    pub async fn build(self) -> Result<Core> {
        let mut registries = RegistryBuilder::new();
        let mut plugin_ids = std::collections::BTreeSet::<PluginId>::new();
        for plugin in &self.plugins {
            let id = plugin.id();
            if !plugin_ids.insert(id.clone()) || registries.contains_plugin(&id) {
                return Err(super::Error::DuplicatePlugin(id));
            }
        }
        for plugin in self.plugins {
            let plugin_id = plugin.id();
            let registrar = PluginRegistrar::new(plugin_id.clone());
            plugin.clone().init(registrar.clone()).await?;
            registries.commit(plugin_id, plugin, registrar.take())?;
        }
        let (providers, tools, plugins, hooks, workdir_layers) = registries.freeze();
        let (events, _) = broadcast::channel(256);
        Ok(Core(Arc::new(CoreInner {
            providers,
            tools,
            plugins,
            hooks,
            workdir_layers,
            events,
            cancellation: CancellationToken::new(),
        })))
    }
}
