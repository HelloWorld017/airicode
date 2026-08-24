use std::{path::PathBuf, sync::Arc};

use serde_json::Value;

use super::{
    config::{aggregate, Config},
    error::Result,
    models::{Plugin, ProjectId, SessionGroupId, SessionId, SessionState},
    operations::{new_session, SessionHandle},
    registry::Registry,
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
        for plugin in self.plugins {
            let scope = registry.scope(plugin.id());
            schemas.push((plugin.name().to_string(), plugin.config_schema()));
            plugin.init(scope).await?;
        }
        let config = aggregate(self.raw_config, &schemas)?;
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
    pub fn create_session(&self, group_id: SessionGroupId) -> SessionHandle {
        new_session(SessionId::new(), group_id)
    }
    pub fn open_session(&self, state: SessionState) -> SessionHandle {
        SessionHandle::spawn(state)
    }
}

pub fn project_from_path(root: PathBuf) -> super::models::Project {
    super::models::Project {
        id: ProjectId::new(),
        name: root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_string(),
        root,
    }
}
