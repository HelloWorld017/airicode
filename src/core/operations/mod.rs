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

use super::models::{RuntimeEvent, SessionCommit, SessionMutation, SessionState};
use super::persistence::SessionStore;
use super::{
    error::{Error, Result},
    runtime::{SessionRuntime, SessionRuntimeDeps, TurnEngine},
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
    runtime: Weak<SessionRuntime>,
}

#[derive(Clone)]
pub struct SessionHandle {
    runtime: Arc<SessionRuntime>,
}

impl SessionHandle {
    pub fn spawn(state: SessionState, deps: SessionRuntimeDeps) -> Result<Self> {
        Ok(Self {
            runtime: SessionRuntime::spawn(state, deps)?,
        })
    }

    pub fn operations(&self) -> Operations {
        self.runtime.operations()
    }

    pub fn turn_engine(&self) -> TurnEngine {
        self.runtime.turn_engine()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.runtime.events().subscribe()
    }

    pub fn snapshot(&self) -> SessionState {
        self.runtime.snapshot().borrow().clone()
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.runtime
            .sender()
            .send(SessionRequest::Shutdown)
            .await
            .map_err(|_| Error::Session("session actor stopped".into()))?;
        Ok(())
    }
}

impl Operations {
    pub(crate) fn from_runtime(runtime: Weak<SessionRuntime>) -> Self {
        Self { runtime }
    }

    pub(crate) fn runtime(&self) -> Result<Arc<SessionRuntime>> {
        self.runtime
            .upgrade()
            .ok_or_else(|| Error::Session("session runtime is no longer available".into()))
    }

    pub(crate) fn workdir(&self) -> Result<Arc<dyn Workdir>> {
        Ok(self.runtime()?.workdir())
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
