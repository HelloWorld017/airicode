use tokio::sync::oneshot;

use super::{Operations, SessionRequest};
use crate::core::error::{Error, Result};
use crate::core::models::{
    DurableUIState, RuntimeEvent, SessionCommit, SessionMutation, SessionState,
};

impl Operations {
    pub async fn snapshot(&self) -> Result<SessionState> {
        let (response, receiver) = oneshot::channel();
        self.sender
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
        let (response, receiver) = oneshot::channel();
        self.sender
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

    pub async fn update_ui_state(&self, state: DurableUIState) -> Result<SessionCommit> {
        self.commit(vec![SessionMutation::DurableUIStateUpdated { state }])
            .await
    }

    pub async fn update_plugin_state(
        &self,
        namespace: impl Into<String>,
        value: serde_json::Value,
    ) -> Result<SessionCommit> {
        let mut state = self.snapshot().await?.ui.durable;
        state.plugin_state.insert(namespace.into(), value);
        self.update_ui_state(state).await
    }

    pub(crate) async fn emit(&self, event: RuntimeEvent) -> Result<()> {
        let _ = self.events.send(event);
        Ok(())
    }
}
