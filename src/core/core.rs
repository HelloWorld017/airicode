use std::{path::PathBuf, sync::Arc};

use serde_json::Value;
use tokio::sync::broadcast;

use super::{
    config::{Config, aggregate},
    error::{Error, Result},
    hooks::{ConfigReadContext, OpenProjectContext},
    models::{Plugin, Project, ProjectId, SessionGroupId, SessionId, SessionState, UIEvent},
    operations::SessionHandle,
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
        let (config, mut startup_diagnostics) = match aggregate(self.raw_config, &schemas) {
            Ok(config) => (config, Vec::new()),
            Err(error) => (
                aggregate(Value::Object(Default::default()), &schemas)?,
                vec![format!("configuration is invalid; using defaults: {error}")],
            ),
        };
        for (hook, registry_scope) in registry.config_read_hooks() {
            if let Err(error) = hook
                .config_read(ConfigReadContext {
                    config: config.clone(),
                    registry: registry_scope,
                })
                .await
            {
                startup_diagnostics.push(format!("configuration initialization failed: {error}"));
            }
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
        let (ui_events, _) = broadcast::channel(256);
        Ok(Core {
            inner: Arc::new(CoreInner {
                registry,
                config,
                project: self.project,
                ui_events,
                startup_diagnostics,
            }),
        })
    }
}

#[derive(Clone)]
pub struct Core {
    inner: Arc<CoreInner>,
}

struct CoreInner {
    registry: Registry,
    config: Config,
    project: Option<Project>,
    ui_events: broadcast::Sender<UIEvent>,
    startup_diagnostics: Vec<String>,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Core {
    pub fn new() -> Self {
        let (ui_events, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(CoreInner {
                registry: Registry::new(),
                config: Config::default(),
                project: None,
                ui_events,
                startup_diagnostics: Vec::new(),
            }),
        }
    }

    pub fn registry(&self) -> Registry {
        self.inner.registry.clone()
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn project(&self) -> Result<Project> {
        self.inner
            .project
            .clone()
            .ok_or_else(|| Error::Session("sessions require a project".into()))
    }

    pub fn shell_action_handler(&self) -> ShellActionHandler {
        ShellActionHandler::new(self.registry())
    }

    pub fn subscribe_ui_events(&self) -> broadcast::Receiver<UIEvent> {
        self.inner.ui_events.subscribe()
    }

    pub fn startup_diagnostics(&self) -> &[String] {
        &self.inner.startup_diagnostics
    }

    pub(crate) fn emit_ui_event(&self, event: UIEvent) -> Result<()> {
        let _ = self.inner.ui_events.send(event);
        Ok(())
    }

    pub fn create_session(&self, group_id: SessionGroupId) -> Result<SessionHandle> {
        self.open_session(SessionState::new(SessionId::new(group_id), group_id))
    }

    pub fn open_session(&self, state: SessionState) -> Result<SessionHandle> {
        SessionHandle::spawn(state, self.clone())
    }

    pub async fn load_session(
        &self,
        session_id: SessionId,
        group_id: SessionGroupId,
    ) -> Result<SessionHandle> {
        let state = SessionState::replay(
            session_id,
            group_id,
            match self.registry().session_store() {
                Some(store) => store.load(session_id).await?,
                None => Vec::new(),
            },
        )?;
        self.open_session(state)
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
