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

pub use create_session::{create_session, new_session};

use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::error::{Error, Result};
use super::models::{
    RuntimeEvent, SessionCommit, SessionGroupId, SessionId, SessionMutation, SessionState,
};

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
        let (sender, receiver) = mpsc::channel(64);
        let (snapshot_tx, snapshot) = watch::channel(state.clone());
        let (events, _) = broadcast::channel(256);
        let operations = Operations {
            sender,
            session_id: state.session_id,
            group_id: state.group_id,
            events: events.clone(),
        };
        tokio::spawn(run_actor(state, receiver, snapshot_tx, events.clone()));
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
) {
    while let Some(request) = receiver.recv().await {
        match request {
            SessionRequest::Commit {
                mutations,
                response,
            } => {
                let commit = SessionCommit::new(state.last_sequence + 1, mutations);
                let result = state.apply(&commit);
                if result.is_ok() {
                    let _ = snapshot.send(state.clone());
                    let _ = events.send(RuntimeEvent::SessionSnapshotChanged);
                }
                let _ = response.send(result.map(|_| commit));
            }
            SessionRequest::Snapshot { response } => {
                let _ = response.send(state.clone());
            }
            SessionRequest::Shutdown => break,
        }
    }
}
