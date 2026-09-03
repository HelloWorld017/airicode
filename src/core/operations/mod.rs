mod add_context_part;
mod add_message;
mod add_note;
mod build_file_context;
mod create_session;
mod get_context;
mod get_messages;
mod invalidate_context_part;
mod invalidate_message;
mod request;
mod update_note;

use std::sync::{Arc, Weak};

use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::persistence::SessionStore;
use super::{
    config::Config,
    models::{Project, RuntimeEvent, SessionCommit, SessionMutation, SessionState},
    registry::Registry,
};
use super::{
    error::{Error, Result},
    runtime::TurnEngine,
    session::SessionHost,
    workdir::Workdir,
};

pub(crate) enum SessionRequest {
    Commit {
        mutations: Vec<SessionMutation>,
        response: oneshot::Sender<Result<SessionCommit>>,
    },
    Snapshot {
        response: oneshot::Sender<SessionState>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct Operations {
    host: Weak<SessionHost>,
}

#[derive(Clone)]
pub struct SessionHandle {
    host: Arc<SessionHost>,
}

impl SessionHandle {
    pub fn spawn(state: SessionState, core: super::core::Core) -> Result<Self> {
        Ok(Self {
            host: SessionHost::spawn(state, core)?,
        })
    }

    pub fn operations(&self) -> Operations {
        self.host.operations()
    }

    pub fn turn_engine(&self) -> TurnEngine {
        self.host.turn_engine()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.host.events().subscribe()
    }

    pub fn snapshot(&self) -> SessionState {
        self.host.snapshot().borrow().clone()
    }

    pub(crate) fn core(&self) -> super::core::Core {
        self.host.core().clone()
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.host
            .sender()
            .send(SessionRequest::Shutdown)
            .await
            .map_err(|_| Error::Session("session actor stopped".into()))?;
        Ok(())
    }
}

impl Operations {
    pub(crate) fn from_host(host: Weak<SessionHost>) -> Self {
        Self { host }
    }

    pub(crate) fn host(&self) -> Result<Arc<SessionHost>> {
        self.host
            .upgrade()
            .ok_or_else(|| Error::Session("session host is no longer available".into()))
    }

    pub fn project(&self) -> Result<Project> {
        self.host()?.core().project()
    }

    pub fn config(&self) -> Result<Config> {
        Ok(self.host()?.core().config().clone())
    }

    pub(crate) fn registry(&self) -> Result<Registry> {
        Ok(self.host()?.core().registry())
    }

    pub(crate) fn workdir(&self) -> Result<Arc<dyn Workdir>> {
        Ok(self.host()?.workdir())
    }
}

pub(crate) async fn run_actor(
    mut state: SessionState,
    mut receiver: mpsc::Receiver<SessionRequest>,
    snapshot: watch::Sender<SessionState>,
    events: broadcast::Sender<RuntimeEvent>,
    store: Option<Arc<dyn SessionStore>>,
) {
    while let Some(request) = receiver.recv().await {
        match request {
            SessionRequest::Commit {
                mutations,
                response,
            } => {
                let commit = SessionCommit::new(state.last_sequence + 1, mutations);
                let mut next = state.clone();
                if let Err(error) = next.apply(&commit) {
                    let _ = response.send(Err(error));
                    continue;
                }
                if let Some(store) = &store {
                    if let Err(error) = store.append(state.session_id, &commit).await {
                        let _ = response.send(Err(error));
                        continue;
                    }
                }
                state = next;
                let _ = snapshot.send(state.clone());
                let _ = events.send(RuntimeEvent::SessionSnapshotChanged);
                let _ = response.send(Ok(commit));
            }
            SessionRequest::Snapshot { response } => {
                let _ = response.send(state.clone());
            }
            SessionRequest::Shutdown => break,
        }
    }
}
