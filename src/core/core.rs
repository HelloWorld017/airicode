use std::{path::PathBuf, sync::Arc};

use serde_json::Value;

use super::{
    config::{aggregate, Config},
    error::Result,
    models::{Plugin, ProjectId, SessionGroupId, SessionId, SessionState},
    operations::{new_session, new_session_with_store, SessionHandle},
    registry::Registry,
    shell::ShellActionHandler,
};

pub struct CoreBuilder {
    plugins: Vec<Arc<dyn Plugin>>,
    raw_config: Value,
}

impl Default for CoreBuilder {
    fn default() -> Self {
        Self {
            plugins: Vec::new(),
            raw_config: Value::Object(Default::default()),
        }
    }
}

impl CoreBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn plugin(mut self, plugin: Arc<dyn Plugin>) -> Self {
        self.plugins.push(plugin);
        self
    }
    pub fn config(mut self, config: Value) -> Self {
        self.raw_config = config;
        self
    }
    pub async fn build(self) -> Result<Core> {
        let registry = Registry::new();
        let mut schemas = Vec::new();
        for plugin in &self.plugins {
            let scope = registry.scope(plugin.id());
            schemas.push((plugin.name().to_string(), plugin.config_schema()));
            plugin.clone().init(scope).await?;
        }
        let config = aggregate(self.raw_config, &schemas)?;
        for plugin in &self.plugins {
            let namespace = config
                .namespace(plugin.name())
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            plugin
                .configure(&namespace, registry.scope(plugin.id()))
                .await?;
        }
        Ok(Core { registry, config })
    }
}

#[derive(Clone)]
pub struct Core {
    pub registry: Registry,
    pub config: Config,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Core {
    pub fn new() -> Self {
        Self {
            registry: Registry::new(),
            config: Config::default(),
        }
    }
    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }
    pub fn shell_action_handler(&self) -> ShellActionHandler {
        ShellActionHandler::new(self.registry())
    }
    pub fn create_session(&self, group_id: SessionGroupId) -> SessionHandle {
        match self.registry.session_store() {
            Some(store) => new_session_with_store(SessionId::new(group_id), group_id, store),
            None => new_session(SessionId::new(group_id), group_id),
        }
    }
    pub fn open_session(&self, state: SessionState) -> SessionHandle {
        SessionHandle::spawn_with_store(state, self.registry.session_store())
    }

    pub async fn load_session(
        &self,
        session_id: SessionId,
        group_id: SessionGroupId,
    ) -> Result<SessionHandle> {
        let state = SessionState::replay(
            session_id,
            group_id,
            match self.registry.session_store() {
                Some(store) => store.load(session_id).await?,
                None => Vec::new(),
            },
        )?;
        Ok(self.open_session(state))
    }

    pub async fn open_or_create_session(
        &self,
        session_id: SessionId,
        group_id: SessionGroupId,
    ) -> Result<SessionHandle> {
        self.load_session(session_id, group_id).await
    }
}

pub fn project_from_path(root: PathBuf) -> super::models::Project {
    super::models::Project {
        id: ProjectId::from_workdir(&root),
        name: root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_string(),
        root,
    }
}
