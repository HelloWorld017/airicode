use super::{Operations, SessionHandle};
use crate::core::models::{SessionGroupId, SessionId, SessionState};

pub fn create_session(session_id: SessionId, group_id: SessionGroupId) -> SessionHandle {
    SessionHandle::spawn(SessionState::new(session_id, group_id))
}

pub fn new_session(session_id: SessionId, group_id: SessionGroupId) -> SessionHandle {
    create_session(session_id, group_id)
}

impl Operations {
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn group_id(&self) -> SessionGroupId {
        self.group_id
    }
}
