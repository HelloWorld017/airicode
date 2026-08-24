use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use tokio_util::sync::CancellationToken;

use super::{
    Core, Error, ProjectId, ProviderId, Result, RuntimeEvent, Session, SessionId, SessionSpawn,
    SessionStoreContext, Workdir,
};

#[derive(Clone, Debug)]
pub struct OpenSession {
    pub id: Option<SessionId>,
    pub provider: ProviderId,
    pub model: String,
}

struct ProjectInner {
    id: ProjectId,
    name: String,
    core: Core,
    host_workdir: Arc<dyn Workdir>,
    sessions: RwLock<BTreeMap<SessionId, Session>>,
    cancellation: CancellationToken,
}

#[derive(Clone)]
pub struct Project(Arc<ProjectInner>);

impl Project {
    pub(crate) fn new(
        core: Core,
        id: ProjectId,
        name: String,
        workdir: Arc<dyn Workdir>,
        cancellation: CancellationToken,
    ) -> Self {
        Self(Arc::new(ProjectInner {
            id,
            name,
            core,
            host_workdir: workdir,
            sessions: RwLock::new(BTreeMap::new()),
            cancellation,
        }))
    }

    pub fn id(&self) -> ProjectId {
        self.0.id
    }
    pub fn name(&self) -> &str {
        &self.0.name
    }
    pub fn get_workdir(&self) -> Arc<dyn Workdir> {
        self.0.core.workdir_layers().apply(
            &super::WorkdirLayerContext {
                project_id: self.id(),
                project_name: self.name().to_owned(),
                session_id: None,
            },
            self.0.host_workdir.clone(),
        )
    }
    pub fn get_session(&self, id: SessionId) -> Option<Session> {
        self.0
            .sessions
            .read()
            .expect("sessions lock poisoned")
            .get(&id)
            .cloned()
    }
    pub fn close(&self) {
        self.0.cancellation.cancel();
    }

    pub async fn open_session(&self, request: OpenSession) -> Result<Session> {
        let provider = self
            .0
            .core
            .providers()
            .get(&request.provider)
            .ok_or_else(|| Error::ProviderNotFound(request.provider.clone()))?;
        let id = request.id.unwrap_or_default();
        let workdir = self.0.core.workdir_layers().apply(
            &super::WorkdirLayerContext {
                project_id: self.id(),
                project_name: self.name().to_owned(),
                session_id: Some(id),
            },
            self.0.host_workdir.clone(),
        );
        let store = self
            .0
            .core
            .hooks()
            .open_session_store(&SessionStoreContext {
                project_id: self.id(),
                project_name: self.name().to_owned(),
                workdir: workdir.clone(),
                session: OpenSession {
                    id: Some(id),
                    ..request.clone()
                },
            })
            .await?;
        let session = Session::spawn(SessionSpawn {
            id,
            project_id: self.id(),
            provider,
            provider_id: request.provider,
            model: request.model,
            hooks: self.0.core.hooks().clone(),
            tools: self.0.core.tools().clone(),
            commands: self.0.core.commands().clone(),
            providers: self.0.core.providers().clone(),
            workdir,
            core: self.0.core.clone(),
            store,
            cancellation: self.0.cancellation.child_token(),
        })
        .await?;
        self.0
            .sessions
            .write()
            .expect("sessions lock poisoned")
            .insert(session.id(), session.clone());
        self.0
            .core
            .emit(RuntimeEvent::SessionOpened {
                session_id: session.id(),
            })
            .await;
        Ok(session)
    }
}
