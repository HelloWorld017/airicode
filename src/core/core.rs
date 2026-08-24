use std::{path::PathBuf, sync::Arc};

use serde_json::Value;

use super::{
    config::{aggregate, Config},
    error::Result,
    hooks::{ConfigReadContext, OpenProjectContext},
    models::{Plugin, Project, ProjectId, SessionGroupId, SessionId, SessionState},
    operations::{new_session, new_session_with_store, SessionHandle},
    registry::Registry,
    shell::ShellActionHandler,
};

pub struct CoreBuilder {
    plugins: Vec<Arc<dyn Plugin>>,
    project: Option<Project>,
    raw_config: Value,
}

impl Default for CoreBuilder {
    fn default() -> Self {
        Self {
            plugins: Vec::new(),
            project: None,
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
    pub fn project(mut self, project: Project) -> Self {
        self.project = Some(project);
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
        for (hook, registry_scope) in registry.config_read_hooks() {
            hook.config_read(ConfigReadContext {
                config: config.clone(),
                registry: registry_scope,
            })
            .await?;
        }
        if let Some(project) = self.project.clone() {
            for (hook, registry_scope) in registry.open_project_hooks() {
                hook.open_project(OpenProjectContext {
                    project: project.clone(),
                    registry: registry_scope,
                })
                .await?;
            }
        }
        Ok(Core {
            registry,
            config,
            project: self.project,
        })
    }
}

#[derive(Clone)]
pub struct Core {
    pub registry: Registry,
    pub config: Config,
    pub project: Option<Project>,
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
            project: None,
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
