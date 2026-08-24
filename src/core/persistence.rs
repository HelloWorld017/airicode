use async_trait::async_trait;

use super::{
    error::Result,
    models::{SessionCommit, SessionId},
};

/// Durable session storage is intentionally expressed in terms of complete
/// commits. Implementations must append a commit before reporting success.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn load(&self, session_id: SessionId) -> Result<Vec<SessionCommit>>;
    async fn append(&self, session_id: SessionId, commit: &SessionCommit) -> Result<()>;
}
