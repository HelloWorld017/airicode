use super::{Operations, SessionHandle};
use crate::core::models::{SessionGroupId, SessionId, SessionState};
use crate::core::persistence::SessionStore;
use std::sync::Arc;

pub fn create_session(session_id: SessionId, group_id: SessionGroupId) -> SessionHandle {
    SessionHandle::spawn(SessionState::new(session_id, group_id))
}

pub fn new_session(session_id: SessionId, group_id: SessionGroupId) -> SessionHandle {
    create_session(session_id, group_id)
}

pub fn new_session_with_store(
    session_id: SessionId,
    group_id: SessionGroupId,
    store: Arc<dyn SessionStore>,
) -> SessionHandle {
    SessionHandle::spawn_with_store(SessionState::new(session_id, group_id), Some(store))
}

impl Operations {
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn group_id(&self) -> SessionGroupId {
        self.group_id
    }
}
