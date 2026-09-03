use tokio::sync::oneshot;

use super::{Operations, SessionRequest};
use crate::core::error::{Error, Result};
use crate::core::models::{
    RuntimeEvent, SessionCommit, SessionMutation, SessionState, UIEvent, UIState,
};

impl Operations {
    pub async fn snapshot(&self) -> Result<SessionState> {
        let sender = self.host()?.sender();
        let (response, receiver) = oneshot::channel();
        sender
            .send(SessionRequest::Snapshot { response })
            .await
            .map_err(|_| Error::Session("session actor stopped".into()))?;
        receiver
            .await
            .map_err(|_| Error::Session("session actor stopped".into()))
    }

    pub async fn commit(&self, mutations: Vec<SessionMutation>) -> Result<SessionCommit> {
        if mutations.is_empty() {
            return Err(Error::InvalidState("empty session commit".into()));
        }
        let sender = self.host()?.sender();
        let (response, receiver) = oneshot::channel();
        sender
            .send(SessionRequest::Commit {
                mutations,
                response,
            })
            .await
            .map_err(|_| Error::Session("session actor stopped".into()))?;
        receiver
            .await
            .map_err(|_| Error::Session("session actor stopped".into()))?
    }

    pub async fn update_ui_state(&self, state: UIState) -> Result<SessionCommit> {
        self.commit(vec![SessionMutation::UIStateUpdated { state }])
            .await
    }

    pub fn emit_ui_event(&self, event: UIEvent) -> Result<()> {
        self.host()?.core().emit_ui_event(event)
    }

    pub(crate) async fn emit(&self, event: RuntimeEvent) -> Result<()> {
        let _ = self.host()?.events().send(event);
        Ok(())
    }
}
