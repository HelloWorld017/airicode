mod add_context_part;
mod add_message;
mod add_note;
mod create_session;
mod get_context;
mod get_messages;
mod invalidate_context_part;
mod invalidate_message;
mod request;
mod update_note;

pub use create_session::{create_session, new_session, new_session_with_store};

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::error::{Error, Result};
use super::models::{
    RuntimeEvent, SessionCommit, SessionGroupId, SessionId, SessionMutation, SessionState,
};
use super::persistence::SessionStore;

enum SessionRequest {
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
    sender: mpsc::Sender<SessionRequest>,
    session_id: SessionId,
    group_id: SessionGroupId,
    events: broadcast::Sender<RuntimeEvent>,
}

pub struct SessionHandle {
    pub operations: Operations,
    snapshot: watch::Receiver<SessionState>,
    events: broadcast::Sender<RuntimeEvent>,
}

impl SessionHandle {
    pub fn spawn(state: SessionState) -> Self {
        Self::spawn_with_store(state, None)
    }

    pub fn spawn_with_store(state: SessionState, store: Option<Arc<dyn SessionStore>>) -> Self {
        let (sender, receiver) = mpsc::channel(64);
        let (snapshot_tx, snapshot) = watch::channel(state.clone());
        let (events, _) = broadcast::channel(256);
        let operations = Operations {
            sender,
            session_id: state.session_id,
            group_id: state.group_id,
            events: events.clone(),
        };
        tokio::spawn(run_actor(
            state,
            receiver,
            snapshot_tx,
            events.clone(),
            store,
        ));
        Self {
            operations,
            snapshot,
            events,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.events.subscribe()
    }

    pub fn snapshot(&self) -> SessionState {
        self.snapshot.borrow().clone()
    }

    pub async fn wait_snapshot(&mut self) -> Result<SessionState> {
        self.snapshot
            .changed()
            .await
            .map_err(|_| Error::Session("session actor stopped".into()))?;
        Ok(self.snapshot())
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.operations
            .sender
            .send(SessionRequest::Shutdown)
            .await
            .map_err(|_| Error::Session("session actor stopped".into()))?;
        Ok(())
    }
}

async fn run_actor(
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
