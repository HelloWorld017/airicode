use super::SessionId;

#[derive(Clone, Debug)]
pub enum UIEvent {
    OpenSession { session_id: SessionId },
}
